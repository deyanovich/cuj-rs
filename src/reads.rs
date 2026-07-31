//! Reads are substrate queries (SPEC §9.1): everything fact-shaped
//! comes from metatheca's fact queries — at head or at a historical
//! view — and cuj holds no projection of its own.

use std::collections::{BTreeMap, HashMap, HashSet};

use metatheca::{Fact, Hash, Uuid, Vault, View};

use crate::error::{Error, Result};
use crate::facts::{
    self, AppliedOverlay, AttachmentBody, Birth, CounterBody, EffectiveIndex, ExtractionBody,
    Ident, ObsidianIndex, ParsedIndex, ProfileConfig, SavedQueryBody, SpaceBody, TemplateBody,
};

/// Where a (profile, id) lookup landed (SPEC §5.3).
pub enum Resolution {
    Live(Uuid),
    /// Identity is current but the entry has no live path.
    Deleted(Uuid),
    /// Courtesy answer only (SPEC §5.3): the identity was held
    /// historically by an entry whose current identity differs.
    Moved(Uuid, Ident),
}

pub struct Reads<'a> {
    vault: &'a Vault,
    view: Option<View<'a>>,
    /// The state this view was loaded at — the pin for
    /// `App::commit_pinned` (SPEC §3.2).
    pub head: Hash,
    /// entry -> state time of its latest content blob (the
    /// modification time of SPEC §10.4 / §11.2).
    pub modified: HashMap<Uuid, i64>,
    pub idents: HashMap<Uuid, Ident>,
    pub id_map: HashMap<(String, i64), Uuid>,
    pub births: HashMap<Uuid, Birth>,
    pub parsed: HashMap<Uuid, ParsedIndex>,
    pub applied: HashMap<Uuid, AppliedOverlay>,
    /// Dialect facts on file entries (SPEC §obsidian/index).
    pub obsidian: HashMap<Uuid, ObsidianIndex>,
    /// profile name -> (profile entry, config)
    pub profiles: BTreeMap<String, (Uuid, ProfileConfig)>,
    /// profile entry -> highest allocated id
    pub counters: HashMap<Uuid, i64>,
    pub attachments: HashMap<Uuid, AttachmentBody>,
    /// entry -> current `core/mime` value (SPEC §7.1 commits one
    /// per attachment; jots and observed files carry one too).
    pub mimes: HashMap<Uuid, String>,
    /// entry -> current content blob hash, for change detection
    /// and streaming reads without loading the bytes.
    pub blobs: HashMap<Uuid, Hash>,
    /// (extraction entry, body, state_ns)
    pub extractions: Vec<(Uuid, ExtractionBody, i64)>,
    /// space name -> (space entry, body); live registrations only.
    pub spaces: BTreeMap<String, (Uuid, SpaceBody)>,
    /// (profile, template name) -> (entry, body); live only.
    pub templates: BTreeMap<(String, String), (Uuid, TemplateBody)>,
    /// (profile, query name) -> (entry, body); live only.
    pub queries: BTreeMap<(String, String), (Uuid, SavedQueryBody)>,
    /// (space, relative path) -> file entry, from live
    /// `space/<name>/<rel>` paths.
    pub files: BTreeMap<(String, String), Uuid>,
    /// file entry -> (space, relative path).
    pub file_paths: HashMap<Uuid, (String, String)>,
    pub protocol: Option<String>,
    live: HashSet<Uuid>,
}

fn bodies<T: for<'de> serde::Deserialize<'de>>(rows: Vec<(Uuid, Fact)>) -> Vec<(Uuid, T)> {
    rows.into_iter()
        .filter_map(|(e, f)| facts::from_body::<T>(&f.body).map(|b| (e, b)))
        .collect()
}

impl<'a> Reads<'a> {
    pub fn load(vault: &'a Vault, at: Option<&Hash>) -> Result<Reads<'a>> {
        let view = match at {
            Some(h) => Some(vault.view_at(h)?),
            None => None,
        };
        // Record the pin before any fact query: if a concurrent
        // commit lands in between, the queries may see it but the
        // pin will not — the pinned commit then conflicts and the
        // caller rereads, which is safe; the reverse order could
        // pin a state the queries never saw.
        let head = match at {
            Some(h) => *h,
            None => vault.current_state_hash()?,
        };
        // Local dispatch: the same queries against head or the view.
        let current = |kind: &str| -> Result<Vec<(Uuid, Fact)>> {
            Ok(match &view {
                Some(v) => v.current_facts(kind)?,
                None => vault.current_facts(kind)?,
            })
        };
        let by_kind = |kind: &str| -> Result<Vec<(Uuid, Fact, i64)>> {
            Ok(match &view {
                Some(v) => v.facts_by_kind(kind)?,
                None => vault.facts_by_kind(kind)?,
            })
        };
        let ls = |prefix: Option<&str>| -> Result<Vec<(String, String)>> {
            Ok(match &view {
                Some(v) => v.ls(prefix)?,
                None => vault.ls(prefix)?,
            })
        };

        let mut r = Reads {
            vault,
            view: None,
            head,
            modified: HashMap::new(),
            idents: HashMap::new(),
            id_map: HashMap::new(),
            births: HashMap::new(),
            parsed: HashMap::new(),
            applied: HashMap::new(),
            obsidian: HashMap::new(),
            profiles: BTreeMap::new(),
            counters: HashMap::new(),
            attachments: HashMap::new(),
            mimes: HashMap::new(),
            blobs: HashMap::new(),
            extractions: Vec::new(),
            spaces: BTreeMap::new(),
            templates: BTreeMap::new(),
            queries: BTreeMap::new(),
            files: BTreeMap::new(),
            file_paths: HashMap::new(),
            protocol: None,
            live: HashSet::new(),
        };

        for (path, entry) in ls(None)? {
            if let Ok(u) = Uuid::parse_str(&entry) {
                r.live.insert(u);
                // File entries live at space/<name>/<rel> (SPEC
                // §file entries).
                if let Some(rest) = path.strip_prefix("space/") {
                    if let Some((space, rel)) = rest.split_once('/') {
                        r.files.insert((space.to_string(), rel.to_string()), u);
                        r.file_paths.insert(u, (space.to_string(), rel.to_string()));
                    }
                }
            }
        }
        for (e, ident) in bodies::<Ident>(current(facts::K_CUJ_ID)?) {
            r.id_map.insert((ident.profile.clone(), ident.id), e);
            r.idents.insert(e, ident);
        }
        for (e, b) in bodies::<Birth>(current(facts::K_CUJ_JOT)?) {
            r.births.insert(e, b);
        }
        for (e, p) in bodies::<ParsedIndex>(current(facts::K_JOT_INDEX)?) {
            r.parsed.insert(e, p);
        }
        for (e, a) in bodies::<AppliedOverlay>(current(facts::K_CUJ_APPLIED)?) {
            r.applied.insert(e, a);
        }
        for (e, o) in bodies::<ObsidianIndex>(current(facts::K_OBSIDIAN_INDEX)?) {
            // The empty snapshot is the retraction idiom (SPEC
            // §dialects): absent and empty read identically.
            if !o.is_empty() {
                r.obsidian.insert(e, o);
            }
        }
        for (e, p) in bodies::<ProfileConfig>(current(facts::K_CUJ_PROFILE)?) {
            if r.live.contains(&e) {
                r.profiles.insert(p.name.clone(), (e, p));
            }
        }
        for (e, c) in bodies::<CounterBody>(current(facts::K_CUJ_COUNTER)?) {
            r.counters.insert(e, c.value);
        }
        for (e, a) in bodies::<AttachmentBody>(current(facts::K_CUJ_ATTACHMENT)?) {
            r.attachments.insert(e, a);
        }
        for (e, f) in current("core/mime")? {
            if let Some(m) = f.body.get("mime_type").and_then(|v| v.as_str()) {
                r.mimes.insert(e, m.to_string());
            }
        }
        // `view` is moved into `r` only at the end of load — match
        // the local, not the (still-None) field.
        let blob_rows = match &view {
            Some(v) => v.current_blobs()?,
            None => vault.current_blobs()?,
        };
        for (e, h) in blob_rows {
            r.blobs.insert(e, h);
        }
        for (e, s) in bodies::<SpaceBody>(current(facts::K_CUJ_SPACE)?) {
            if r.live.contains(&e) {
                r.spaces.insert(s.name.clone(), (e, s));
            }
        }
        for (e, t) in bodies::<TemplateBody>(current(facts::K_CUJ_TEMPLATE)?) {
            if r.live.contains(&e) {
                r.templates
                    .insert((t.profile.clone(), t.name.clone()), (e, t));
            }
        }
        for (e, q) in bodies::<SavedQueryBody>(current(facts::K_CUJ_SAVED_QUERY)?) {
            if r.live.contains(&e) {
                r.queries
                    .insert((q.profile.clone(), q.name.clone()), (e, q));
            }
        }
        for (e, f, ns) in by_kind(facts::K_CUJ_EXTRACTION)? {
            if let Some(b) = facts::from_body::<ExtractionBody>(&f.body) {
                r.extractions.push((e, b, ns));
            }
        }
        for (e, _f, ns) in by_kind("core/blob-ref")? {
            let m = r.modified.entry(e).or_insert(ns);
            if ns > *m {
                *m = ns;
            }
        }
        if let Some((_, f)) = current(facts::K_CUJ_VAULT)?.into_iter().next() {
            r.protocol = f
                .body
                .get("protocol")
                .and_then(|v| v.as_str())
                .map(String::from);
        }
        r.view = view;
        Ok(r)
    }

    pub fn check_protocol(&self) -> Result<()> {
        match self.protocol.as_deref() {
            // The current revision — under either spelling — is
            // additive over 0.4: both serve (SPEC §5.13);
            // current-only writes gate on require_protocol().
            Some(facts::PROTOCOL)
            | Some(facts::PROTOCOL_ALIAS)
            | Some(facts::PROTOCOL_PREV) => Ok(()),
            Some(p) => Err(Error::Unsupported(format!(
                "vault protocol {p} (this build speaks {} — also \
                 spelled {} — and {})",
                facts::PROTOCOL,
                facts::PROTOCOL_ALIAS,
                facts::PROTOCOL_PREV
            ))),
            None => Err(Error::NotFound(
                "not a cuj vault (no cuj/vault fact; run `cuj init`)".into(),
            )),
        }
    }

    /// Gate for operations writing fact kinds the current revision
    /// added (SPEC §5.13).
    pub fn require_protocol(&self, what: &str) -> Result<()> {
        if matches!(
            self.protocol.as_deref(),
            Some(facts::PROTOCOL) | Some(facts::PROTOCOL_ALIAS)
        ) {
            Ok(())
        } else {
            Err(Error::Unsupported(format!(
                "{what} needs vault protocol {} (run `cuj migrate`)",
                facts::PROTOCOL
            )))
        }
    }

    pub fn bytes(&self, entry: Uuid) -> Result<Vec<u8>> {
        let target = entry.hyphenated().to_string();
        Ok(match &self.view {
            Some(v) => v.get_bytes(&target)?,
            None => self.vault.get_bytes(&target)?,
        })
    }

    pub fn is_live(&self, entry: Uuid) -> bool {
        self.live.contains(&entry)
    }

    pub fn effective(&self, entry: Uuid) -> EffectiveIndex {
        let mut e = facts::effective_index(self.parsed.get(&entry), self.applied.get(&entry));
        // File-jots in a dialect space extend to their dialect
        // facts (SPEC §file retrieval); the facts only exist on
        // such entries by construction.
        if let Some(o) = self.obsidian.get(&entry) {
            facts::union_dialect(&mut e, o);
        }
        e
    }

    pub fn profile(&self, name: &str) -> Result<&ProfileConfig> {
        self.profiles
            .get(name)
            .map(|(_, p)| p)
            .ok_or_else(|| Error::NotFound(format!("profile {name:?}")))
    }

    pub fn profile_entry(&self, name: &str) -> Result<Uuid> {
        self.profiles
            .get(name)
            .map(|(e, _)| *e)
            .ok_or_else(|| Error::NotFound(format!("profile {name:?}")))
    }

    /// SPEC §3.2: counter value + 1.
    pub fn next_id(&self, name: &str) -> Result<i64> {
        let e = self.profile_entry(name)?;
        Ok(self.counters.get(&e).copied().unwrap_or(0) + 1)
    }

    /// SPEC §5.3.
    pub fn resolve(&self, profile: &str, id: i64) -> Result<Resolution> {
        if let Some(e) = self.id_map.get(&(profile.to_string(), id)) {
            return Ok(if self.is_live(*e) {
                Resolution::Live(*e)
            } else {
                Resolution::Deleted(*e)
            });
        }
        // Courtesy hint (SPEC §5.3, MAY): scan historical identities.
        let all = match &self.view {
            Some(v) => v.facts_by_kind(facts::K_CUJ_ID)?,
            None => self.vault.facts_by_kind(facts::K_CUJ_ID)?,
        };
        for (e, f, _) in all {
            if let Some(i) = facts::from_body::<Ident>(&f.body) {
                if i.profile == profile && i.id == id {
                    if let Some(cur) = self.idents.get(&e) {
                        if !(cur.profile == profile && cur.id == id) {
                            return Ok(Resolution::Moved(e, cur.clone()));
                        }
                    }
                }
            }
        }
        Err(Error::NotFound(format!("--{profile}--{id}")))
    }

    pub fn resolve_live(&self, profile: &str, id: i64) -> Result<(Uuid, &Ident)> {
        match self.resolve(profile, id)? {
            Resolution::Live(e) => Ok((e, &self.idents[&e])),
            Resolution::Deleted(_) => Err(Error::Deleted(format!("--{profile}--{id}"))),
            Resolution::Moved(_, cur) => Err(Error::NotFound(format!(
                "--{profile}--{id} (moved to --{}--{})",
                cur.profile, cur.id
            ))),
        }
    }

    /// Live jots of one profile (or all), ascending (profile, id).
    pub fn list(&self, profile: Option<&str>) -> Vec<(Uuid, &Ident)> {
        let mut v: Vec<(Uuid, &Ident)> = self
            .idents
            .iter()
            .filter(|(e, _)| self.is_live(**e))
            .filter(|(_, i)| profile.is_none_or(|p| i.profile == p))
            .map(|(e, i)| (*e, i))
            .collect();
        v.sort_by(|a, b| (&a.1.profile, a.1.id).cmp(&(&b.1.profile, b.1.id)));
        v
    }

    /// Non-detached attachments of a jot, sorted by filename.
    pub fn attachments_of(&self, jot: Uuid) -> Vec<(Uuid, &AttachmentBody)> {
        let key = jot.hyphenated().to_string();
        let mut v: Vec<(Uuid, &AttachmentBody)> = self
            .attachments
            .iter()
            .filter(|(_, a)| a.jot == key && !a.detached)
            .map(|(e, a)| (*e, a))
            .collect();
        v.sort_by(|a, b| a.1.filename.cmp(&b.1.filename));
        v
    }

    /// Latest extraction for an attachment entry.
    pub fn extraction_of(&self, attachment: Uuid) -> Option<Uuid> {
        let key = attachment.hyphenated().to_string();
        self.extractions
            .iter()
            .filter(|(_, b, _)| b.of == key)
            .max_by_key(|(_, _, ns)| *ns)
            .map(|(e, _, _)| *e)
    }

    /// Map any entry to its owning jot entry (SPEC §8.3); observed
    /// file entries are their own result (SPEC §file retrieval).
    pub fn owning_jot(&self, entry: Uuid) -> Option<Uuid> {
        if self.idents.contains_key(&entry) || self.file_paths.contains_key(&entry) {
            return Some(entry);
        }
        let jot_of_att = |a: &AttachmentBody| Uuid::parse_str(&a.jot).ok();
        if let Some(a) = self.attachments.get(&entry) {
            return jot_of_att(a);
        }
        if let Some((_, b, _)) = self.extractions.iter().find(|(e, _, _)| *e == entry) {
            let att = Uuid::parse_str(&b.of).ok()?;
            return jot_of_att(self.attachments.get(&att)?);
        }
        None
    }
}
