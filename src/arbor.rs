//! Quarb binding: the vault as an arbor (SPEC appendix).
//!
//! A cuj vault is a tree-spanned graph — profiles/jots/attachments
//! and space/file subtrees as the hierarchy, entry-resolved
//! reference edges as the crosslinks — which is exactly Quarb's
//! data model. This module maps a loaded fact view onto
//! `quarb::AstAdapter`, so `cuj q '<expr>'` runs the Quarb language
//! over the vault (and, through `--at`, over any historical state).
//!
//! The vocabulary (what names, traits, properties, metadata, and
//! links a node carries) is the cuj-specific surface; the syntax is
//! Quarb's, unchanged:
//!
//! - tree: `^` → profiles → jots (named by decimal ID) → todo
//!   items (named by action word, 1-based index when wordless),
//!   bookmarks (named by anchor name), and attachments (named by
//!   filename); `space` → space name → path segments → file-jots
//!   (with the same todo/bookmark children); `cats` → the category
//!   hierarchy, one node per `::`-separated level; `tags` → one
//!   node per tag;
//! - traits: `<jot>`, `<file>`, `<attachment>`, `<profile>`,
//!   `<dir>`, `<space>`, `<todo>` (a jot with an open todo, and
//!   every todo item node), `<open>`/`<done>` on todo items,
//!   `<bookmark>`, `<cat>`, `<tag>`, `<frozen>`, `<extracted>`,
//!   `<orphan>` (no incoming reference edges), plus every tag as
//!   a trait (`<urgent>`);
//! - properties: `::tags`, `::categories`, `::todos`, `::urls`,
//!   `::bookmarks`, `::summary`; todo items carry `::action`,
//!   `::description`, `::importance`, `::raw`, and `::due` /
//!   `::completed` as typed instants; bare `::` is the content
//!   (a todo's raw form; a bookmark's fragment text, the same
//!   slice `cuj show --N#name` prints);
//! - metadata (`:::`): jots — `jot-id`, `profile`, `ext`, `created`,
//!   `modified` (typed instants), `attachments`, `n-links`,
//!   `n-backlinks`; files — `space`, `rel-path`, `ext`, `modified`,
//!   `n-links`, `n-backlinks`; attachments — `size` (a byte
//!   quantity), `added`, `extracted`; category and tag nodes —
//!   `n-jots`;
//! - crosslinks: a reference annotated `::cat…` walks as an edge
//!   labeled by its category's first segment (`--42::cites` →
//!   `->cites`), an unannotated reference as `->ref`; file
//!   references likewise, defaulting to `->file`; inter-jot
//!   resources (`--42;;paper.pdf`) link to the target attachment,
//!   defaulting to `->resource`; jots link `->cat` to their
//!   category nodes and tag nodes link `->tagged` to their jots.
//!   Edge annotations read through `$-`: `$-::category` (the full
//!   path), `$-::tags`, `$-::as` (the written form),
//!   `$-::bookmark`, `$-::fragment`, `$-::bookmark-range`.
//!
//! Parallel edges all traverse and all count in `n-links` /
//! `n-backlinks`; what is single-valued is the per-(source, edge
//! label, target) property slot: where parallel edges share all
//! three, `$-` reads answer for the first in parse order,
//! consistently — quarb's adapter contract, the same documented
//! limit as quarb-neo4j. Distinct targets or labels never
//! collapse. Also: a profile named `cats`/`tags` shadows the
//! taxonomy root's *name* (SPEC appendix); the shadowed root
//! still displays as `cats`/`tags` in locators, colliding with
//! the profile's locator string — display is informational, not
//! resolution.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use metatheca::Uuid;
use quarb::{AstAdapter, NodeId, Value};

use crate::app::Suffix;
use crate::facts::{EdgeAnnot, EffectiveIndex};
use crate::queries::{extract_fragment, summary_line};
use crate::reads::Reads;

enum Node {
    Root,
    Profile(String),
    Jot(Uuid),
    /// The `space` grouping node under the root.
    SpaceRoot,
    Space(String),
    /// An intermediate path segment inside a space.
    Dir(String),
    File(Uuid),
    Attachment {
        entry: Uuid,
        filename: String,
    },
    /// The `cats` grouping node under the root.
    CatRoot,
    /// One level of the category hierarchy; the full lowercased
    /// path (`work::acme`).
    Cat(String),
    /// The `tags` grouping node under the root.
    TagRoot,
    Tag(String),
    /// A todo item of a jot or file; `index` into the owner's
    /// effective todos.
    Todo {
        owner: Uuid,
        index: usize,
    },
    /// A `!!name` bookmark of a jot or file; `index` into the
    /// owner's effective bookmarks.
    Bookmark {
        owner: Uuid,
        index: usize,
    },
}

/// The vault as a Quarb arbor, borrowing a loaded fact view.
pub struct VaultArbor<'a> {
    reads: &'a Reads<'a>,
    nodes: Vec<Node>,
    children: Vec<Vec<NodeId>>,
    parents: Vec<Option<NodeId>>,
    by_entry: HashMap<Uuid, usize>,
    /// The effective index per jot/file entry, computed once — the
    /// arbor reads it on every trait, property, and edge answer.
    eff: HashMap<Uuid, EffectiveIndex>,
    /// Category path → its node, tag → its node.
    cat_nodes: HashMap<String, usize>,
    tag_nodes: BTreeMap<String, usize>,
    /// Outgoing crosslinks, precomputed at build.
    outlinks: HashMap<usize, Vec<(String, NodeId)>>,
    /// Incoming crosslinks, precomputed (cuj already holds the
    /// reference graph; `<-` must not be expensive here).
    inlinks: HashMap<usize, Vec<(String, NodeId)>>,
    /// `(source, label, target)` → the edge's written form and
    /// annotations, for `$-::prop` reads.
    edge_props: HashMap<(usize, String, usize), (Option<String>, EdgeAnnot)>,
    /// A profile named `cats` / `tags` shadows the taxonomy
    /// subtree's NAME (SPEC Appendix B; the names are not
    /// reserved): the subtree stays structurally reachable, but
    /// name resolution answers the profile alone.
    shadow_cats: bool,
    shadow_tags: bool,
}

impl<'a> VaultArbor<'a> {
    pub fn build(reads: &'a Reads<'a>) -> VaultArbor<'a> {
        let mut a = VaultArbor {
            reads,
            nodes: Vec::new(),
            children: Vec::new(),
            parents: Vec::new(),
            by_entry: HashMap::new(),
            eff: HashMap::new(),
            cat_nodes: HashMap::new(),
            tag_nodes: BTreeMap::new(),
            outlinks: HashMap::new(),
            inlinks: HashMap::new(),
            edge_props: HashMap::new(),
            shadow_cats: reads.profiles.contains_key("cats"),
            shadow_tags: reads.profiles.contains_key("tags"),
        };
        let root = a.intern(Node::Root, None);

        for name in reads.profiles.keys() {
            let p = a.intern(Node::Profile(name.clone()), Some(root));
            for (entry, _) in reads.list(Some(name)) {
                let j = a.intern(Node::Jot(entry), Some(p));
                a.by_entry.insert(entry, j);
                a.eff.insert(entry, reads.effective(entry));
                for (att_entry, att) in reads.attachments_of(entry) {
                    let n = a.intern(
                        Node::Attachment {
                            entry: att_entry,
                            filename: att.filename.clone(),
                        },
                        Some(j),
                    );
                    a.by_entry.insert(att_entry, n);
                }
                a.intern_items(entry, j);
            }
        }

        if !reads.spaces.is_empty() || !reads.files.is_empty() {
            let sroot = a.intern(Node::SpaceRoot, Some(root));
            let mut space_nodes: HashMap<&str, usize> = HashMap::new();
            for name in reads.spaces.keys() {
                space_nodes.insert(name, a.intern(Node::Space(name.clone()), Some(sroot)));
            }
            // reads.files is sorted by (space, path), so a dir chain
            // is always interned before anything beneath it.
            let mut dirs: HashMap<(usize, String), usize> = HashMap::new();
            for ((space, rel), entry) in &reads.files {
                // Files under an unregistered space (fsck flags
                // this) still get a node, so queries see them.
                let mut at = *space_nodes
                    .entry(space.as_str())
                    .or_insert_with(|| a.intern(Node::Space(space.clone()), Some(sroot)));
                let mut segments: Vec<&str> = rel.split('/').collect();
                let leaf = segments.pop().unwrap_or_default();
                let mut prefix = String::new();
                for seg in segments {
                    prefix.push_str(seg);
                    prefix.push('/');
                    at = *dirs
                        .entry((at, prefix.clone()))
                        .or_insert_with(|| a.intern(Node::Dir(seg.to_string()), Some(at)));
                }
                let _ = leaf;
                let f = a.intern(Node::File(*entry), Some(at));
                a.by_entry.insert(*entry, f);
                a.eff.insert(*entry, reads.effective(*entry));
                a.intern_items(*entry, f);
            }
        }

        // The category hierarchy and the tag list, from the union of
        // every entry's effective classification.
        let mut cats: BTreeSet<String> = BTreeSet::new();
        let mut tags: BTreeSet<String> = BTreeSet::new();
        for eff in a.eff.values() {
            cats.extend(eff.categories.iter().cloned());
            tags.extend(eff.tags.iter().cloned());
        }
        if !cats.is_empty() {
            let croot = a.intern(Node::CatRoot, Some(root));
            for cat in &cats {
                let mut path = String::new();
                let mut at = croot;
                for seg in cat.split("::") {
                    if !path.is_empty() {
                        path.push_str("::");
                    }
                    path.push_str(seg);
                    at = match a.cat_nodes.get(&path) {
                        Some(n) => *n,
                        None => {
                            let n = a.intern(Node::Cat(path.clone()), Some(at));
                            a.cat_nodes.insert(path.clone(), n);
                            n
                        }
                    };
                }
            }
        }
        if !tags.is_empty() {
            let troot = a.intern(Node::TagRoot, Some(root));
            for tag in &tags {
                let n = a.intern(Node::Tag(tag.clone()), Some(troot));
                a.tag_nodes.insert(tag.clone(), n);
            }
        }

        // Precompute the crosslink fabric: outgoing sets (with edge
        // annotations) and their inversion.
        for i in 0..a.nodes.len() {
            let links = a.compute_links(i);
            let mut out = Vec::new();
            for (label, target, written, annot) in links {
                if !annot.is_empty() || written.is_some() {
                    a.edge_props
                        .entry((i, label.clone(), target.0 as usize))
                        .or_insert((written, annot));
                }
                out.push((label, target));
            }
            for (label, target) in &out {
                a.inlinks
                    .entry(target.0 as usize)
                    .or_default()
                    .push((label.clone(), NodeId(i as u64)));
            }
            a.outlinks.insert(i, out);
        }
        a
    }

    fn intern(&mut self, node: Node, parent: Option<usize>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(node);
        self.children.push(Vec::new());
        self.parents.push(parent.map(|p| NodeId(p as u64)));
        if let Some(p) = parent {
            self.children[p].push(NodeId(id as u64));
        }
        id
    }

    /// Intern an entry's todo and bookmark children (jots and
    /// file-jots alike).
    fn intern_items(&mut self, entry: Uuid, under: usize) {
        let (n_todos, n_bookmarks) = {
            let eff = &self.eff[&entry];
            (eff.todos.len(), eff.bookmarks.len())
        };
        for index in 0..n_todos {
            self.intern(
                Node::Todo {
                    owner: entry,
                    index,
                },
                Some(under),
            );
        }
        for index in 0..n_bookmarks {
            self.intern(
                Node::Bookmark {
                    owner: entry,
                    index,
                },
                Some(under),
            );
        }
    }

    /// The display reference of a node, in cuj's own notation.
    /// Node ids arrive off the wire (quarb-serve) — an out-of-range
    /// id answers empty, never panics.
    pub fn display(&self, node: NodeId) -> String {
        let Some(n) = self.nodes.get(node.0 as usize) else {
            return String::new();
        };
        match n {
            Node::Root => "^".into(),
            Node::SpaceRoot => "space".into(),
            Node::CatRoot => "cats".into(),
            Node::TagRoot => "tags".into(),
            Node::Profile(name) | Node::Space(name) => name.clone(),
            Node::Cat(path) => format!("::{path}"),
            Node::Tag(tag) => format!("..{tag}"),
            Node::Jot(entry) => {
                let i = &self.reads.idents[entry];
                format!("--{}--{}", i.profile, i.id)
            }
            Node::File(entry) => match self.reads.file_paths.get(entry) {
                Some((space, rel)) => format!("///{space}/{rel}"),
                None => entry.hyphenated().to_string(),
            },
            Node::Dir(seg) => seg.clone(),
            Node::Attachment { entry, filename } => {
                let jot = self.reads.owning_jot(*entry);
                match jot.and_then(|j| self.reads.idents.get(&j)) {
                    Some(i) => format!("--{}--{};;{filename}", i.profile, i.id),
                    None => filename.clone(),
                }
            }
            Node::Todo { index, .. } => {
                let parent = self.parents[node.0 as usize].expect("todo has an owner");
                format!("{}??{}", self.display(parent), index + 1)
            }
            Node::Bookmark { owner, index } => {
                let parent = self.parents[node.0 as usize].expect("bookmark has an owner");
                let name = self.eff[owner]
                    .bookmarks
                    .get(*index)
                    .cloned()
                    .unwrap_or_default();
                format!("{}#{name}", self.display(parent))
            }
        }
    }

    /// The effective-index entry behind a jot or file node.
    fn indexed_entry(&self, node: usize) -> Option<Uuid> {
        match &self.nodes[node] {
            Node::Jot(e) | Node::File(e) => Some(*e),
            _ => None,
        }
    }

    /// The edge label of an annotation: the category's first
    /// segment (`::cites::weak` → `cites`; the full path stays
    /// readable as `$-::category`), or the kind's default label.
    fn edge_label(annot: &EdgeAnnot, default: &str) -> String {
        annot
            .category
            .as_deref()
            .and_then(|c| c.split("::").next())
            .filter(|s| !s.is_empty())
            .unwrap_or(default)
            .to_string()
    }

    /// Outgoing crosslinks with their edge data: reference edges,
    /// resolvable file references, inter-jot resource edges, and
    /// the taxonomy edges (SPEC §3.4 and the Quarb appendix).
    #[allow(clippy::type_complexity)]
    fn compute_links(
        &self,
        node: usize,
    ) -> Vec<(String, NodeId, Option<String>, EdgeAnnot)> {
        if let Node::Tag(tag) = &self.nodes[node] {
            // Tag nodes link to every entry carrying the tag, in
            // tree order.
            let mut out = Vec::new();
            for (i, n) in self.nodes.iter().enumerate() {
                if let Node::Jot(e) | Node::File(e) = n
                    && self.eff[e].tags.contains(tag)
                {
                    out.push((
                        "tagged".to_string(),
                        NodeId(i as u64),
                        None,
                        EdgeAnnot::default(),
                    ));
                }
            }
            return out;
        }
        let Some(entry) = self.indexed_entry(node) else {
            return Vec::new();
        };
        let eff = &self.eff[&entry];
        let mut out = Vec::new();
        for (target, infos) in &eff.edges {
            let Some(n) = Uuid::parse_str(target)
                .ok()
                .and_then(|u| self.by_entry.get(&u))
            else {
                continue;
            };
            for info in infos {
                out.push((
                    Self::edge_label(&info.annot, "ref"),
                    NodeId(*n as u64),
                    Some(info.written_as.clone()),
                    info.annot.clone(),
                ));
            }
        }
        for x in &eff.cross_file_references {
            if let Some(n) = self
                .reads
                .files
                .get(&(x.space.clone(), x.path.clone()))
                .and_then(|e| self.by_entry.get(e))
            {
                out.push((
                    Self::edge_label(&x.annot, "file"),
                    NodeId(*n as u64),
                    None,
                    x.annot.clone(),
                ));
            }
        }
        // Bare //path references resolve in the owning profile's
        // default space (SPEC §file references).
        if !eff.file_references.is_empty() {
            let space = match &self.nodes[node] {
                Node::Jot(e) => self
                    .reads
                    .idents
                    .get(e)
                    .and_then(|i| self.reads.profile(&i.profile).ok())
                    .and_then(|p| p.default_space.clone()),
                _ => None,
            };
            if let Some(space) = space {
                for r in &eff.file_references {
                    if let Some(n) = self
                        .reads
                        .files
                        .get(&(space.clone(), r.path.clone()))
                        .and_then(|e| self.by_entry.get(e))
                    {
                        out.push((
                            Self::edge_label(&r.annot, "file"),
                            NodeId(*n as u64),
                            None,
                            r.annot.clone(),
                        ));
                    }
                }
            }
        }
        // Inter-jot resources: an edge to the target attachment
        // node, when the owner resolved at write time.
        for r in &eff.inter_jot_resources {
            let Some(owner) = r
                .entry
                .as_deref()
                .and_then(|e| Uuid::parse_str(e).ok())
                .and_then(|u| self.by_entry.get(&u))
            else {
                continue;
            };
            let attachment = self.children[*owner].iter().find(|c| {
                matches!(
                    &self.nodes[c.0 as usize],
                    Node::Attachment { filename, .. } if *filename == r.name
                )
            });
            if let Some(att) = attachment {
                out.push((
                    Self::edge_label(&r.annot, "resource"),
                    *att,
                    None,
                    r.annot.clone(),
                ));
            }
        }
        // Taxonomy: one `cat` edge per effective category, to the
        // category's leaf node.
        for cat in &eff.categories {
            if let Some(n) = self.cat_nodes.get(cat) {
                out.push((
                    "cat".to_string(),
                    NodeId(*n as u64),
                    None,
                    EdgeAnnot::default(),
                ));
            }
        }
        out
    }

    /// Incoming reference-kind edges: crosslinks whose source is a
    /// jot or file (taxonomy `tagged` edges originate from tag
    /// nodes and are excluded by construction).
    fn n_ref_backlinks(&self, node: usize) -> usize {
        self.inlinks
            .get(&node)
            .map(|v| {
                v.iter()
                    .filter(|(_, src)| self.indexed_entry(src.0 as usize).is_some())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Outgoing reference-kind edges: crosslinks into the data
    /// tree (jots, files, attachments) — the taxonomy `cat` edges
    /// point at category nodes and do not count.
    fn n_ref_links(&self, node: usize) -> usize {
        self.outlinks
            .get(&node)
            .map(|v| {
                v.iter()
                    .filter(|(_, tgt)| {
                        matches!(
                            &self.nodes[tgt.0 as usize],
                            Node::Jot(_) | Node::File(_) | Node::Attachment { .. }
                        )
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn str_list(items: impl IntoIterator<Item = String>) -> Value {
        Value::list(items.into_iter().map(Value::Str).collect())
    }

    /// An epoch-nanosecond stamp as a typed instant (UTC timeline;
    /// the source carries no authored offset).
    fn instant_ns(ns: i64) -> Value {
        Value::Instant {
            secs: ns.div_euclid(1_000_000_000),
            nanos: ns.rem_euclid(1_000_000_000) as u32,
            offset_min: None,
            calendar: Default::default(),
        }
    }

    /// An ISO-8601 specifier (todo due / completion) as a typed
    /// instant; unparseable text rides as written.
    fn instant_iso(s: &str) -> Value {
        match quarb::temporal::parse_iso(s) {
            Some((secs, nanos, offset_min)) => Value::Instant {
                secs,
                nanos,
                offset_min,
                calendar: Default::default(),
            },
            None => Value::Str(s.to_string()),
        }
    }
}

impl AstAdapter for VaultArbor<'_> {
    fn root(&self) -> NodeId {
        // Node ids are dense arena indices from 0 — far inside the
        // 2^56-1 bound quarb-mount gives an inner adapter.
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.children
            .get(node.0 as usize)
            .cloned()
            .unwrap_or_default()
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match self.nodes.get(node.0 as usize)? {
            Node::Root => None,
            Node::SpaceRoot => Some("space".into()),
            Node::CatRoot if self.shadow_cats => None,
            Node::TagRoot if self.shadow_tags => None,
            Node::CatRoot => Some("cats".into()),
            Node::TagRoot => Some("tags".into()),
            Node::Profile(name) | Node::Space(name) => Some(name.clone()),
            Node::Cat(path) => path.rsplit("::").next().map(String::from),
            Node::Tag(tag) => Some(tag.clone()),
            Node::Jot(entry) => self.reads.idents.get(entry).map(|i| i.id.to_string()),
            Node::File(entry) => self
                .reads
                .file_paths
                .get(entry)
                .and_then(|(_, rel)| rel.rsplit('/').next())
                .map(String::from),
            Node::Dir(seg) => Some(seg.clone()),
            Node::Attachment { filename, .. } => Some(filename.clone()),
            Node::Todo { owner, index } => {
                let todo = self.eff[owner].todos.get(*index)?;
                match &todo.action {
                    Some(a) => Some(a.clone()),
                    None => Some((index + 1).to_string()),
                }
            }
            Node::Bookmark { owner, index } => self.eff[owner].bookmarks.get(*index).cloned(),
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.parents.get(node.0 as usize).copied().flatten()
    }

    fn traits(&self, node: NodeId) -> Vec<String> {
        let i = node.0 as usize;
        let Some(n) = self.nodes.get(i) else {
            return Vec::new();
        };
        match n {
            Node::Root | Node::SpaceRoot | Node::CatRoot | Node::TagRoot => Vec::new(),
            Node::Profile(name) => {
                let mut t = vec!["profile".to_string()];
                if self.reads.profile(name).is_ok_and(|p| p.jots_frozen) {
                    t.push("frozen".into());
                }
                t
            }
            Node::Space(_) => vec!["space".into()],
            Node::Dir(_) => vec!["dir".into()],
            Node::Cat(_) => vec!["cat".into()],
            Node::Tag(_) => vec!["tag".into()],
            Node::Jot(entry) => {
                let eff = &self.eff[entry];
                let mut t = vec!["jot".to_string()];
                if eff.todos.iter().any(|x| !x.done) {
                    t.push("todo".into());
                }
                if self
                    .reads
                    .idents
                    .get(entry)
                    .and_then(|id| self.reads.profile(&id.profile).ok())
                    .is_some_and(|p| p.jots_frozen)
                {
                    t.push("frozen".into());
                }
                if self.n_ref_backlinks(i) == 0 {
                    t.push("orphan".into());
                }
                t.extend(eff.tags.iter().cloned());
                t
            }
            Node::File(entry) => {
                let eff = &self.eff[entry];
                let mut t = vec!["file".to_string()];
                if eff.todos.iter().any(|x| !x.done) {
                    t.push("todo".into());
                }
                if self.n_ref_backlinks(i) == 0 {
                    t.push("orphan".into());
                }
                t.extend(eff.tags.iter().cloned());
                t
            }
            Node::Attachment { entry, .. } => {
                let mut t = vec!["attachment".to_string()];
                if self.reads.extraction_of(*entry).is_some() {
                    t.push("extracted".into());
                }
                t
            }
            Node::Todo { owner, index } => {
                let done = self.eff[owner].todos.get(*index).is_some_and(|t| t.done);
                vec![
                    "todo".to_string(),
                    if done { "done".into() } else { "open".into() },
                ]
            }
            Node::Bookmark { .. } => vec!["bookmark".into()],
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let i = node.0 as usize;
        if let Node::Todo { owner, index } = self.nodes.get(i)? {
            let todo = self.eff[owner].todos.get(*index)?;
            return match name {
                "action" => todo.action.clone().map(Value::Str),
                "description" => Some(Value::Str(todo.description.clone())),
                "importance" => Some(Value::Int(todo.importance as i64)),
                "raw" => Some(Value::Str(todo.raw.clone())),
                "due" => todo.due.as_deref().map(Self::instant_iso),
                "completed" => todo.completed.as_deref().map(Self::instant_iso),
                _ => None,
            };
        }
        let entry = self.indexed_entry(i)?;
        let eff = &self.eff[&entry];
        match name {
            "tags" => Some(Self::str_list(eff.tags.iter().cloned())),
            "categories" => Some(Self::str_list(eff.categories.iter().cloned())),
            "todos" => Some(Self::str_list(eff.todos.iter().map(|t| t.raw.clone()))),
            "urls" => Some(Self::str_list(eff.urls.iter().map(|u| u.url.clone()))),
            "bookmarks" => Some(Self::str_list(eff.bookmarks.iter().cloned())),
            "summary" => Some(Value::Str(summary_line(self.reads, entry))),
            _ => None,
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        let i = node.0 as usize;
        match self.nodes.get(i)? {
            Node::Jot(entry) | Node::File(entry) => {
                let bytes = self.reads.bytes(*entry).ok()?;
                Some(Value::Str(String::from_utf8_lossy(&bytes).into_owned()))
            }
            // An attachment's default value is its extracted text —
            // the queryable face of a binary blob (SPEC §7.2).
            Node::Attachment { entry, .. } => {
                let x = self.reads.extraction_of(*entry)?;
                let bytes = self.reads.bytes(x).ok()?;
                Some(Value::Str(String::from_utf8_lossy(&bytes).into_owned()))
            }
            Node::Todo { owner, index } => self.eff[owner]
                .todos
                .get(*index)
                .map(|t| Value::Str(t.raw.clone())),
            // A bookmark's default value is its fragment — the same
            // slice `cuj show --N#name` prints (SPEC §6.8).
            Node::Bookmark { owner, index } => {
                let name = self.eff[owner].bookmarks.get(*index)?;
                let bytes = self.reads.bytes(*owner).ok()?;
                let content = String::from_utf8_lossy(&bytes);
                extract_fragment(
                    self.reads,
                    *owner,
                    &content,
                    Some(&Suffix::Bookmark(name.clone())),
                )
                .ok()
                .map(Value::Str)
            }
            _ => None,
        }
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        let i = node.0 as usize;
        match self.nodes.get(i)? {
            Node::Jot(entry) => {
                let ident = self.reads.idents.get(entry)?;
                match key {
                    "jot-id" => Some(Value::Int(ident.id)),
                    "profile" => Some(Value::Str(ident.profile.clone())),
                    "ext" => Some(Value::Str(ident.ext.clone())),
                    "created" => self
                        .reads
                        .births
                        .get(entry)
                        .map(|b| Self::instant_ns(b.created_at_ns)),
                    "modified" => self
                        .reads
                        .modified
                        .get(entry)
                        .map(|ns| Self::instant_ns(*ns)),
                    "attachments" => {
                        Some(Value::Int(self.reads.attachments_of(*entry).len() as i64))
                    }
                    "n-links" => Some(Value::Int(self.n_ref_links(i) as i64)),
                    "n-backlinks" => Some(Value::Int(self.n_ref_backlinks(i) as i64)),
                    _ => None,
                }
            }
            Node::File(entry) => {
                let (space, rel) = self.reads.file_paths.get(entry)?;
                match key {
                    "space" => Some(Value::Str(space.clone())),
                    "rel-path" => Some(Value::Str(rel.clone())),
                    "ext" => Some(Value::Str(
                        rel.rsplit('.').next().unwrap_or_default().to_string(),
                    )),
                    "modified" => self
                        .reads
                        .modified
                        .get(entry)
                        .map(|ns| Self::instant_ns(*ns)),
                    "n-links" => Some(Value::Int(self.n_ref_links(i) as i64)),
                    "n-backlinks" => Some(Value::Int(self.n_ref_backlinks(i) as i64)),
                    _ => None,
                }
            }
            Node::Attachment { entry, .. } => {
                let att = self.reads.attachments.get(entry)?;
                match key {
                    "size" => Some(Value::bytes(att.size as i64)),
                    "added" => Some(Self::instant_ns(att.added_at_ns)),
                    "extracted" => Some(Value::Bool(self.reads.extraction_of(*entry).is_some())),
                    _ => None,
                }
            }
            // The number of entries linked to this exact node (a
            // category subtree sweeps compose it via `//*`): a
            // category's edges all arrive (`<-cat`), a tag's all
            // leave (`->tagged`).
            Node::Cat(_) if key == "n-jots" => Some(Value::Int(
                self.inlinks.get(&i).map(|v| v.len()).unwrap_or(0) as i64,
            )),
            Node::Tag(_) if key == "n-jots" => Some(Value::Int(
                self.outlinks.get(&i).map(|v| v.len()).unwrap_or(0) as i64,
            )),
            _ => None,
        }
    }

    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.outlinks
            .get(&(node.0 as usize))
            .cloned()
            .unwrap_or_default()
    }

    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.inlinks
            .get(&(node.0 as usize))
            .cloned()
            .unwrap_or_default()
    }

    fn link_property(
        &self,
        source: NodeId,
        label: &str,
        target: NodeId,
        name: &str,
    ) -> Option<Value> {
        let (written, annot) = self.edge_props.get(&(
            source.0 as usize,
            label.to_string(),
            target.0 as usize,
        ))?;
        match name {
            "category" => annot.category.clone().map(Value::Str),
            "tags" => (!annot.tags.is_empty()).then(|| Self::str_list(annot.tags.iter().cloned())),
            "as" => written.clone().map(Value::Str),
            "bookmark" => annot.bookmark.clone().map(Value::Str),
            "fragment" => annot.fragment.clone().map(Value::Str),
            // The written ##start,end form; either side may be open.
            "bookmark-range" => annot.bookmark_range.as_ref().map(|r| {
                Value::Str(format!(
                    "{},{}",
                    r.start.as_deref().unwrap_or(""),
                    r.end.as_deref().unwrap_or("")
                ))
            }),
            _ => None,
        }
    }
}
