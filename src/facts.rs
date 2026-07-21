//! The cuj fact namespaces (SPEC §5): wire-format bodies for the
//! `jot/*`, `cuj/*`, and dialect (`obsidian/*`) kinds, and the
//! effective-index merge.

use std::collections::BTreeSet;

use metatheca::{Fact, Uuid};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

pub const PROTOCOL: &str = "0.5";
/// The previous protocol line, still served read/write for its 0.4
/// surface (SPEC §5.13); 0.5-only fact kinds require migration.
pub const PROTOCOL_PREV: &str = "0.4";

pub const K_JOT_INDEX: &str = "jot/index";
pub const K_CUJ_JOT: &str = "cuj/jot";
pub const K_CUJ_ID: &str = "cuj/id";
pub const K_CUJ_APPLIED: &str = "cuj/applied";
pub const K_CUJ_COUNTER: &str = "cuj/counter";
pub const K_CUJ_ATTACHMENT: &str = "cuj/attachment";
pub const K_CUJ_EXTRACTION: &str = "cuj/extraction";
pub const K_CUJ_IMPORT: &str = "cuj/import";
pub const K_CUJ_PROFILE: &str = "cuj/profile";
pub const K_CUJ_SPACE: &str = "cuj/space";
pub const K_CUJ_TEMPLATE: &str = "cuj/template";
pub const K_CUJ_SAVED_QUERY: &str = "cuj/saved-query";
pub const K_CUJ_VAULT: &str = "cuj/vault";
/// Dialect-derived (SPEC §5): the Obsidian-metadata snapshot on
/// file entries in spaces carrying the `obsidian` dialect.
pub const K_OBSIDIAN_INDEX: &str = "obsidian/index";

/// The space dialects this protocol defines (SPEC §dialects).
pub const DIALECTS: &[&str] = &["obsidian"];

pub fn validate_dialect(name: &str) -> Result<()> {
    if DIALECTS.contains(&name) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "unknown dialect {name:?} (one of: {})",
            DIALECTS.join(", ")
        )))
    }
}

pub const ROOT_PATH: &str = "config";
pub const RESERVED_NAMES: &[&str] = &["all", "config", "index", "space", "tmp"];

pub fn profile_path(name: &str) -> String {
    format!("config/{name}")
}

pub fn space_registry_path(name: &str) -> String {
    format!("config/space/{name}")
}

/// The path prefix of a space's observed files (SPEC §file
/// entries).
pub fn space_prefix(name: &str) -> String {
    format!("space/{name}/")
}

pub fn file_jot_path(space: &str, rel: &str) -> String {
    format!("space/{space}/{rel}")
}

pub fn template_path(profile: &str, name: &str) -> String {
    format!("config/{profile}/template/{name}")
}

pub fn query_path(profile: &str, name: &str) -> String {
    format!("config/{profile}/query/{name}")
}

// -- body structs ----------------------------------------------------

/// Jot Syntax edge annotations (SPEC §5.1): the `::category` /
/// `..tag` suffixes that annotate a reference — the source→target
/// relationship itself, not the jot — plus the target specifier
/// when one was written. Absent fields are omitted on the wire, so
/// pre-annotation facts deserialize unchanged and unannotated
/// edges serialize exactly as before.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EdgeAnnot {
    /// The full lowercased category path (`cites::weak`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// A `#name` bookmark specifier on the reference, name only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
    /// A raw line-fragment specifier (`:N`, `:N-M`, `:N+L`), when
    /// written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
    /// A `##start,end` bookmark-range specifier; either side may
    /// be open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark_range: Option<BookmarkRangeAnnot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BookmarkRangeAnnot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
}

impl EdgeAnnot {
    pub fn is_empty(&self) -> bool {
        self.category.is_none()
            && self.tags.is_empty()
            && self.bookmark.is_none()
            && self.fragment.is_none()
            && self.bookmark_range.is_none()
    }
}

/// An entry-resolved reference edge (SPEC §3.4): `entry` is the
/// durable target, `as` the identity it was written against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EdgeBody {
    pub entry: String,
    #[serde(rename = "as")]
    pub written_as: String,
    #[serde(flatten)]
    pub annot: EdgeAnnot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct XFileRefBody {
    pub space: String,
    pub path: String,
    #[serde(flatten)]
    pub annot: EdgeAnnot,
}

/// `entry` is the owning jot's UUID, resolved at write time exactly
/// like reference edges (SPEC §3.4) — durable across renumbering;
/// absent when the locator was a filepath or did not resolve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InterJotResourceBody {
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    pub name: String,
    #[serde(flatten)]
    pub annot: EdgeAnnot,
}

/// Generate a body that wires as a **plain string when bare** (the
/// pre-annotation form, so annotation-free content is byte-identical
/// on the wire) and as an object only when annotated.
macro_rules! str_or_object_body {
    ($Body:ident, $key:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq)]
        pub struct $Body {
            pub $key: String,
            pub annot: EdgeAnnot,
        }

        impl $Body {
            pub fn bare($key: impl Into<String>) -> Self {
                $Body {
                    $key: $key.into(),
                    annot: EdgeAnnot::default(),
                }
            }
        }

        impl Serialize for $Body {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                #[derive(Serialize)]
                struct Obj<'a> {
                    $key: &'a str,
                    #[serde(flatten)]
                    annot: &'a EdgeAnnot,
                }
                if self.annot.is_empty() {
                    s.serialize_str(&self.$key)
                } else {
                    Obj {
                        $key: &self.$key,
                        annot: &self.annot,
                    }
                    .serialize(s)
                }
            }
        }

        impl<'de> Deserialize<'de> for $Body {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                #[derive(Deserialize)]
                struct Obj {
                    $key: String,
                    #[serde(flatten)]
                    annot: EdgeAnnot,
                }
                #[derive(Deserialize)]
                #[serde(untagged)]
                enum StrOr {
                    S(String),
                    B(Obj),
                }
                Ok(match StrOr::deserialize(d)? {
                    StrOr::S($key) => $Body {
                        $key,
                        annot: EdgeAnnot::default(),
                    },
                    StrOr::B(o) => $Body {
                        $key: o.$key,
                        annot: o.annot,
                    },
                })
            }
        }
    };
}

str_or_object_body!(
    FileRefBody,
    path,
    "A bare `//path` file reference (SPEC §5.1), with its edge \
     annotations when written."
);
str_or_object_body!(
    ResourceBody,
    name,
    "An own-jot `;;filename` resource (SPEC §5.1), with its edge \
     annotations when written."
);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UrlBody {
    pub url: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoBody {
    pub raw: String,
    pub action: Option<String>,
    pub description: String,
    pub importance: u8,
    pub due: Option<String>,
    pub done: bool,
    /// The `.@<iso-date>` completion timestamp on a done todo, when
    /// written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<String>,
}

/// `jot/index` body (SPEC §5.1): the parsed annotation snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParsedIndex {
    pub jots_version: String,
    pub tags: Vec<String>,
    pub categories: Vec<String>,
    pub references: Vec<EdgeBody>,
    pub unresolved_references: Vec<String>,
    pub file_references: Vec<FileRefBody>,
    pub cross_file_references: Vec<XFileRefBody>,
    pub resources: Vec<ResourceBody>,
    pub inter_jot_resources: Vec<InterJotResourceBody>,
    pub urls: Vec<UrlBody>,
    pub bookmarks: Vec<String>,
    pub todos: Vec<TodoBody>,
    pub diagnostics_count: u64,
}

impl ParsedIndex {
    /// Body equality ignoring the parser version: `reparse` re-emits
    /// on content divergence, not on a cuj upgrade (the same rule as
    /// [`ObsidianIndex::same_body`]).
    pub fn same_body(&self, other: &ParsedIndex) -> bool {
        self.tags == other.tags
            && self.categories == other.categories
            && self.references == other.references
            && self.unresolved_references == other.unresolved_references
            && self.file_references == other.file_references
            && self.cross_file_references == other.cross_file_references
            && self.resources == other.resources
            && self.inter_jot_resources == other.inter_jot_resources
            && self.urls == other.urls
            && self.bookmarks == other.bookmarks
            && self.todos == other.todos
            && self.diagnostics_count == other.diagnostics_count
    }
}

/// `cuj/applied` body (SPEC §5.4): the metadata overlay.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppliedOverlay {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub references: Vec<EdgeBody>,
    #[serde(default)]
    pub todos: Vec<TodoBody>,
}

/// `cuj/jot` body (SPEC §5.2): the immutable birth record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Birth {
    pub profile: String,
    pub id: i64,
    pub ext: String,
    pub created_at_ns: i64,
}

/// `cuj/id` body (SPEC §5.3): the current user-facing identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ident {
    pub profile: String,
    pub id: i64,
    pub ext: String,
}

/// `cuj/counter` body (SPEC §5.6): the profile's allocation
/// counter — the highest jot ID ever allocated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterBody {
    pub value: i64,
}

/// `jot` is the owning jot's entry UUID in RFC 4122 string form —
/// the fact wire format carries UUIDs as strings (SPEC §5.6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentBody {
    pub jot: String,
    pub filename: String,
    pub size: u64,
    pub added_at_ns: i64,
    pub detached: bool,
}

/// `of` is the attachment entry's UUID string (SPEC §5.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionBody {
    pub of: String,
    pub tool: String,
    pub tool_version: String,
}

/// `cuj/import` provenance: written once, in the birth commit, on
/// every jot created by `cuj import`. The fingerprint doubles as
/// the duplicate-detection key — re-importing a source note whose
/// fingerprint is already present is a skip, even if the jot has
/// since been deleted or moved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportBody {
    /// SHA-256 hex of `title \0 created-raw \0 raw-content`.
    pub fingerprint: String,
    /// Basename of the source export file (e.g. `notes.enex`).
    pub source: String,
    pub imported_at_ns: i64,
    /// The source note's last-modified time, when it carried one.
    pub updated_at_ns: Option<i64>,
}

/// `cuj/space` body (SPEC §space registry): the file-space
/// registry record. `dialect` is vault-resident deliberately —
/// dialect facts are vault contents, so every client must agree
/// on which spaces carry which dialect (SPEC §space registry).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpaceBody {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
}

/// A resolved dialect link or embed (SPEC §obsidian/index):
/// `space`/`path` deliberately mirror `XFileRefBody`, so backlink
/// queries take one shape; `as` preserves the target as written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DialectLink {
    pub space: String,
    pub path: String,
    #[serde(rename = "as")]
    pub written_as: String,
}

/// A Markdown checkbox item (SPEC §obsidian/index): dialect-scoped
/// in this protocol — it does not join the effective todo list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DialectTask {
    pub raw: String,
    pub done: bool,
}

/// `obsidian/index` body (SPEC §obsidian/index): the parsed
/// Obsidian-metadata snapshot of a file entry's current content.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ObsidianIndex {
    #[serde(default)]
    pub dialect_version: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub links: Vec<DialectLink>,
    #[serde(default)]
    pub unresolved_links: Vec<String>,
    #[serde(default)]
    pub embeds: Vec<DialectLink>,
    #[serde(default)]
    pub unresolved_embeds: Vec<String>,
    #[serde(default)]
    pub properties: Map<String, Value>,
    #[serde(default)]
    pub tasks: Vec<DialectTask>,
}

impl ObsidianIndex {
    /// The retraction idiom (SPEC §dialects): absent and empty are
    /// equivalent everywhere dialect facts are read.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
            && self.categories.is_empty()
            && self.aliases.is_empty()
            && self.links.is_empty()
            && self.unresolved_links.is_empty()
            && self.embeds.is_empty()
            && self.unresolved_embeds.is_empty()
            && self.properties.is_empty()
            && self.tasks.is_empty()
    }

    /// Body equality ignoring the parser version: reconciliation
    /// re-emits on content divergence, not on a cuj upgrade.
    pub fn same_body(&self, other: &ObsidianIndex) -> bool {
        self.tags == other.tags
            && self.categories == other.categories
            && self.aliases == other.aliases
            && self.links == other.links
            && self.unresolved_links == other.unresolved_links
            && self.embeds == other.embeds
            && self.unresolved_embeds == other.unresolved_embeds
            && self.properties == other.properties
            && self.tasks == other.tasks
    }
}

/// `cuj/template` body (SPEC §cuj/template): template metadata;
/// the template text is the entry's content blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateBody {
    pub profile: String,
    pub name: String,
    #[serde(default = "default_ext")]
    pub ext: String,
}

/// `cuj/saved-query` body (SPEC §cuj/saved-query): a named find
/// argument vector, stored as tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedQueryBody {
    pub profile: String,
    pub name: String,
    pub args: Vec<String>,
}

/// `cuj/profile` body (SPEC §5.12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub name: String,
    #[serde(default = "default_ext")]
    pub default_ext: String,
    #[serde(default)]
    pub exclusive: Vec<String>,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub jots_frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_space: Option<String>,
    #[serde(default)]
    pub parse: serde_json::Map<String, Value>,
}

fn default_ext() -> String {
    "md".to_string()
}

impl ProfileConfig {
    pub fn new(name: &str) -> Self {
        ProfileConfig {
            name: name.to_string(),
            default_ext: default_ext(),
            exclusive: Vec::new(),
            required: Vec::new(),
            jots_frozen: false,
            default_space: None,
            parse: Map::new(),
        }
    }
}

pub const EXTS: &[&str] = &["md", "txt", "tex", "html"];

// -- fact construction / parsing --------------------------------------

pub fn to_fact<T: Serialize>(kind: &str, entry: Uuid, body: &T) -> Result<Fact> {
    let v = serde_json::to_value(body).map_err(|e| Error::Substrate(format!("fact body: {e}")))?;
    let Value::Object(map) = v else {
        return Err(Error::Substrate("fact body is not an object".into()));
    };
    Fact::new(kind, entry, map).map_err(Into::into)
}

pub fn from_body<T: for<'de> Deserialize<'de>>(body: &Map<String, Value>) -> Option<T> {
    serde_json::from_value(Value::Object(body.clone())).ok()
}

pub fn vault_fact(entry: Uuid) -> Result<Fact> {
    let mut m = Map::new();
    m.insert("protocol".into(), Value::String(PROTOCOL.into()));
    Fact::new(K_CUJ_VAULT, entry, m).map_err(Into::into)
}

// -- profile / extension validation ------------------------------------

pub fn validate_profile_name(name: &str) -> Result<()> {
    validate_short_name(name, "profile name")?;
    if RESERVED_NAMES.contains(&name) {
        return Err(Error::Usage(format!("profile name {name:?} is reserved")));
    }
    Ok(())
}

/// The shared name grammar (SPEC §3.3): spaces, templates, and
/// saved queries reuse it, without the profile reserved-word list.
pub fn validate_short_name(name: &str, what: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !ok {
        return Err(Error::Usage(format!(
            "invalid {what} {name:?} (must match [a-z][a-z0-9]*, 1..32 chars)"
        )));
    }
    Ok(())
}

/// The tag/category token alphabet of the Jot Syntax grammar:
/// `[a-z0-9_]+` per segment (values are canonicalized lowercase
/// before validation).
pub fn validate_tag(tag: &str) -> Result<()> {
    let ok = !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(Error::Usage(format!(
            "invalid tag {tag:?} (must match [a-z0-9_]+)"
        )));
    }
    Ok(())
}

pub fn validate_category(cat: &str) -> Result<()> {
    let ok = !cat.is_empty() && cat.split("::").all(|seg| validate_tag(seg).is_ok());
    if !ok {
        return Err(Error::Usage(format!(
            "invalid category {cat:?} (segments must match [a-z0-9_]+, joined by ::)"
        )));
    }
    Ok(())
}

pub fn validate_ext(ext: &str) -> Result<()> {
    if EXTS.contains(&ext) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "unsupported extension {ext:?} (one of: {})",
            EXTS.join(", ")
        )))
    }
}

// -- effective index (SPEC §5.5) ---------------------------------------

/// One effective edge to a target entry: the written form for
/// display plus the edge annotations.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeInfo {
    pub written_as: String,
    pub annot: EdgeAnnot,
}

#[derive(Debug, Clone, Default)]
pub struct EffectiveIndex {
    pub tags: BTreeSet<String>,
    pub categories: BTreeSet<String>,
    /// Entry-resolved edges, unioned by (entry, category)
    /// (SPEC §5.5): `--42::cites` and `--42::uses` survive as
    /// distinct typed edges; two bare references to one target
    /// still union to a single edge.
    pub edges: std::collections::BTreeMap<String, Vec<EdgeInfo>>,
    pub unresolved_references: Vec<String>,
    pub todos: Vec<TodoBody>,
    pub file_references: Vec<FileRefBody>,
    pub cross_file_references: Vec<XFileRefBody>,
    pub resources: Vec<ResourceBody>,
    pub inter_jot_resources: Vec<InterJotResourceBody>,
    pub urls: Vec<UrlBody>,
    pub bookmarks: Vec<String>,
}

/// Merge parsed index and overlay per SPEC §5.5.
pub fn effective_index(
    parsed: Option<&ParsedIndex>,
    applied: Option<&AppliedOverlay>,
) -> EffectiveIndex {
    let mut e = EffectiveIndex::default();
    if let Some(p) = parsed {
        e.tags.extend(p.tags.iter().cloned());
        e.categories.extend(p.categories.iter().cloned());
        for r in &p.references {
            let infos = e.edges.entry(r.entry.clone()).or_default();
            if !infos.iter().any(|x| x.annot.category == r.annot.category) {
                infos.push(EdgeInfo {
                    written_as: r.written_as.clone(),
                    annot: r.annot.clone(),
                });
            }
        }
        e.unresolved_references = p.unresolved_references.clone();
        e.todos.extend(p.todos.iter().cloned());
        e.file_references = p.file_references.clone();
        e.cross_file_references = p.cross_file_references.clone();
        e.resources = p.resources.clone();
        e.inter_jot_resources = p.inter_jot_resources.clone();
        e.urls = p.urls.clone();
        e.bookmarks = p.bookmarks.clone();
    }
    if let Some(a) = applied {
        e.tags.extend(a.tags.iter().cloned());
        e.categories.extend(a.categories.iter().cloned());
        // An overlay edge replaces the parsed edge with the same
        // (entry, category) key, else joins the union.
        for r in &a.references {
            let info = EdgeInfo {
                written_as: r.written_as.clone(),
                annot: r.annot.clone(),
            };
            let infos = e.edges.entry(r.entry.clone()).or_default();
            match infos
                .iter_mut()
                .find(|x| x.annot.category == r.annot.category)
            {
                Some(x) => *x = info,
                None => infos.push(info),
            }
        }
        // Overlay todos: an overlay todo whose raw equals a parsed
        // todo's raw overrides it (SPEC §5.5).
        for t in &a.todos {
            if let Some(existing) = e.todos.iter_mut().find(|x| x.raw == t.raw) {
                *existing = t.clone();
            } else {
                e.todos.push(t.clone());
            }
        }
    }
    e
}

/// Extend a file-jot's effective index with its dialect facts
/// (SPEC §file retrieval): tags and categories union; resolved
/// links and embeds count as cross-file references. Tasks,
/// aliases, and properties stay dialect-scoped.
pub fn union_dialect(e: &mut EffectiveIndex, o: &ObsidianIndex) {
    e.tags.extend(o.tags.iter().cloned());
    e.categories.extend(o.categories.iter().cloned());
    for l in o.links.iter().chain(o.embeds.iter()) {
        let x = XFileRefBody {
            space: l.space.clone(),
            path: l.path.clone(),
            annot: EdgeAnnot::default(),
        };
        if !e.cross_file_references.contains(&x) {
            e.cross_file_references.push(x);
        }
    }
}

// -- constraints (SPEC §6.9) -------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ConstraintPattern {
    Tag(String),
    Category(String),
    Todo(String),
}

impl ConstraintPattern {
    pub fn parse(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix("..") {
            Ok(ConstraintPattern::Tag(rest.to_lowercase()))
        } else if let Some(rest) = s.strip_prefix("::") {
            Ok(ConstraintPattern::Category(rest.to_lowercase()))
        } else if let Some(rest) = s.strip_prefix("??") {
            Ok(ConstraintPattern::Todo(rest.to_lowercase()))
        } else {
            Err(Error::Usage(format!(
                "invalid constraint pattern {s:?} (must start with .., ::, or ??)"
            )))
        }
    }

    /// Count matching constructs in an effective index. Payloads
    /// name atoms, never string prefixes (SPEC §6.9): a tag or
    /// todo action matches exactly; a category payload names a
    /// hierarchical level, selecting it and everything beneath it.
    /// An empty payload selects every construct of the kind.
    pub fn count(&self, e: &EffectiveIndex) -> usize {
        match self {
            ConstraintPattern::Tag(name) => {
                e.tags
                    .iter()
                    .filter(|t| name.is_empty() || t.as_str() == name)
                    .count()
            }
            ConstraintPattern::Category(level) => {
                let level = level.trim_end_matches(':');
                e.categories
                    .iter()
                    .filter(|c| {
                        level.is_empty()
                            || c.as_str() == level
                            || c.starts_with(&format!("{level}::"))
                    })
                    .count()
            }
            ConstraintPattern::Todo(action) => e
                .todos
                .iter()
                .filter(|t| {
                    action.is_empty()
                        || t.action.as_deref().unwrap_or("").to_lowercase() == *action
                })
                .count(),
        }
    }
}

/// Run a profile's exclusive/required checks against an effective
/// index (SPEC §6.9). `force` bypasses required only.
pub fn check_constraints(profile: &ProfileConfig, e: &EffectiveIndex, force: bool) -> Result<()> {
    for pat in &profile.exclusive {
        let p = ConstraintPattern::parse(pat)?;
        if p.count(e) > 1 {
            return Err(Error::ConstraintViolation(format!(
                "exclusive constraint {pat:?} matched more than one construct"
            )));
        }
    }
    if !force {
        for pat in &profile.required {
            let p = ConstraintPattern::parse(pat)?;
            if p.count(e) == 0 {
                return Err(Error::ConstraintViolation(format!(
                    "required constraint {pat:?} matched nothing (use --force to bypass)"
                )));
            }
        }
    }
    Ok(())
}

pub fn now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos() as i64
}

pub fn client_id() -> String {
    format!("cuj-rs {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraint_patterns_select_atoms_not_string_prefixes() {
        let mut e = EffectiveIndex::default();
        e.tags.extend(["urg".to_string(), "urgent".to_string()]);
        e.categories.extend([
            "email".to_string(),
            "email::inbox".to_string(),
            "emailfoo".to_string(),
        ]);
        e.todos.push(TodoBody {
            raw: "??pay rent".into(),
            action: Some("pay".into()),
            description: "rent".into(),
            importance: 1,
            due: None,
            done: false,
            completed: None,
        });
        // Tags are atoms: ..urg selects urg alone, never urgent.
        assert_eq!(ConstraintPattern::parse("..urg").unwrap().count(&e), 1);
        assert_eq!(ConstraintPattern::parse("..").unwrap().count(&e), 2);
        // A category payload names a hierarchical level: itself and
        // everything beneath it, never the string-prefix sibling.
        assert_eq!(ConstraintPattern::parse("::email").unwrap().count(&e), 2);
        assert_eq!(ConstraintPattern::parse("::email::").unwrap().count(&e), 2);
        assert_eq!(ConstraintPattern::parse("::").unwrap().count(&e), 3);
        // Todo actions are atoms too.
        assert_eq!(ConstraintPattern::parse("??pay").unwrap().count(&e), 1);
        assert_eq!(ConstraintPattern::parse("??p").unwrap().count(&e), 0);
    }

    #[test]
    fn old_shape_parsed_index_deserializes() {
        // A pre-annotation jot/index body (protocol 0.5 before the
        // edge-annotation extension): plain-string file refs and
        // resources, bare reference objects.
        let old = serde_json::json!({
            "jots_version": "jots 0.9.0",
            "tags": ["urgent"],
            "categories": ["work::alpha"],
            "references": [{"entry": "e-1", "as": "default:1"}],
            "unresolved_references": ["7"],
            "file_references": ["notes.md"],
            "cross_file_references": [{"space": "wiki", "path": "a.md"}],
            "resources": ["img.png"],
            "inter_jot_resources": [{"id": "4", "name": "p.pdf"}],
            "urls": [],
            "bookmarks": ["intro"],
            "todos": [{"raw": "??x", "action": "x", "description": "",
                       "importance": 1, "due": null, "done": false}],
            "diagnostics_count": 0
        });
        let Value::Object(map) = old else { unreachable!() };
        let p: ParsedIndex = from_body(&map).expect("old shape reads");
        assert!(p.references[0].annot.is_empty());
        assert!(p.cross_file_references[0].annot.is_empty());
        assert_eq!(p.file_references[0].path, "notes.md");
        assert_eq!(p.resources[0].name, "img.png");
        assert!(p.inter_jot_resources[0].entry.is_none());
        assert!(p.inter_jot_resources[0].space.is_none());
        assert!(p.todos[0].completed.is_none());
    }

    #[test]
    fn bare_bodies_wire_as_plain_strings() {
        // Annotation-free content keeps the pre-annotation wire,
        // byte for byte.
        let f = FileRefBody::bare("notes.md");
        assert_eq!(serde_json::to_value(&f).unwrap(), serde_json::json!("notes.md"));
        let r = ResourceBody::bare("img.png");
        assert_eq!(serde_json::to_value(&r).unwrap(), serde_json::json!("img.png"));
        let e = EdgeBody {
            entry: "e-1".into(),
            written_as: "default:1".into(),
            annot: EdgeAnnot::default(),
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({"entry": "e-1", "as": "default:1"})
        );
    }

    #[test]
    fn annotated_bodies_roundtrip_as_objects() {
        let f = FileRefBody {
            path: "notes.md".into(),
            annot: EdgeAnnot {
                category: Some("about".into()),
                tags: vec!["draft".into()],
                ..Default::default()
            },
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v.pointer("/path").unwrap(), "notes.md");
        assert_eq!(v.pointer("/category").unwrap(), "about");
        let back: FileRefBody = serde_json::from_value(v).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn edges_union_by_entry_and_category() {
        let edge = |cat: Option<&str>| EdgeBody {
            entry: "e-1".into(),
            written_as: "default:1".into(),
            annot: EdgeAnnot {
                category: cat.map(String::from),
                ..Default::default()
            },
        };
        let parsed = ParsedIndex {
            references: vec![
                edge(Some("cites")),
                edge(Some("uses")),
                edge(None),
                edge(None), // bare duplicate unions away
            ],
            ..Default::default()
        };
        let e = effective_index(Some(&parsed), None);
        assert_eq!(e.edges["e-1"].len(), 3);
        // An overlay edge replaces its (entry, category) slot.
        let overlay = AppliedOverlay {
            references: vec![EdgeBody {
                entry: "e-1".into(),
                written_as: "default:9".into(),
                annot: EdgeAnnot {
                    category: Some("cites".into()),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let e = effective_index(Some(&parsed), Some(&overlay));
        assert_eq!(e.edges["e-1"].len(), 3);
        let cites = e.edges["e-1"]
            .iter()
            .find(|x| x.annot.category.as_deref() == Some("cites"))
            .unwrap();
        assert_eq!(cites.written_as, "default:9");
    }
}
