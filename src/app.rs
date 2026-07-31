//! Application state and the write-operation surface (SPEC §10).
//! Every write is specified as the fact set it commits; nothing
//! here mutates the vault except through `Vault::commit`.

use std::path::{Path, PathBuf};

use metatheca::{Fact, FsMetadata, Hash, Uuid, Vault};

use crate::binding::{self, mime_for_bytes, mime_for_ext};
use crate::client::ClientConfig;
use crate::error::{Error, Result};
use crate::ext;
use crate::facts::{
    self, AppliedOverlay, AttachmentBody, Birth, CounterBody, EdgeBody, ExtractionBody, Ident,
    ProfileConfig, TodoBody, check_constraints, now_ns, profile_path, to_fact,
};
use crate::reads::Reads;

pub struct App {
    pub client: ClientConfig,
    pub vault: Vault,
}

/// The bound on head-CAS retries (SPEC §3.2): a write that keeps
/// losing the race rereads and rebuilds this many times, then
/// surfaces `Conflict`.
pub const CAS_RETRIES: usize = 8;

/// Global CLI options (SPEC §11.1).
#[derive(Debug, Default, Clone)]
pub struct Opts {
    pub profile: Option<String>,
    pub json: bool,
    pub at: Option<String>,
    /// Pin `now()` in Quarb queries to this ISO instant (`--now`);
    /// unset, `cuj q` binds one startup clock read.
    pub now: Option<String>,
    pub force: bool,
    pub stale: bool,
    pub ext_override: Option<String>,
    pub k: Option<usize>,
    /// Emit "index is stale" notes on stderr under --stale. The
    /// CLI turns this on; it defaults off so library consumers
    /// (a raw-mode TUI) get no uncontrolled terminal writes.
    pub stale_notes: bool,
}

impl Opts {
    /// The profile write commands operate on. `all` is read-only.
    pub fn write_profile<'a>(&'a self, client: &'a ClientConfig) -> Result<&'a str> {
        let p = self.profile.as_deref().unwrap_or(&client.active_profile);
        if p == "all" {
            return Err(Error::Usage("--profile all is read-only".into()));
        }
        Ok(p)
    }

    /// Read scope: None = all profiles.
    pub fn read_scope<'a>(&'a self, client: &'a ClientConfig) -> Option<&'a str> {
        match self.profile.as_deref() {
            Some("all") => None,
            Some(p) => Some(p),
            None => Some(&client.active_profile),
        }
    }
}

/// A parsed `--…` reference argument.
#[derive(Debug, Clone, PartialEq)]
pub struct RefArg {
    pub profile: Option<String>,
    pub id: i64,
    pub suffix: Option<Suffix>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Suffix {
    Bookmark(String),
    Lines(usize, usize),
}

/// Parse `--42`, `--work--42`, `--42#intro`, `--42:10-20`.
pub fn parse_ref(arg: &str) -> Option<RefArg> {
    let body = arg.strip_prefix("--")?;
    let (main, suffix) = match body.find(['#', ':']) {
        Some(i) => {
            let (m, s) = body.split_at(i);
            let suffix = match s.as_bytes()[0] {
                b'#' => Some(Suffix::Bookmark(s[1..].to_string())),
                _ => parse_lines(&s[1..]).map(|(a, b)| Suffix::Lines(a, b)),
            };
            suffix.as_ref()?;
            (m, suffix)
        }
        None => (body, None),
    };
    if let Some(i) = main.find("--") {
        let (p, rest) = main.split_at(i);
        let id: i64 = rest[2..].parse().ok()?;
        if facts::validate_profile_name(p).is_err() || id < 1 {
            return None;
        }
        Some(RefArg {
            profile: Some(p.to_string()),
            id,
            suffix,
        })
    } else {
        let id: i64 = main.parse().ok()?;
        if id < 1 {
            return None;
        }
        Some(RefArg {
            profile: None,
            id,
            suffix,
        })
    }
}

fn parse_lines(s: &str) -> Option<(usize, usize)> {
    if let Some((a, b)) = s.split_once('-') {
        let a: usize = a.parse().ok()?;
        let b: usize = b.parse().ok()?;
        (a >= 1 && b >= a).then_some((a, b))
    } else if let Some((a, l)) = s.split_once('+') {
        let a: usize = a.parse().ok()?;
        let l: usize = l.parse().ok()?;
        (a >= 1).then_some((a, a + l))
    } else {
        let a: usize = s.parse().ok()?;
        (a >= 1).then_some((a, a))
    }
}

pub fn jot_path(ident: &Ident) -> String {
    format!("{}/{}.{}", ident.profile, ident.id, ident.ext)
}

pub fn att_path(profile: &str, id: i64, filename: &str) -> String {
    format!("{profile}/att/{id}/{filename}")
}

impl App {
    pub fn open(client: ClientConfig) -> Result<App> {
        let vault = Vault::open(&client.vault).map_err(|e| match e {
            // Only a genuinely absent vault gets the init hint —
            // reporting EACCES or a read error as "no vault" invites
            // a destructive re-init attempt.
            metatheca::Error::NotFound(_) => Error::NotFound(format!(
                "no cuj vault at {} (run `cuj init` first)",
                client.vault.display()
            )),
            metatheca::Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
                Error::NotFound(format!(
                    "no cuj vault at {} (run `cuj init` first)",
                    client.vault.display()
                ))
            }
            other => other.into(),
        })?;
        Ok(App { client, vault })
    }

    pub fn root(&self) -> &Path {
        self.vault.root()
    }

    /// Load the fact-query view at head or at --at (SPEC §9.1).
    pub fn reads(&self, opts: &Opts) -> Result<Reads<'_>> {
        let at = match &opts.at {
            Some(sref) => Some(self.vault.resolve(sref)?),
            None => None,
        };
        let r = Reads::load(&self.vault, at.as_ref())?;
        r.check_protocol()?;
        Ok(r)
    }

    pub fn require_no_at(&self, opts: &Opts, what: &str) -> Result<()> {
        if opts.at.is_some() {
            return Err(Error::Usage(format!("{what} cannot run under --at")));
        }
        Ok(())
    }

    /// The write-side retry driver (SPEC §3.2): each attempt loads a
    /// fresh fact view and rebuilds its commit against it; an attempt
    /// that loses the head-CAS race (`Conflict` out of
    /// `commit_pinned`) is retried, and `Conflict` surfaces once the
    /// bound is exhausted.
    pub fn with_cas_retry<T>(&self, opts: &Opts, f: impl Fn(&Reads<'_>) -> Result<T>) -> Result<T> {
        for _ in 0..CAS_RETRIES {
            let reads = self.reads(opts)?;
            match f(&reads) {
                Err(Error::Conflict) => continue,
                other => return other,
            }
        }
        Err(Error::Conflict)
    }

    /// Commit a batch pinned to the state `reads` was loaded at:
    /// fails with `Conflict` instead of rebasing when another writer
    /// advanced head in between, so a batch derived from `reads` is
    /// never committed against state it did not see (SPEC §3.2).
    pub fn commit_pinned(&self, reads: &Reads<'_>, facts: &[Fact]) -> Result<Hash> {
        Ok(self.vault.commit_onto(facts, &reads.head)?)
    }

    fn ref_profile(&self, r: &RefArg, opts: &Opts) -> Result<String> {
        match &r.profile {
            Some(p) => Ok(p.clone()),
            None => opts
                .read_scope(&self.client)
                .map(String::from)
                .ok_or_else(|| {
                    Error::Usage(
                        "bare --N is ambiguous under --profile all; use --profile--N".into(),
                    )
                }),
        }
    }

    pub fn resolve_live<'r>(
        &self,
        reads: &'r Reads<'_>,
        r: &RefArg,
        opts: &Opts,
    ) -> Result<(Uuid, &'r Ident)> {
        let profile = self.ref_profile(r, opts)?;
        reads.resolve_live(&profile, r.id)
    }

    /// Parse content and build the jot/index body, resolving
    /// references against the current identities (SPEC §3.4).
    pub(crate) fn parse_index(
        &self,
        reads: &Reads<'_>,
        profile: &ProfileConfig,
        own_profile: &str,
        content: &str,
    ) -> facts::ParsedIndex {
        let result = binding::parse(content, profile);
        let resolve = |p: &str, n: i64| reads.id_map.get(&(p.to_string(), n)).copied();
        binding::to_parsed_index(&result, own_profile, &resolve)
    }

    fn counter_fact(&self, reads: &Reads<'_>, profile: &str, value: i64) -> Result<Fact> {
        to_fact(
            facts::K_CUJ_COUNTER,
            reads.profile_entry(profile)?,
            &CounterBody { value },
        )
    }

    // ── init (SPEC §10.1) ───────────────────────────────────────────

    pub fn init(client: ClientConfig, profile_arg: Option<&str>) -> Result<()> {
        let profile = profile_arg.unwrap_or("default");
        facts::validate_profile_name(profile)?;
        let first_run = !client.vault.join("cella").is_dir();
        if first_run {
            std::fs::create_dir_all(&client.vault)?;
            let vault = Vault::init(&client.vault)?;
            let root_entry = vault.new_entry_id();
            let profile_entry = vault.new_entry_id();
            vault.commit(&[
                facts::vault_fact(root_entry)?,
                Fact::path(root_entry, facts::ROOT_PATH, true),
                to_fact(
                    facts::K_CUJ_PROFILE,
                    profile_entry,
                    &ProfileConfig::new(profile),
                )?,
                to_fact(
                    facts::K_CUJ_COUNTER,
                    profile_entry,
                    &CounterBody { value: 0 },
                )?,
                Fact::path(profile_entry, &profile_path(profile), true),
            ])?;
            ext::init_chains(&client.vault)?;
            println!(
                "initialized cuj vault at {} with profile {profile:?}",
                client.vault.display()
            );
        } else {
            let app = App::open(client)?;
            app.with_cas_retry(&Opts::default(), |reads| app.create_profile(reads, profile))?;
            println!("created profile {profile:?}");
        }
        Ok(())
    }

    fn create_profile(&self, reads: &Reads<'_>, name: &str) -> Result<Uuid> {
        facts::validate_profile_name(name)?;
        if reads.profiles.contains_key(name) {
            return Err(Error::Usage(format!("profile {name:?} already exists")));
        }
        let entry = self.vault.new_entry_id();
        self.commit_pinned(
            reads,
            &[
                to_fact(facts::K_CUJ_PROFILE, entry, &ProfileConfig::new(name))?,
                to_fact(facts::K_CUJ_COUNTER, entry, &CounterBody { value: 0 })?,
                Fact::path(entry, &profile_path(name), true),
            ],
        )?;
        Ok(entry)
    }

    // ── create (SPEC §10.2) ─────────────────────────────────────────

    pub fn create(&self, opts: &Opts, text: &str) -> Result<Ident> {
        self.create_at(opts, text, None)
    }

    /// The create path with an explicit creation instant — the
    /// ingest binding (SPEC §inbox) dates captures by file mtime.
    pub fn create_at(&self, opts: &Opts, text: &str, created_ns: Option<i64>) -> Result<Ident> {
        self.require_no_at(opts, "create")?;
        self.with_cas_retry(opts, |reads| {
            let profile_name = opts.write_profile(&self.client)?;
            let profile = reads.profile(profile_name)?.clone();
            let ext = match &opts.ext_override {
                Some(e) => {
                    facts::validate_ext(e)?;
                    e.clone()
                }
                None => profile.default_ext.clone(),
            };

            let text = binding::normalize_due_times(text, &profile);
            let content = if text.ends_with('\n') {
                text
            } else {
                format!("{text}\n")
            };
            let parsed = self.parse_index(reads, &profile, profile_name, &content);
            let eff = facts::effective_index(Some(&parsed), None);
            check_constraints(&profile, &eff, opts.force)?;

            let id = reads.next_id(profile_name)?;
            let ident = Ident {
                profile: profile_name.to_string(),
                id,
                ext,
            };
            let birth = Birth {
                profile: ident.profile.clone(),
                id,
                ext: ident.ext.clone(),
                created_at_ns: created_ns.unwrap_or_else(now_ns),
            };
            let entry = self.vault.new_entry_id();
            let blob = self.vault.deposit_blob(content.as_bytes())?;

            self.commit_pinned(
                reads,
                &[
                    to_fact(facts::K_CUJ_JOT, entry, &birth)?,
                    to_fact(facts::K_CUJ_ID, entry, &ident)?,
                    to_fact(facts::K_JOT_INDEX, entry, &parsed)?,
                    Fact::blob_ref(entry, &blob.to_hex(), content.len() as u64),
                    Fact::path(entry, &jot_path(&ident), true),
                    Fact::fs_metadata(
                        entry,
                        &FsMetadata {
                            birthtime_ns: Some(birth.created_at_ns),
                            ..Default::default()
                        },
                        None,
                    ),
                    Fact::mime(entry, mime_for_ext(&ident.ext)),
                    self.counter_fact(reads, profile_name, id)?,
                ],
            )?;
            Ok(ident)
        })
    }

    // ── overlay writes: label / link / todo (SPEC §10.12–10.14) ────

    fn commit_overlay(
        &self,
        reads: &Reads<'_>,
        entry: Uuid,
        ident: &Ident,
        overlay: AppliedOverlay,
        opts: &Opts,
    ) -> Result<bool> {
        let profile = reads.profile(&ident.profile)?;
        let eff = facts::effective_index(reads.parsed.get(&entry), Some(&overlay));
        check_constraints(profile, &eff, opts.force)?;
        // Idempotence: no-op when the effective index is unchanged.
        let before = reads.effective(entry);
        if before.tags == eff.tags
            && before.categories == eff.categories
            && before.edges == eff.edges
            && before.todos == eff.todos
        {
            return Ok(false);
        }
        self.commit_pinned(reads, &[to_fact(facts::K_CUJ_APPLIED, entry, &overlay)?])?;
        Ok(true)
    }

    fn canon_overlay(o: &mut AppliedOverlay) {
        o.tags.sort();
        o.tags.dedup();
        o.categories.sort();
        o.categories.dedup();
        o.references.sort_by(|a, b| a.entry.cmp(&b.entry));
        o.references.dedup_by(|a, b| a.entry == b.entry);
    }

    pub fn label(&self, opts: &Opts, refs: &[RefArg], tokens: &[String]) -> Result<Vec<String>> {
        self.require_no_at(opts, "label")?;
        let mut tags = Vec::new();
        let mut cats = Vec::new();
        for t in tokens {
            if let Some(x) = t.strip_prefix("..") {
                let x = x.to_lowercase();
                facts::validate_tag(&x)?;
                tags.push(x);
            } else if let Some(x) = t.strip_prefix("::") {
                let x = x.to_lowercase();
                facts::validate_category(&x)?;
                cats.push(x);
            } else {
                return Err(Error::Usage(format!(
                    "label takes ..tag / ::category tokens, got {t:?}"
                )));
            }
        }
        let mut report = Vec::new();
        for r in refs {
            // Resolution errors abort; per-ref commit errors are
            // reported (Conflict alone escapes to the retry driver).
            let (ident, outcome) = self.with_cas_retry(opts, |reads| {
                let (entry, ident) = self.resolve_live(reads, r, opts)?;
                let ident = ident.clone();
                let mut overlay = reads.applied.get(&entry).cloned().unwrap_or_default();
                overlay.tags.extend(tags.iter().cloned());
                overlay.categories.extend(cats.iter().cloned());
                Self::canon_overlay(&mut overlay);
                match self.commit_overlay(reads, entry, &ident, overlay, opts) {
                    Err(Error::Conflict) => Err(Error::Conflict),
                    other => Ok((ident, other)),
                }
            })?;
            match outcome {
                Ok(true) => report.push(format!("--{}--{} labeled", ident.profile, ident.id)),
                Ok(false) => report.push(format!(
                    "--{}--{} unchanged (labels already present)",
                    ident.profile, ident.id
                )),
                Err(e) => report.push(format!("--{}--{} failed: {e}", ident.profile, ident.id)),
            }
        }
        Ok(report)
    }

    pub fn link(&self, opts: &Opts, src: &RefArg, tgt: &RefArg) -> Result<bool> {
        self.require_no_at(opts, "link")?;
        self.with_cas_retry(opts, |reads| {
            let (src_entry, src_ident) = self.resolve_live(reads, src, opts)?;
            let src_ident = src_ident.clone();
            let (tgt_entry, tgt_ident) = self.resolve_live(reads, tgt, opts)?;

            // An edge already in the effective index — content-
            // parsed or overlay, whatever its written form — commits
            // nothing (SPEC §10.13). `link` creates a BARE edge, and
            // typed edges are distinct (§5.5): only an existing
            // bare edge to the target makes it a no-op.
            if reads
                .effective(src_entry)
                .edges
                .get(&tgt_entry.hyphenated().to_string())
                .is_some_and(|infos| infos.iter().any(|i| i.annot.category.is_none()))
            {
                return Ok(false);
            }

            let mut overlay = reads.applied.get(&src_entry).cloned().unwrap_or_default();
            overlay.references.push(EdgeBody {
                entry: tgt_entry.hyphenated().to_string(),
                written_as: format!("{}:{}", tgt_ident.profile, tgt_ident.id),
                annot: facts::EdgeAnnot::default(),
            });
            Self::canon_overlay(&mut overlay);
            self.commit_overlay(reads, src_entry, &src_ident, overlay, opts)
        })
    }

    /// Parse a `??…` construct in isolation into a TodoBody.
    fn parse_todo_token(
        &self,
        reads: &Reads<'_>,
        profile: &ProfileConfig,
        construct: &str,
    ) -> Result<TodoBody> {
        let content = binding::normalize_due_times(construct, profile);
        let parsed = self.parse_index(reads, profile, &profile.name, &content);
        parsed
            .todos
            .into_iter()
            .next()
            .ok_or_else(|| Error::Usage(format!("not a todo construct: {construct:?}")))
    }

    pub fn todo(
        &self,
        opts: &Opts,
        refs: &[RefArg],
        construct: &str,
    ) -> Result<either::TodoOutcome> {
        self.require_no_at(opts, "todo")?;
        if refs.is_empty() {
            let ident = self.create(opts, construct)?;
            return Ok(either::TodoOutcome::Created(ident));
        }
        let mut applied = Vec::new();
        for r in refs {
            let ident = self.with_cas_retry(opts, |reads| {
                let (entry, ident) = self.resolve_live(reads, r, opts)?;
                let ident = ident.clone();
                let profile = reads.profile(&ident.profile)?.clone();
                let todo = self.parse_todo_token(reads, &profile, construct)?;
                let mut overlay = reads.applied.get(&entry).cloned().unwrap_or_default();
                if !overlay.todos.iter().any(|t| t.raw == todo.raw) {
                    overlay.todos.push(todo);
                }
                Self::canon_overlay(&mut overlay);
                self.commit_overlay(reads, entry, &ident, overlay, opts)?;
                Ok(ident)
            })?;
            applied.push(ident);
        }
        Ok(either::TodoOutcome::Applied(applied))
    }

    // ── todo done (SPEC §10.15) ─────────────────────────────────────

    pub fn todo_done(&self, opts: &Opts, r: &RefArg, matcher: &str) -> Result<String> {
        self.require_no_at(opts, "todo done")?;
        self.with_cas_retry(opts, |reads| {
            let (entry, ident) = self.resolve_live(reads, r, opts)?;
            let ident = ident.clone();
            let profile = reads.profile(&ident.profile)?.clone();
            let eff = reads.effective(entry);
            // <match> selects by the 1-based ordinal of show's todo
            // listing (SPEC §10.15), which numbers all todos — done
            // ones included — or by raw/description text, preferring
            // an open match.
            let target: &TodoBody = if let Ok(ord) = matcher.parse::<usize>() {
                eff.todos
                    .get(
                        ord.checked_sub(1)
                            .ok_or_else(|| Error::Usage("todo ordinals are 1-based".into()))?,
                    )
                    .ok_or_else(|| Error::NotFound(format!("todo #{ord}")))?
            } else {
                eff.todos
                    .iter()
                    .find(|t| !t.done && (t.raw == matcher || t.description == matcher))
                    .or_else(|| {
                        eff.todos
                            .iter()
                            .find(|t| t.raw == matcher || t.description == matcher)
                    })
                    .ok_or_else(|| Error::NotFound(format!("todo matching {matcher:?}")))?
            };
            if target.done {
                return Ok(format!(
                    "--{}--{}: todo already done",
                    ident.profile, ident.id
                ));
            }
            let target = target.clone();

            let in_content = reads
                .parsed
                .get(&entry)
                .is_some_and(|p| p.todos.iter().any(|t| t.raw == target.raw));

            if in_content && !profile.jots_frozen {
                // Content rewrite to the Jot Syntax done form (`??.`).
                let content = String::from_utf8_lossy(&reads.bytes(entry)?).into_owned();
                let done_raw = target.raw.replacen("??", "??.", 1);
                let (new_content, n) =
                    binding::rewrite_todo(&content, &profile, &target.raw, &done_raw);
                if n == 0 {
                    return Err(Error::NotFound(format!(
                        "todo construct {:?} not found in content",
                        target.raw
                    )));
                }
                self.commit_content(reads, entry, &ident, &new_content, opts)?;
                Ok(format!(
                    "--{}--{}: todo done (content)",
                    ident.profile, ident.id
                ))
            } else {
                // Frozen (or overlay-only todo): override in the overlay.
                let mut overlay = reads.applied.get(&entry).cloned().unwrap_or_default();
                let mut done = target.clone();
                done.done = true;
                if let Some(t) = overlay.todos.iter_mut().find(|t| t.raw == done.raw) {
                    *t = done;
                } else {
                    overlay.todos.push(done);
                }
                self.commit_overlay(reads, entry, &ident, overlay, opts)?;
                Ok(format!(
                    "--{}--{}: todo done (overlay)",
                    ident.profile, ident.id
                ))
            }
        })
    }

    // ── content writes: edit / promote / demote (SPEC §10.16, 10.19) ─

    fn commit_content(
        &self,
        reads: &Reads<'_>,
        entry: Uuid,
        ident: &Ident,
        new_content: &str,
        opts: &Opts,
    ) -> Result<()> {
        let profile = reads.profile(&ident.profile)?;
        if profile.jots_frozen {
            return Err(Error::Frozen(format!(
                "profile {:?} has jots_frozen = true",
                ident.profile
            )));
        }
        let parsed = self.parse_index(reads, profile, &ident.profile, new_content);
        let eff = facts::effective_index(Some(&parsed), reads.applied.get(&entry));
        check_constraints(profile, &eff, opts.force)?;
        let blob = self.vault.deposit_blob(new_content.as_bytes())?;
        self.commit_pinned(
            reads,
            &[
                to_fact(facts::K_JOT_INDEX, entry, &parsed)?,
                Fact::blob_ref(entry, &blob.to_hex(), new_content.len() as u64),
            ],
        )?;
        Ok(())
    }

    pub fn edit(&self, opts: &Opts, r: &RefArg) -> Result<bool> {
        self.require_no_at(opts, "edit")?;
        let reads = self.reads(opts)?;
        let (entry, ident) = self.resolve_live(&reads, r, opts)?;
        let ident = ident.clone();
        let profile = reads.profile(&ident.profile)?;
        if profile.jots_frozen {
            return Err(Error::Frozen(format!(
                "profile {:?} has jots_frozen = true",
                ident.profile
            )));
        }
        let content = reads.bytes(entry)?;

        // Private random-named tempdir: a predictable path in a
        // shared /tmp invites symlink planting.
        let tmpdir = tempfile::Builder::new().prefix("cuj-edit-").tempdir()?;
        let tmpfile = tmpdir
            .path()
            .join(format!("{}-{}.{}", ident.profile, ident.id, ident.ext));
        std::fs::write(&tmpfile, &content)?;

        let editor = self.client.editor_command();
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} {}", shell_quote(&tmpfile)))
            .status()?;
        if !status.success() {
            return Err(Error::Substrate(format!("editor exited with {status}")));
        }
        let new_content = std::fs::read_to_string(&tmpfile)?;
        // An untouched buffer commits nothing: comparing the editor
        // output against *fresh* state instead would let a no-change
        // session silently revert a concurrent write to this jot.
        if new_content.as_bytes() == content.as_slice() {
            return Ok(false);
        }
        // The editor ran outside the retry loop; the commit is
        // rebuilt after a lost race.
        self.update_content(opts, r, &new_content)
    }

    /// Non-interactive content update — the library-facing write
    /// path behind `edit` (SPEC §10.16), for consumers that cannot
    /// shell out to $EDITOR (a web backend, a TUI with its own
    /// buffer). Same frozen/constraint discipline, CAS-retried.
    /// Returns false when the content is byte-identical.
    pub fn update_content(&self, opts: &Opts, r: &RefArg, new_content: &str) -> Result<bool> {
        self.require_no_at(opts, "update")?;
        self.with_cas_retry(opts, |reads| {
            let (entry, ident) = self.resolve_live(reads, r, opts)?;
            let ident = ident.clone();
            if new_content.as_bytes() == reads.bytes(entry)?.as_slice() {
                return Ok(false);
            }
            self.commit_content(reads, entry, &ident, new_content, opts)?;
            Ok(true)
        })
    }

    // ── rm / mv (SPEC §10.17, 10.18) ───────────────────────────────

    pub fn rm(&self, opts: &Opts, r: &RefArg) -> Result<Ident> {
        self.require_no_at(opts, "rm")?;
        self.with_cas_retry(opts, |reads| {
            let (entry, ident) = self.resolve_live(reads, r, opts)?;
            let ident = ident.clone();
            // One commit of path retractions — that is the whole
            // deletion (SPEC §10.17).
            let mut batch = vec![Fact::path(entry, &jot_path(&ident), false)];
            for (att_entry, att) in reads.attachments_of(entry) {
                batch.push(Fact::path(
                    att_entry,
                    &att_path(&ident.profile, ident.id, &att.filename),
                    false,
                ));
            }
            self.commit_pinned(reads, &batch)?;
            Ok(ident)
        })
    }

    pub fn mv(&self, opts: &Opts, from: &str, to: &str, id: i64) -> Result<(Ident, Ident)> {
        self.require_no_at(opts, "mv")?;
        facts::validate_profile_name(to)?;
        self.with_cas_retry(opts, |reads| {
            let target_profile = reads.profile(to)?.clone();
            let (entry, old) = reads.resolve_live(from, id)?;
            let old = old.clone();

            // Constraint check against the *target* profile.
            let eff = reads.effective(entry);
            check_constraints(&target_profile, &eff, opts.force)?;

            let new_id = reads.next_id(to)?;
            let new = Ident {
                profile: to.to_string(),
                id: new_id,
                ext: old.ext.clone(),
            };
            let mut batch = vec![
                to_fact(facts::K_CUJ_ID, entry, &new)?,
                Fact::path(entry, &jot_path(&old), false),
                Fact::path(entry, &jot_path(&new), true),
                self.counter_fact(reads, to, new_id)?,
            ];
            for (att_entry, att) in reads.attachments_of(entry) {
                batch.push(Fact::path(
                    att_entry,
                    &att_path(&old.profile, old.id, &att.filename),
                    false,
                ));
                batch.push(Fact::path(
                    att_entry,
                    &att_path(&new.profile, new.id, &att.filename),
                    true,
                ));
            }
            self.commit_pinned(reads, &batch)?;
            Ok((old, new))
        })
    }

    // ── promote / demote (SPEC §10.19) ─────────────────────────────

    pub fn promote_tag(&self, opts: &Opts, tag: &str) -> Result<Vec<String>> {
        self.rewrite_batch(opts, &format!("..{tag}"), &format!("::{tag}"), tag, true)
    }

    pub fn demote_category(&self, opts: &Opts, cat: &str) -> Result<Vec<String>> {
        self.rewrite_batch(opts, &format!("::{cat}"), &format!("..{cat}"), cat, false)
    }

    fn rewrite_batch(
        &self,
        opts: &Opts,
        from_raw: &str,
        to_raw: &str,
        value: &str,
        tag_to_cat: bool,
    ) -> Result<Vec<String>> {
        self.require_no_at(opts, "promote/demote")?;
        let profile_name = opts.write_profile(&self.client)?.to_string();
        let value = value.to_lowercase();
        let ids: Vec<i64> = {
            let reads = self.reads(opts)?;
            if reads.profile(&profile_name)?.jots_frozen {
                return Err(Error::Frozen(format!(
                    "profile {profile_name:?} has jots_frozen = true \
                     (promote/demote rewrite content)"
                )));
            }
            reads
                .list(Some(&profile_name))
                .into_iter()
                .map(|(_, i)| i.id)
                .collect()
        };
        let mut report = Vec::new();
        for id in ids {
            let line = self.with_cas_retry(opts, |reads| {
                let (entry, ident) = reads.resolve_live(&profile_name, id)?;
                let ident = ident.clone();
                let eff = reads.effective(entry);
                let carries = if tag_to_cat {
                    eff.tags.contains(&value)
                } else {
                    eff.categories.contains(&value)
                };
                if !carries {
                    return Ok(None);
                }
                let profile = reads.profile(&ident.profile)?;

                // Content rewrite and overlay move land as ONE
                // commit, constraint-checked on the combined result.
                let mut batch = Vec::new();
                let content = String::from_utf8_lossy(&reads.bytes(entry)?).into_owned();
                let (new_content, n) = binding::rewrite_constructs(&content, from_raw, to_raw);
                let parsed_new =
                    (n > 0).then(|| self.parse_index(reads, profile, &ident.profile, &new_content));

                let mut overlay_new = reads.applied.get(&entry).cloned();
                let mut overlay_changed = false;
                if let Some(o) = overlay_new.as_mut() {
                    let (from_list, to_list) = if tag_to_cat {
                        (&mut o.tags, &mut o.categories)
                    } else {
                        (&mut o.categories, &mut o.tags)
                    };
                    if let Some(pos) = from_list.iter().position(|x| x == &value) {
                        from_list.remove(pos);
                        to_list.push(value.clone());
                        Self::canon_overlay(o);
                        overlay_changed = true;
                    }
                }

                let eff = facts::effective_index(
                    parsed_new.as_ref().or(reads.parsed.get(&entry)),
                    if overlay_changed {
                        overlay_new.as_ref()
                    } else {
                        reads.applied.get(&entry)
                    },
                );
                check_constraints(profile, &eff, opts.force)?;

                if let Some(parsed) = &parsed_new {
                    let blob = self.vault.deposit_blob(new_content.as_bytes())?;
                    batch.push(to_fact(facts::K_JOT_INDEX, entry, parsed)?);
                    batch.push(Fact::blob_ref(
                        entry,
                        &blob.to_hex(),
                        new_content.len() as u64,
                    ));
                }
                if overlay_changed {
                    batch.push(to_fact(
                        facts::K_CUJ_APPLIED,
                        entry,
                        overlay_new.as_ref().unwrap(),
                    )?);
                }
                let rewrote = !batch.is_empty();
                if rewrote {
                    self.commit_pinned(reads, &batch)?;
                }
                // carries && nothing rewritten means the construct
                // only occurs chained inside a longer one, which the
                // scan refuses to touch — say so, don't imply done.
                Ok(Some(format!(
                    "--{}--{} {}",
                    ident.profile,
                    ident.id,
                    if rewrote {
                        "rewritten".to_string()
                    } else {
                        format!("skipped ({from_raw} only occurs chained in content)")
                    }
                )))
            })?;
            if let Some(l) = line {
                report.push(l);
            }
        }
        Ok(report)
    }

    pub fn promote_category(&self, opts: &Opts, cat: &str) -> Result<Vec<String>> {
        self.require_no_at(opts, "promote")?;
        let source_profile = opts.write_profile(&self.client)?.to_string();
        let cat = cat.to_lowercase();
        let new_profile = cat.rsplit("::").next().unwrap_or(&cat).to_string();

        let mut report = Vec::new();
        let created = self.with_cas_retry(opts, |reads| {
            // SPEC §10.19: the target must be an unused profile
            // name — merging into an existing profile silently
            // mixes two jot populations.
            if reads.profiles.contains_key(&new_profile) {
                return Err(Error::ConstraintViolation(format!(
                    "profile {new_profile:?} already exists (promote target must be unused)"
                )));
            }
            self.create_profile(reads, &new_profile)?;
            Ok(true)
        })?;
        if created {
            report.push(format!("created profile {new_profile:?}"));
        }
        let ids: Vec<i64> = {
            let reads = self.reads(opts)?;
            reads
                .list(Some(&source_profile))
                .into_iter()
                .filter(|(e, _)| reads.effective(*e).categories.contains(&cat))
                .map(|(_, i)| i.id)
                .collect()
        };
        for id in ids {
            // mv reloads per move so counters see prior moves.
            match self.mv(opts, &source_profile, &new_profile, id) {
                Ok((old, new)) => report.push(format!(
                    "--{}--{} -> --{}--{}",
                    old.profile, old.id, new.profile, new.id
                )),
                Err(e) => {
                    report.push(format!("--{source_profile}--{id} failed: {e} (stopping)"));
                    break;
                }
            }
        }
        Ok(report)
    }

    // ── attach / detach (SPEC §10.22) ──────────────────────────────

    pub fn attach(&self, opts: &Opts, files: &[PathBuf], r: &RefArg) -> Result<Vec<String>> {
        self.require_no_at(opts, "attach")?;
        // Read and extract every input up front: a bad second file
        // must fail before the first file's commit, not after it —
        // a mid-batch abort would discard the report of what did
        // land. Extraction (SPEC §7.2) runs once per file, outside
        // the retry loop.
        let mut inputs = Vec::new();
        for file in files {
            let bytes = std::fs::read(file)?;
            let filename = file
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Usage(format!("bad filename: {}", file.display())))?
                .to_string();
            let extracted = self.extract_text(file, &filename)?;
            inputs.push((bytes, filename, extracted));
        }
        let mut report = Vec::new();
        for (bytes, filename, extracted) in inputs {
            // Per-file view reload: filename uniqueness and re-attach
            // detection must see prior commits.
            let line = self.with_cas_retry(opts, |reads| {
                let (entry, ident) = self.resolve_live(reads, r, opts)?;
                let ident = ident.clone();
                let existing = reads
                    .attachments_of(entry)
                    .into_iter()
                    .find(|(_, a)| a.filename == filename)
                    .map(|(e, _)| e);
                let att_entry = existing.unwrap_or_else(|| self.vault.new_entry_id());
                let blob = self.vault.deposit_blob(&bytes)?;
                let body = AttachmentBody {
                    jot: entry.hyphenated().to_string(),
                    filename: filename.clone(),
                    size: bytes.len() as u64,
                    added_at_ns: now_ns(),
                    detached: false,
                };
                let mime = mime_for_bytes(&filename, &bytes);
                let mut batch = vec![
                    to_fact(facts::K_CUJ_ATTACHMENT, att_entry, &body)?,
                    Fact::blob_ref(att_entry, &blob.to_hex(), bytes.len() as u64),
                    Fact::mime(att_entry, &mime),
                ];
                if existing.is_none() {
                    batch.push(Fact::path(
                        att_entry,
                        &att_path(&ident.profile, ident.id, &filename),
                        true,
                    ));
                }
                if let Some((tool, text)) = &extracted {
                    let xentry = self.vault.new_entry_id();
                    let xblob = self.vault.deposit_blob(text.as_bytes())?;
                    batch.push(to_fact(
                        facts::K_CUJ_EXTRACTION,
                        xentry,
                        &ExtractionBody {
                            of: att_entry.hyphenated().to_string(),
                            tool: tool.clone(),
                            tool_version: String::new(),
                        },
                    )?);
                    batch.push(Fact::blob_ref(xentry, &xblob.to_hex(), text.len() as u64));
                }
                self.commit_pinned(reads, &batch)?;
                Ok(format!(
                    "attached {filename} to --{}--{}{}",
                    ident.profile,
                    ident.id,
                    if existing.is_some() {
                        " (replaced)"
                    } else {
                        ""
                    }
                ))
            })?;
            report.push(line);
        }
        Ok(report)
    }

    fn extract_text(&self, file: &Path, filename: &str) -> Result<Option<(String, String)>> {
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        let Some(cmd_template) = self.client.extract.get(&ext) else {
            return Ok(None);
        };
        let out_dir = tempfile::Builder::new().prefix("cuj-extract-").tempdir()?;
        let out = out_dir.path().join(format!("{filename}.txt"));
        let cmd = cmd_template
            .replace("{in}", &shell_quote(file))
            .replace("{out}", &shell_quote(&out));
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .status();
        let text = match status {
            Ok(s) if s.success() => std::fs::read_to_string(&out).ok(),
            _ => None,
        };
        let tool = cmd_template
            .split_whitespace()
            .next()
            .unwrap_or("extract")
            .to_string();
        Ok(text.map(|t| (tool, t)))
    }

    pub fn detach(&self, opts: &Opts, filenames: &[String], r: &RefArg) -> Result<Vec<String>> {
        self.require_no_at(opts, "detach")?;
        let mut report = Vec::new();
        for filename in filenames {
            let line = self.with_cas_retry(opts, |reads| {
                let (entry, ident) = self.resolve_live(reads, r, opts)?;
                let ident = ident.clone();
                let Some((att_entry, att)) = reads
                    .attachments_of(entry)
                    .into_iter()
                    .find(|(_, a)| &a.filename == filename)
                else {
                    return Ok(format!("{filename}: not attached"));
                };
                let mut body = att.clone();
                body.detached = true;
                self.commit_pinned(
                    reads,
                    &[
                        to_fact(facts::K_CUJ_ATTACHMENT, att_entry, &body)?,
                        Fact::path(
                            att_entry,
                            &att_path(&ident.profile, ident.id, filename),
                            false,
                        ),
                    ],
                )?;
                Ok(format!("detached {filename}"))
            })?;
            report.push(line);
        }
        Ok(report)
    }

    // ── profile / config writes (SPEC §10.27, 10.29) ───────────────

    pub fn update_profile(&self, name: &str, f: impl Fn(&mut ProfileConfig)) -> Result<()> {
        self.with_cas_retry(&Opts::default(), |reads| {
            let entry = reads.profile_entry(name)?;
            let mut pf = reads.profile(name)?.clone();
            f(&mut pf);
            self.commit_pinned(reads, &[to_fact(facts::K_CUJ_PROFILE, entry, &pf)?])?;
            Ok(())
        })
    }

    // ── migrate (SPEC §10.37) ──────────────────────────────────────

    /// Move a 0.4 vault to protocol 0.5: one committed fact, the
    /// new marker on the root entry. Idempotent on a 0.5 vault.
    pub fn migrate(&self) -> Result<String> {
        self.with_cas_retry(&Opts::default(), |reads| {
            if reads.protocol.as_deref() == Some(facts::PROTOCOL) {
                return Ok(format!("already protocol {}", facts::PROTOCOL));
            }
            if reads.profiles.contains_key("space") {
                return Err(Error::Unsupported(
                    "a profile is named \"space\" (reserved in 0.5); \
                     mv its jots to another profile first"
                        .into(),
                ));
            }
            let (root_entry, _) = self
                .vault
                .current_facts(facts::K_CUJ_VAULT)?
                .into_iter()
                .next()
                .ok_or_else(|| Error::NotFound("no cuj/vault fact".into()))?;
            self.commit_pinned(reads, &[facts::vault_fact(root_entry)?])?;
            Ok(format!("migrated to protocol {}", facts::PROTOCOL))
        })
    }

    // ── file spaces (SPEC §spaces) ─────────────────────────────────

    /// Register a space, or re-emit an existing registration —
    /// the way to declare or remove a dialect on an existing
    /// space; the next observe reconciles the dialect facts
    /// (SPEC §op-space).
    pub fn space_add(&self, name: &str, dialect: Option<&str>) -> Result<&'static str> {
        facts::validate_short_name(name, "space name")?;
        if let Some(d) = dialect {
            facts::validate_dialect(d)?;
        }
        self.with_cas_retry(&Opts::default(), |reads| {
            reads.require_protocol("space add")?;
            let body = facts::SpaceBody {
                name: name.to_string(),
                dialect: dialect.map(String::from),
            };
            match reads.spaces.get(name) {
                Some((_, existing)) if *existing == body => Ok("unchanged"),
                Some((entry, _)) => {
                    self.commit_pinned(reads, &[to_fact(facts::K_CUJ_SPACE, *entry, &body)?])?;
                    Ok("updated")
                }
                None => {
                    let entry = self.vault.new_entry_id();
                    self.commit_pinned(
                        reads,
                        &[
                            to_fact(facts::K_CUJ_SPACE, entry, &body)?,
                            Fact::path(entry, &facts::space_registry_path(name), true),
                        ],
                    )?;
                    Ok("registered")
                }
            }
        })
    }

    /// Retract the registry path and every file path under the
    /// space's prefix, in one commit (SPEC §space registry).
    pub fn space_rm(&self, name: &str) -> Result<usize> {
        self.with_cas_retry(&Opts::default(), |reads| {
            reads.require_protocol("space rm")?;
            let (entry, _) = reads
                .spaces
                .get(name)
                .ok_or_else(|| Error::NotFound(format!("space {name:?}")))?;
            let mut batch = vec![Fact::path(*entry, &facts::space_registry_path(name), false)];
            let mut files = 0usize;
            for ((space, rel), fentry) in &reads.files {
                if space == name {
                    batch.push(Fact::path(
                        *fentry,
                        &facts::file_jot_path(space, rel),
                        false,
                    ));
                    files += 1;
                }
            }
            self.commit_pinned(reads, &batch)?;
            Ok(files)
        })
    }

    /// Snapshot a space's local directory into the vault (SPEC
    /// §observation): one commit carrying the whole delta.
    pub fn observe(&self, name: &str) -> Result<Vec<String>> {
        let dir = self.client.spaces.get(name).cloned().ok_or_else(|| {
            Error::NotFound(format!(
                "space {name:?} has no local directory on this machine \
                     (config set space.{name} <dir>)"
            ))
        })?;
        if !dir.is_dir() {
            return Err(Error::NotFound(format!(
                "space {name:?}: {} is not a directory",
                dir.display()
            )));
        }
        // Walk the tree once (paths + mtimes), outside the retry
        // loop; bytes are read one file at a time below.
        let mut found: Vec<(String, i64)> = Vec::new();
        walk_tree(&dir, &dir, &mut found)?;

        self.with_cas_retry(&Opts::default(), |reads| {
            reads.require_protocol("observe")?;
            if !reads.spaces.contains_key(name) {
                return Err(Error::NotFound(format!(
                    "space {name:?} is not registered (space add {name})"
                )));
            }
            // Change detection by blob hash — no old-content reads.
            let blob_of = &reads.blobs;
            // Files may vanish between walk and read; a vanished
            // file stays out of `seen` and retracts below.
            let vanished = |e: &Error| {
                matches!(e, Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
            };
            let mut batch = Vec::new();
            let mut report = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut reported = std::collections::HashSet::new();
            let mut new_entries: std::collections::HashMap<String, Uuid> =
                std::collections::HashMap::new();
            // Dialect reconciliation below needs md text; cache it
            // from this loop's read so both stages see one content.
            let mut md_text: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for (rel, mtime_ns) in &found {
                let path = dir.join(rel);
                let existing = reads.files.get(&(name.to_string(), rel.clone()));
                let ext = rel.rsplit('.').next().unwrap_or("");
                if facts::EXTS.contains(&ext) {
                    // Text files buffer whole — the jots/dialect
                    // parses need the full text; small by nature.
                    let bytes = match std::fs::read(&path) {
                        Ok(b) => b,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e.into()),
                    };
                    seen.insert(rel.clone());
                    if ext == "md" {
                        if let Ok(t) = std::str::from_utf8(&bytes) {
                            md_text.insert(rel.clone(), t.to_string());
                        }
                    }
                    let unchanged = existing
                        .is_some_and(|e| blob_of.get(e) == Some(&Hash::of(&bytes)));
                    if unchanged {
                        continue;
                    }
                    match existing {
                        Some(entry) => {
                            self.file_facts(
                                reads, &mut batch, *entry, name, rel, &bytes, *mtime_ns, false,
                            )?;
                            report.push(format!("updated  ///{name}/{rel}"));
                        }
                        None => {
                            let entry = self.vault.new_entry_id();
                            new_entries.insert(rel.clone(), entry);
                            self.file_facts(
                                reads, &mut batch, entry, name, rel, &bytes, *mtime_ns, true,
                            )?;
                            report.push(format!("added    ///{name}/{rel}"));
                        }
                    }
                } else {
                    // Binaries stream: memory bounded by the copy
                    // buffer, idempotent for unchanged content.
                    let (hash, size, mime) = match self.deposit_file_streaming(&path, rel) {
                        Ok(t) => t,
                        Err(e) if vanished(&e) => continue,
                        Err(e) => return Err(e),
                    };
                    seen.insert(rel.clone());
                    if existing.is_some_and(|e| blob_of.get(e) == Some(&hash)) {
                        continue;
                    }
                    let (entry, new) = match existing {
                        Some(e) => (*e, false),
                        None => {
                            let e = self.vault.new_entry_id();
                            new_entries.insert(rel.clone(), e);
                            (e, true)
                        }
                    };
                    batch.push(Fact::blob_ref(entry, &hash.to_hex(), size));
                    if new {
                        batch.push(Fact::path(entry, &facts::file_jot_path(name, rel), true));
                    }
                    batch.push(Fact::fs_metadata(
                        entry,
                        &FsMetadata {
                            mtime_ns: Some(*mtime_ns),
                            ..Default::default()
                        },
                        None,
                    ));
                    batch.push(Fact::mime(entry, &mime));
                    report.push(format!(
                        "{}  ///{name}/{rel}",
                        if new { "added  " } else { "updated" }
                    ));
                }
                reported.insert(rel.clone());
            }
            for ((space, rel), entry) in &reads.files {
                if space == name && !seen.contains(rel) {
                    batch.push(Fact::path(*entry, &facts::file_jot_path(space, rel), false));
                    report.push(format!("removed  ///{name}/{rel}"));
                }
            }
            // Dialect reconciliation (SPEC §observation): leave
            // every file entry's dialect facts equal to the
            // dialect parse of its current content — including
            // entries whose content did not change, when the
            // space's registered dialect changed since the last
            // run (newly declared: parse and emit; removed: emit
            // the empty snapshot).
            let dialect = reads
                .spaces
                .get(name)
                .and_then(|(_, s)| s.dialect.clone());
            let rel_set: std::collections::BTreeSet<String> =
                found.iter().map(|(r, _)| r.clone()).collect();
            for (rel, _) in &found {
                if !seen.contains(rel) {
                    continue; // vanished between walk and read
                }
                let entry = match (
                    new_entries.get(rel),
                    reads.files.get(&(name.to_string(), rel.clone())),
                ) {
                    (Some(e), _) => *e,
                    (None, Some(e)) => *e,
                    // A vault mutated between load and here (or a
                    // corrupted files projection) — error, don't
                    // panic mid-observe.
                    (None, None) => {
                        return Err(Error::Substrate(format!(
                            "observed file {rel:?} has no entry in space {name:?}"
                        )));
                    }
                };
                let expected = if dialect.as_deref() == Some("obsidian") {
                    // The text cached by the main loop, so both
                    // stages describe one read of the content.
                    md_text
                        .get(rel)
                        .map(|text| crate::obsidian::index_for(text, name, &rel_set))
                } else {
                    None
                };
                let stored = reads.obsidian.get(&entry);
                let emit = match (&expected, stored) {
                    (Some(e), Some(s)) => (!e.same_body(s)).then(|| e.clone()),
                    // Absent and empty are equivalent (SPEC
                    // §dialects): an empty parse emits nothing.
                    (Some(e), None) => (!e.is_empty()).then(|| e.clone()),
                    // Dialect removed: retract via the empty
                    // snapshot (SPEC §dialects).
                    (None, Some(s)) if !s.is_empty() => {
                        Some(crate::facts::ObsidianIndex::default())
                    }
                    (None, _) => None,
                };
                if let Some(body) = emit {
                    batch.push(to_fact(facts::K_OBSIDIAN_INDEX, entry, &body)?);
                    if !reported.contains(rel) {
                        report.push(format!("indexed  ///{name}/{rel}"));
                    }
                }
            }
            if !batch.is_empty() {
                self.commit_pinned(reads, &batch)?;
            }
            Ok(report)
        })
    }

    /// The fact set of one observed file (SPEC §file entries).
    #[allow(clippy::too_many_arguments)]
    /// Stream a file into the vault without buffering it whole:
    /// (blob hash, byte count, MIME sniffed from the head). The
    /// UTF-8 fallback of the MIME sniff sees only the head, which
    /// can misread a file whose tail is binary — acceptable for
    /// the non-EXTS files this serves.
    pub(crate) fn deposit_file_streaming(
        &self,
        path: &Path,
        display_name: &str,
    ) -> Result<(Hash, u64, String)> {
        use std::io::Read;
        let mut f = std::fs::File::open(path)?;
        let mut head = Vec::with_capacity(8192);
        f.by_ref().take(8192).read_to_end(&mut head)?;
        let mime = mime_for_bytes(display_name, &head);
        let mut counted = CountingReader {
            inner: std::io::Cursor::new(head).chain(f),
            n: 0,
        };
        let hash = self.vault.deposit_blob_from(&mut counted)?;
        Ok((hash, counted.n, mime))
    }

    #[allow(clippy::too_many_arguments)]
    fn file_facts(
        &self,
        reads: &Reads<'_>,
        batch: &mut Vec<Fact>,
        entry: Uuid,
        space: &str,
        rel: &str,
        bytes: &[u8],
        mtime_ns: i64,
        new: bool,
    ) -> Result<()> {
        let blob = self.vault.deposit_blob(bytes)?;
        batch.push(Fact::blob_ref(entry, &blob.to_hex(), bytes.len() as u64));
        if new {
            batch.push(Fact::path(entry, &facts::file_jot_path(space, rel), true));
        }
        batch.push(Fact::fs_metadata(
            entry,
            &FsMetadata {
                mtime_ns: Some(mtime_ns),
                ..Default::default()
            },
            None,
        ));
        batch.push(Fact::mime(entry, &mime_for_bytes(rel, bytes)));
        // Text files with jot extensions join classification and
        // reference semantics; bare --N refs have no containing
        // profile and stay unresolved (SPEC §file entries).
        let ext = rel.rsplit('.').next().unwrap_or("");
        if facts::EXTS.contains(&ext) {
            if let Ok(text) = std::str::from_utf8(bytes) {
                let result = binding::parse(text, &ProfileConfig::new("file"));
                let resolve = |p: &str, n: i64| reads.id_map.get(&(p.to_string(), n)).copied();
                let parsed = binding::to_parsed_index(&result, "", &resolve);
                batch.push(to_fact(facts::K_JOT_INDEX, entry, &parsed)?);
            }
        }
        Ok(())
    }

    // ── templates (SPEC §cuj/template) ─────────────────────────────

    pub fn template_set(&self, opts: &Opts, name: &str, text: &str) -> Result<()> {
        self.require_no_at(opts, "template set")?;
        facts::validate_short_name(name, "template name")?;
        self.with_cas_retry(opts, |reads| {
            reads.require_protocol("template set")?;
            let profile = opts.write_profile(&self.client)?.to_string();
            reads.profile(&profile)?;
            let ext = match &opts.ext_override {
                Some(e) => {
                    facts::validate_ext(e)?;
                    e.clone()
                }
                None => reads.profile(&profile)?.default_ext.clone(),
            };
            let key = (profile.clone(), name.to_string());
            let (entry, new) = match reads.templates.get(&key) {
                Some((e, _)) => (*e, false),
                None => (self.vault.new_entry_id(), true),
            };
            let blob = self.vault.deposit_blob(text.as_bytes())?;
            let mut batch = vec![
                to_fact(
                    facts::K_CUJ_TEMPLATE,
                    entry,
                    &facts::TemplateBody {
                        profile: profile.clone(),
                        name: name.to_string(),
                        ext,
                    },
                )?,
                Fact::blob_ref(entry, &blob.to_hex(), text.len() as u64),
            ];
            if new {
                batch.push(Fact::path(
                    entry,
                    &facts::template_path(&profile, name),
                    true,
                ));
            }
            self.commit_pinned(reads, &batch)?;
            Ok(())
        })
    }

    pub fn template_rm(&self, opts: &Opts, name: &str) -> Result<()> {
        self.require_no_at(opts, "template rm")?;
        self.with_cas_retry(opts, |reads| {
            reads.require_protocol("template rm")?;
            let profile = opts.write_profile(&self.client)?.to_string();
            let key = (profile.clone(), name.to_string());
            let (entry, _) = reads
                .templates
                .get(&key)
                .ok_or_else(|| Error::NotFound(format!("template {name:?}")))?;
            self.commit_pinned(
                reads,
                &[Fact::path(
                    *entry,
                    &facts::template_path(&profile, name),
                    false,
                )],
            )?;
            Ok(())
        })
    }

    /// Instantiate a template (SPEC §new): purely textual
    /// substitution, then the ordinary create path.
    pub fn new_from_template(&self, opts: &Opts, name: &str, text: &str) -> Result<Ident> {
        let (body, ext) = {
            let reads = self.reads(opts)?;
            let profile = opts.write_profile(&self.client)?.to_string();
            let (entry, t) = reads
                .templates
                .get(&(profile, name.to_string()))
                .ok_or_else(|| Error::NotFound(format!("template {name:?}")))?;
            (
                String::from_utf8_lossy(&reads.bytes(*entry)?).into_owned(),
                t.ext.clone(),
            )
        };
        // Local date and instant (SPEC §10.3); the offset keeps
        // the instant exact. Time placeholders expand before
        // {{text}} so a placeholder inside the argument text stays
        // literal.
        let now_local = chrono::Local::now();
        let date = now_local.format("%Y-%m-%d").to_string();
        let datetime = now_local.format("%Y-%m-%dT%H:%M:%S%:z").to_string();
        let had_text_slot = body.contains("{{text}}");
        let mut content = body
            .replace("{{date}}", &date)
            .replace("{{datetime}}", &datetime)
            .replace("{{text}}", text);
        if !had_text_slot && !text.trim().is_empty() {
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(text);
        }
        let mut opts = opts.clone();
        if opts.ext_override.is_none() {
            opts.ext_override = Some(ext);
        }
        self.create(&opts, &content)
    }

    // ── saved queries (SPEC §cuj/saved-query) ──────────────────────

    pub fn query_save(&self, opts: &Opts, name: &str, args: &[String]) -> Result<()> {
        self.require_no_at(opts, "query save")?;
        facts::validate_short_name(name, "query name")?;
        self.with_cas_retry(opts, |reads| {
            reads.require_protocol("query save")?;
            let profile = opts.write_profile(&self.client)?.to_string();
            reads.profile(&profile)?;
            let key = (profile.clone(), name.to_string());
            let (entry, new) = match reads.queries.get(&key) {
                Some((e, _)) => (*e, false),
                None => (self.vault.new_entry_id(), true),
            };
            let mut batch = vec![to_fact(
                facts::K_CUJ_SAVED_QUERY,
                entry,
                &facts::SavedQueryBody {
                    profile: profile.clone(),
                    name: name.to_string(),
                    args: args.to_vec(),
                },
            )?];
            if new {
                batch.push(Fact::path(entry, &facts::query_path(&profile, name), true));
            }
            self.commit_pinned(reads, &batch)?;
            Ok(())
        })
    }

    pub fn query_rm(&self, opts: &Opts, name: &str) -> Result<()> {
        self.require_no_at(opts, "query rm")?;
        self.with_cas_retry(opts, |reads| {
            reads.require_protocol("query rm")?;
            let profile = opts.write_profile(&self.client)?.to_string();
            let key = (profile.clone(), name.to_string());
            let (entry, _) = reads
                .queries
                .get(&key)
                .ok_or_else(|| Error::NotFound(format!("saved query {name:?}")))?;
            self.commit_pinned(
                reads,
                &[Fact::path(
                    *entry,
                    &facts::query_path(&profile, name),
                    false,
                )],
            )?;
            Ok(())
        })
    }

    // ── ingest (SPEC §inbox) ───────────────────────────────────────

    /// Drain the capture inbox: one jot per pending file, oldest
    /// mtime first, the file removed only after its commit lands.
    pub fn ingest(&self, opts: &Opts) -> Result<Vec<String>> {
        self.require_no_at(opts, "ingest")?;
        let inbox = self.root().join("inbox");
        let mut pending: Vec<(std::path::PathBuf, Option<String>, i64)> = Vec::new();
        if inbox.is_dir() {
            collect_inbox(&inbox, None, &mut pending)?;
        }
        pending.sort_by_key(|(_, _, mtime)| *mtime);

        let mut report = Vec::new();
        for (file, profile_dir, mtime_ns) in pending {
            let display = file
                .strip_prefix(&inbox)
                .unwrap_or(&file)
                .display()
                .to_string();
            let outcome = (|| -> Result<Ident> {
                let bytes = std::fs::read(&file)?;
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| Error::Usage("capture is not UTF-8".into()))?;
                if text.trim().is_empty() {
                    return Err(Error::Usage("capture is empty".into()));
                }
                let mut opts = opts.clone();
                if let Some(p) = &profile_dir {
                    opts.profile = Some(p.clone());
                }
                let ext = file
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if opts.ext_override.is_none() && facts::EXTS.contains(&ext.as_str()) {
                    opts.ext_override = Some(ext);
                }
                let ident = self.create_at(&opts, text, Some(mtime_ns))?;
                std::fs::remove_file(&file)?;
                Ok(ident)
            })();
            match outcome {
                Ok(ident) => report.push(format!("{display} -> --{}--{}", ident.profile, ident.id)),
                // Per-capture isolation (SPEC §ingest): the file
                // stays in the inbox.
                Err(e) => report.push(format!("{display}: {e}")),
            }
        }
        Ok(report)
    }

    // ── reindex (SPEC §10.30) ──────────────────────────────────────

    pub fn reindex(&self) -> Result<()> {
        // Recreate any chain a failed init left missing — reindex
        // is the recovery path back to working retrieval.
        ext::ensure_chains(self.root())?;
        let head = self.vault.current_state_hash()?;
        self.vault.reindex_at(&head)?;
        ext::open_taxopsis(self.root())?.reindex(None)?;
        ext::open_logopsis(self.root())?.reindex(None)?;
        ext::open_semopsis(self.root())?.reindex(None)?;
        // The fusion chain is pins over the two feeders just
        // brought to head — advance it too (SPEC §9.6: all
        // attached chains), so search --stale right after an
        // explicit reindex is not stale.
        ext::advance_zetetes(self.root())?;
        Ok(())
    }

    // ── reparse (SPEC §10.31) ──────────────────────────────────────

    /// Re-derive `jot/index` for every live jot and observed text
    /// file from current content — the schema-migration companion of
    /// `reindex`: after a cuj upgrade extends the parsed snapshot
    /// (edge annotations, resolved resource owners), one `reparse`
    /// brings an existing vault's facts up to the current shape.
    /// Idempotent: entries whose snapshot already matches are
    /// skipped (parser-version-only differences included), so a
    /// second run commits nothing. As a side effect, references
    /// that were unresolved at write time re-resolve against
    /// current identities. Returns the number of refreshed facts.
    pub fn reparse(&self) -> Result<usize> {
        self.with_cas_retry(&Opts::default(), |reads| {
            // References resolve at write time and the entry is
            // durable (SPEC §3.4). Re-deriving from raw content
            // must not sever an edge whose written identity has
            // since moved — fall back to the historical cuj/id
            // record when the current map has no holder. Counters
            // never reuse an id, so each (profile, id) has held at
            // most one entry.
            let mut historical: std::collections::HashMap<(String, i64), Uuid> =
                std::collections::HashMap::new();
            for (e, f, _) in self.vault.facts_by_kind(facts::K_CUJ_ID)? {
                if let Some(i) = facts::from_body::<Ident>(&f.body) {
                    historical.entry((i.profile, i.id)).or_insert(e);
                }
            }
            let resolve = |p: &str, n: i64| {
                let key = (p.to_string(), n);
                reads
                    .id_map
                    .get(&key)
                    .copied()
                    .or_else(|| historical.get(&key).copied())
            };
            let mut batch = Vec::new();
            for (entry, ident) in reads.list(None) {
                let profile = reads.profile(&ident.profile)?.clone();
                let bytes = reads.bytes(entry)?;
                let content = String::from_utf8_lossy(&bytes).into_owned();
                let result = binding::parse(&content, &profile);
                let parsed = binding::to_parsed_index(&result, &ident.profile, &resolve);
                if reads.parsed.get(&entry).is_none_or(|old| !old.same_body(&parsed)) {
                    batch.push(to_fact(facts::K_JOT_INDEX, entry, &parsed)?);
                }
            }
            // Observed files: the same EXTS gate and profile-less
            // parse as file_facts (SPEC §file entries).
            for ((_space, rel), entry) in &reads.files {
                let ext = rel.rsplit('.').next().unwrap_or("");
                if !facts::EXTS.contains(&ext) {
                    continue;
                }
                let Ok(bytes) = reads.bytes(*entry) else {
                    continue;
                };
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    continue;
                };
                let result = binding::parse(text, &ProfileConfig::new("file"));
                let parsed = binding::to_parsed_index(&result, "", &resolve);
                if reads.parsed.get(entry).is_none_or(|old| !old.same_body(&parsed)) {
                    batch.push(to_fact(facts::K_JOT_INDEX, *entry, &parsed)?);
                }
            }
            let n = batch.len();
            if n > 0 {
                self.commit_pinned(reads, &batch)?;
            }
            Ok(n)
        })
    }
}

struct CountingReader<R> {
    inner: R,
    n: u64,
}

impl<R: std::io::Read> std::io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n += k as u64;
        Ok(k)
    }
}

pub fn shell_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Walk a directory tree (observe, directory import): regular files,
/// dot-prefixed names skipped, relative slash-paths.
pub(crate) fn walk_tree(root: &Path, dir: &Path, out: &mut Vec<(String, i64)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta = entry.metadata()?;
        if meta.is_dir() {
            walk_tree(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("walk stays under root")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or_else(now_ns);
            // Paths and mtimes only: consumers read (or stream)
            // each file at the point of use, so memory is bounded
            // by one file, not the tree.
            out.push((rel, mtime_ns));
        }
    }
    Ok(())
}

/// Collect pending inbox captures (SPEC §inbox): files directly
/// under inbox/ target the active profile, files under
/// inbox/<dir>/ target the profile named by the first segment.
fn collect_inbox(
    dir: &Path,
    profile: Option<&str>,
    out: &mut Vec<(PathBuf, Option<String>, i64)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Captures may vanish between read_dir and here (a file
        // synchronizer, a concurrent ingest) — skip, don't abort
        // the whole scan (SPEC §4.5).
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if profile.is_none() {
                collect_inbox(&path, Some(name), out)?;
            }
        } else if meta.is_file() {
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or_else(now_ns);
            out.push((path, profile.map(String::from), mtime_ns));
        }
    }
    Ok(())
}

pub mod either {
    use crate::facts::Ident;

    pub enum TodoOutcome {
        Created(Ident),
        Applied(Vec<Ident>),
    }
}
