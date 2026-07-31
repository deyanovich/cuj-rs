//! `cuj import` — Evernote `.enex` import.
//!
//! Two passes over the given files: a cheap metadata scan
//! (timestamps, fingerprints, dry-run data), then the actual import
//! in ascending `<created>` order so assigned jot IDs are
//! chronological across all files. Each note lands as ONE vault
//! commit — jot facts, overlay, attachments, extractions, import
//! provenance, and the profile counter — so an interrupted run
//! leaves no partial notes and resumes via duplicate detection.

pub mod enex;
pub mod enml;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use metatheca::{Fact, FsMetadata, Hash, Uuid};

use crate::app::{App, CAS_RETRIES, Opts, att_path, jot_path};
use crate::binding::{self, mime_for_bytes, mime_for_ext};
use crate::error::{Error, Result};
use crate::facts::{
    self, AppliedOverlay, AttachmentBody, Birth, CounterBody, EdgeBody, ExtractionBody, Ident,
    ImportBody, ProfileConfig, check_constraints, now_ns, to_fact,
};
use crate::reads::Reads;

use enml::{EnmlContext, MediaRef};

pub const EXTRACTION_TOOL: &str = "evernote-recognition";

#[derive(Debug, Default)]
pub struct ImportArgs {
    pub files: Vec<PathBuf>,
    /// Lowercased category paths without the `::` prefix.
    pub categories: Vec<String>,
    pub dry_run: bool,
    pub freeze_todos: bool,
    /// Directory-binding choice (SPEC §op-import): `None`
    /// auto-detects (a root `.obsidian/` selects the Obsidian
    /// binding), `Some("obsidian")` / `Some("none")` force it
    /// either way.
    pub dialect: Option<String>,
}

/// Validate and normalize a `--category ::name::sub` argument to
/// the stored (lowercase, prefixless) form.
pub fn parse_category(tok: &str) -> Result<String> {
    let bad = || {
        Error::Usage(format!(
            "--category takes ::name or ::name::sub (word chars only), got {tok:?}"
        ))
    };
    let rest = tok.strip_prefix("::").ok_or_else(bad)?.to_lowercase();
    let ok = !rest.is_empty()
        && rest.split("::").all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
    if ok { Ok(rest) } else { Err(bad()) }
}

#[derive(Debug, Default)]
pub struct ImportedNote {
    /// None under --dry-run.
    pub id: Option<i64>,
    pub profile: String,
    pub title: String,
    pub file: String,
    pub tags: Vec<String>,
    pub categories: Vec<String>,
    pub attachments: Vec<String>,
    pub created_at_ns: i64,
    /// Obsidian binding: overlay references resolved from
    /// wikilinks, in their written `profile:id` form.
    pub references: Vec<String>,
    /// Obsidian binding: wikilink/embed targets that did not
    /// resolve within the import (SPEC §op-import: reported,
    /// remain inert text).
    pub unresolved: Vec<String>,
    /// Obsidian binding: frontmatter carried no overlay slot and
    /// was dropped (property keys, `aliases`).
    pub dropped: Vec<String>,
}

#[derive(Debug)]
pub struct SkippedNote {
    pub title: String,
    pub file: String,
    /// `--profile--id` of the already-imported jot, when it still
    /// resolves (the fingerprint outlives rm/mv by design).
    pub existing: Option<String>,
}

#[derive(Debug)]
pub struct ImportError {
    /// None for file-level failures.
    pub title: Option<String>,
    pub file: String,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub imported: Vec<ImportedNote>,
    pub skipped: Vec<SkippedNote>,
    pub errors: Vec<ImportError>,
    pub dry_run: bool,
}

/// Pass-1 note record; everything the sort, dedup check, and
/// dry-run output need.
struct NoteMeta {
    file_idx: usize,
    note_idx: usize,
    created_ns: i64,
    fingerprint: String,
    legacy_fingerprint: String,
    title: String,
    tags: Vec<String>,
    attachment_names: Vec<String>,
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

pub fn run(app: &App, opts: &Opts, args: &ImportArgs) -> Result<ImportReport> {
    app.require_no_at(opts, "import")?;
    if args.files.is_empty() {
        return Err(Error::Usage(
            "import [--category ::cat]... [--dry-run] [--freeze-todos] \
             <file.enex>... | <dir>"
                .into(),
        ));
    }
    // The directory bindings (SPEC §op-import): one directory
    // argument, walked for jot-extension files; a root
    // `.obsidian/` selects the Obsidian binding unless --dialect
    // forces the choice.
    if args.files.iter().any(|f| f.is_dir()) {
        if args.files.len() != 1 {
            return Err(Error::Usage(
                "directory import takes exactly one directory".into(),
            ));
        }
        let dir = &args.files[0];
        let obsidian = match args.dialect.as_deref() {
            Some("obsidian") => true,
            Some("none") => false,
            Some(other) => {
                return Err(Error::Usage(format!(
                    "--dialect takes obsidian or none, got {other:?}"
                )));
            }
            None => dir.join(".obsidian").is_dir(),
        };
        return run_dir(app, opts, args, dir, obsidian);
    }
    if args.dialect.is_some() {
        return Err(Error::Usage(
            "--dialect applies to directory import only".into(),
        ));
    }
    if opts.ext_override.is_some() {
        return Err(Error::Usage(
            "import always writes .html jots; drop --md/--txt/--tex/--html".into(),
        ));
    }
    for c in &args.categories {
        // Categories arrive normalized from parse_category; guard
        // library callers constructing ImportArgs directly.
        parse_category(&format!("::{c}"))?;
    }
    for f in &args.files {
        std::fs::File::open(f)
            .map_err(|e| Error::Usage(format!("cannot read {}: {e}", f.display())))?;
    }

    let profile_name = opts.write_profile(&app.client)?.to_string();
    let mut reads = app.reads(opts)?;
    let mut profile = reads.profile(&profile_name)?.clone();
    let mut profile_entry = reads.profile_entry(&profile_name)?;
    // The commit pin (SPEC §3.2): threaded through the per-note
    // commits so an external writer's interleaved commit surfaces
    // as Conflict and triggers a reread instead of a silent rebase.
    let mut parent = reads.head;

    let mut report = ImportReport {
        dry_run: args.dry_run,
        ..Default::default()
    };

    // ── pass 1: metadata scan ──────────────────────────────────────
    let mut metas: Vec<NoteMeta> = Vec::new();
    for (fi, path) in args.files.iter().enumerate() {
        match enex::parse_enex_file(path, false) {
            Ok(notes) => {
                if !opts.json {
                    println!("Processing {} ({} notes)...", basename(path), notes.len());
                }
                for (ni, n) in notes.iter().enumerate() {
                    if n.created_ns.is_none() {
                        eprintln!(
                            "warning: {}: note {:?} has no parsable <created>; \
                             using import time",
                            basename(path),
                            n.title
                        );
                    }
                    metas.push(NoteMeta {
                        file_idx: fi,
                        note_idx: ni,
                        created_ns: n.created_ns.unwrap_or_else(now_ns),
                        fingerprint: n.fingerprint.clone(),
                        legacy_fingerprint: n.legacy_fingerprint.clone(),
                        title: n.title.clone(),
                        tags: n
                            .tags
                            .iter()
                            .filter_map(|t| enml::sanitize_tag(t))
                            .collect(),
                        attachment_names: enml::assign_filenames(&n.resources),
                    });
                }
            }
            Err(e) => report.errors.push(ImportError {
                title: None,
                file: basename(path),
                error: e.to_string(),
            }),
        }
    }

    // ── dedup: fingerprint -> existing display name ────────────────
    // Stored facts may carry the legacy (pre-tags/resources)
    // fingerprint form; a note is a duplicate if either form
    // matches. New imports store the extended form.
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    for (entry, fact) in app.vault.current_facts(facts::K_CUJ_IMPORT)? {
        if let Some(b) = facts::from_body::<ImportBody>(&fact.body) {
            let disp = reads
                .idents
                .get(&entry)
                .filter(|_| reads.is_live(entry))
                .map(|i| format!("--{}--{}", i.profile, i.id));
            seen.insert(b.fingerprint, disp);
        }
    }

    // ── chronological order across all files ──────────────────────
    metas.sort_by(|a, b| {
        (a.created_ns, a.file_idx, a.note_idx).cmp(&(b.created_ns, b.file_idx, b.note_idx))
    });

    if args.dry_run {
        for m in metas {
            let file = basename(&args.files[m.file_idx]);
            let existing = seen
                .get(&m.fingerprint)
                .or_else(|| seen.get(&m.legacy_fingerprint))
                .cloned();
            if let Some(existing) = existing {
                skip(&mut report, opts, m.title, file, existing);
            } else {
                seen.insert(m.fingerprint.clone(), None);
                if !opts.json {
                    println!(
                        "Would import: {:?} (tags: {}, {} attachment(s))",
                        m.title,
                        if m.tags.is_empty() {
                            "-".to_string()
                        } else {
                            m.tags.join(", ")
                        },
                        m.attachment_names.len()
                    );
                }
                report.imported.push(ImportedNote {
                    id: None,
                    profile: profile_name.clone(),
                    title: m.title,
                    file,
                    tags: m.tags,
                    categories: args.categories.clone(),
                    attachments: m.attachment_names,
                    created_at_ns: m.created_ns,
                    ..Default::default()
                });
            }
        }
        return Ok(report);
    }

    // ── pass 2: import in chronological order ──────────────────────
    let mut next_id = reads.next_id(&profile_name)?;
    // One decoded file at a time; notes leave the cache as consumed.
    let mut cache: Option<(usize, Vec<Option<enex::EnexNote>>)> = None;
    for m in metas {
        let file = basename(&args.files[m.file_idx]);
        let existing = seen
            .get(&m.fingerprint)
            .or_else(|| seen.get(&m.legacy_fingerprint))
            .cloned();
        if let Some(existing) = existing {
            skip(&mut report, opts, m.title, file, existing);
            continue;
        }
        if cache.as_ref().map(|(fi, _)| *fi) != Some(m.file_idx) {
            match enex::parse_enex_file(&args.files[m.file_idx], true) {
                Ok(notes) => {
                    cache = Some((m.file_idx, notes.into_iter().map(Some).collect()));
                }
                Err(e) => {
                    report.errors.push(ImportError {
                        title: None,
                        file: file.clone(),
                        error: format!("re-parse failed: {e}"),
                    });
                    cache = Some((m.file_idx, Vec::new()));
                }
            }
        }
        let note = cache
            .as_mut()
            .and_then(|(_, notes)| notes.get_mut(m.note_idx))
            .and_then(Option::take)
            .filter(|n| n.fingerprint == m.fingerprint);
        let Some(note) = note else {
            report.errors.push(ImportError {
                title: Some(m.title),
                file,
                error: "note changed or vanished between scan and import".into(),
            });
            continue;
        };

        // Bounded reread-and-retry on a lost CAS race (SPEC §3.2):
        // an external writer may have consumed the next ID.
        let mut attempts = 0usize;
        let outcome = loop {
            let id = next_id;
            match import_note(
                app,
                opts,
                &reads,
                &profile,
                &profile_name,
                profile_entry,
                id,
                &note,
                args,
                &file,
                &parent,
            ) {
                Err(Error::Conflict) if attempts < CAS_RETRIES => {
                    attempts += 1;
                    reads = app.reads(opts)?;
                    parent = reads.head;
                    profile = reads.profile(&profile_name)?.clone();
                    profile_entry = reads.profile_entry(&profile_name)?;
                    next_id = reads.next_id(&profile_name)?;
                }
                other => break other,
            }
        };
        let id = next_id;
        match outcome {
            Ok((imported, new_head)) => {
                parent = new_head;
                next_id += 1;
                seen.insert(
                    m.fingerprint.clone(),
                    Some(format!("--{profile_name}--{id}")),
                );
                if !opts.json {
                    println!("Imported: {:?} -> --{profile_name}--{id}", imported.title);
                }
                report.imported.push(imported);
            }
            Err(e) => {
                if !opts.json {
                    println!("Error importing {:?}: {e}", m.title);
                }
                report.errors.push(ImportError {
                    title: Some(m.title),
                    file,
                    error: e.to_string(),
                });
            }
        }
    }
    Ok(report)
}

/// The notes-directory and Obsidian-vault bindings (SPEC
/// §op-import): each jot-extension file becomes one jot, content
/// verbatim, ext preserved, created from mtime, fingerprint over
/// `relpath \0 content`. The Obsidian binding adds the dialect
/// parse, applied at import time: tags and categories join the
/// overlay, wikilinks between imported notes become overlay
/// references (resolved after every note has an entry — two
/// passes), embeds of non-jot files become attachments, checkbox
/// tasks become overlay todos.
fn run_dir(
    app: &App,
    opts: &Opts,
    args: &ImportArgs,
    dir: &Path,
    obsidian: bool,
) -> Result<ImportReport> {
    use sha2::{Digest, Sha256};

    let profile_name = opts.write_profile(&app.client)?.to_string();
    let mut reads = app.reads(opts)?;
    let mut profile = reads.profile(&profile_name)?.clone();
    let mut profile_entry = reads.profile_entry(&profile_name)?;
    let mut parent = reads.head;

    let mut report = ImportReport {
        dry_run: args.dry_run,
        ..Default::default()
    };

    // Paths + mtimes only (memory bounded by one file): note
    // bytes are read per note below, embed bytes stream at attach.
    let mut found: Vec<(String, i64)> = Vec::new();
    crate::app::walk_tree(dir, dir, &mut found)?;
    let is_note = |rel: &str| facts::EXTS.contains(&rel.rsplit('.').next().unwrap_or(""));
    // The Obsidian binding resolves wikilinks and embeds against
    // the whole tree (SPEC §obsidian/index resolution rule).
    let all_files: BTreeSet<String> = if obsidian {
        found.iter().map(|(r, _)| r.clone()).collect()
    } else {
        BTreeSet::new()
    };
    found.retain(|(rel, _)| is_note(rel));
    found.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));

    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    let mut seen_entry: HashMap<String, Uuid> = HashMap::new();
    for (entry, fact) in app.vault.current_facts(facts::K_CUJ_IMPORT)? {
        if let Some(b) = facts::from_body::<ImportBody>(&fact.body) {
            let disp = reads
                .idents
                .get(&entry)
                .filter(|_| reads.is_live(entry))
                .map(|i| format!("--{}--{}", i.profile, i.id));
            if disp.is_some() {
                seen_entry.insert(b.fingerprint.clone(), entry);
            }
            seen.insert(b.fingerprint, disp);
        }
    }

    // rel path -> (entry, written form) of every note this run
    // touched — imported now, or already imported and skipped —
    // the link-resolution map of pass two.
    let mut rel_entry: HashMap<String, (Uuid, String)> = HashMap::new();
    // Notes imported this run that carry link targets, resolved
    // after every note has an entry.
    let mut pending_links: Vec<PendingLinks> = Vec::new();

    let mut next_id = reads.next_id(&profile_name)?;
    for (rel, mtime_ns) in found {
        let bytes = match std::fs::read(dir.join(&rel)) {
            Ok(b) => b,
            Err(e) => {
                report.errors.push(ImportError {
                    title: Some(rel.clone()),
                    file: rel,
                    error: e.to_string(),
                });
                continue;
            }
        };
        let mut hasher = Sha256::new();
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        let fingerprint = format!("{:x}", hasher.finalize());
        if let Some(existing) = seen.get(&fingerprint) {
            if let (Some(entry), Some(disp)) = (seen_entry.get(&fingerprint), existing) {
                let written = disp.trim_start_matches("--").replacen("--", ":", 1);
                rel_entry.insert(rel.clone(), (*entry, written));
            }
            skip(&mut report, opts, rel.clone(), rel, existing.clone());
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            report.errors.push(ImportError {
                title: Some(rel.clone()),
                file: rel,
                error: "not UTF-8".into(),
            });
            continue;
        };
        // The dialect parse (Obsidian binding, md notes only).
        let extras = (obsidian && rel.ends_with(".md"))
            .then(|| obsidian_extras(text, args, &all_files));
        if args.dry_run {
            seen.insert(fingerprint, None);
            if !opts.json {
                println!("Would import: {rel:?}");
            }
            report.imported.push(ImportedNote {
                id: None,
                profile: profile_name.clone(),
                title: rel.clone(),
                file: rel,
                tags: extras.as_ref().map(|x| x.tags.clone()).unwrap_or_default(),
                categories: args.categories.clone(),
                created_at_ns: mtime_ns,
                ..Default::default()
            });
            continue;
        }

        let mut attempts = 0usize;
        let outcome = loop {
            let id = next_id;
            match import_file(
                app,
                opts,
                &reads,
                &profile,
                &profile_name,
                profile_entry,
                id,
                dir,
                &rel,
                text,
                mtime_ns,
                &fingerprint,
                args,
                extras.as_ref(),
                &parent,
            ) {
                Err(Error::Conflict) if attempts < CAS_RETRIES => {
                    attempts += 1;
                    reads = app.reads(opts)?;
                    parent = reads.head;
                    profile = reads.profile(&profile_name)?.clone();
                    profile_entry = reads.profile_entry(&profile_name)?;
                    next_id = reads.next_id(&profile_name)?;
                }
                other => break other,
            }
        };
        let id = next_id;
        match outcome {
            Ok((imported, new_head, entry, overlay)) => {
                parent = new_head;
                next_id += 1;
                seen.insert(fingerprint, Some(format!("--{profile_name}--{id}")));
                rel_entry.insert(rel.clone(), (entry, format!("{profile_name}:{id}")));
                if let Some(x) = extras {
                    if !x.link_targets.is_empty() || !x.unresolved.is_empty() {
                        pending_links.push(PendingLinks {
                            rel: rel.clone(),
                            entry,
                            overlay,
                            link_targets: x.link_targets,
                            unresolved: x.unresolved,
                            report_idx: report.imported.len(),
                        });
                    }
                }
                if !opts.json {
                    println!("Imported: {:?} -> --{profile_name}--{id}", imported.title);
                    if !imported.dropped.is_empty() {
                        println!(
                            "Dropped in {:?}: {}",
                            imported.title,
                            imported.dropped.join(", ")
                        );
                    }
                }
                report.imported.push(imported);
            }
            Err(e) => {
                if !opts.json {
                    println!("Error importing {rel:?}: {e}");
                }
                report.errors.push(ImportError {
                    title: Some(rel.clone()),
                    file: rel,
                    error: e.to_string(),
                });
            }
        }
    }

    // ── pass two (Obsidian binding): wikilinks between imported
    // notes become overlay references, now that every note has an
    // entry (SPEC §op-import).
    let notes: BTreeSet<String> = all_files
        .iter()
        .filter(|r| is_note(r))
        .cloned()
        .collect();
    for p in pending_links {
        let mut refs: Vec<EdgeBody> = Vec::new();
        let mut unresolved = p.unresolved;
        for raw in p.link_targets {
            let target = crate::obsidian::resolve_target(&raw, &notes)
                .and_then(|path| rel_entry.get(&path).cloned());
            match target {
                Some((entry, written)) => refs.push(EdgeBody {
                    entry: entry.hyphenated().to_string(),
                    written_as: written,
                    annot: facts::EdgeAnnot::default(),
                }),
                None => unresolved.push(raw),
            }
        }
        if !refs.is_empty() {
            refs.sort_by(|a, b| a.entry.cmp(&b.entry));
            refs.dedup_by(|a, b| a.entry == b.entry);
            let mut overlay = p.overlay;
            overlay.references = refs.clone();
            let fact = to_fact(facts::K_CUJ_APPLIED, p.entry, &overlay)?;
            let mut attempts = 0usize;
            loop {
                match app.vault.commit_onto(std::slice::from_ref(&fact), &parent) {
                    Ok(new_head) => {
                        parent = new_head;
                        break;
                    }
                    Err(metatheca::Error::Conflict) if attempts < CAS_RETRIES => {
                        attempts += 1;
                        parent = app.vault.current_state_hash()?;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }
        if let Some(n) = report.imported.get_mut(p.report_idx) {
            n.references = refs.iter().map(|r| r.written_as.clone()).collect();
            if !unresolved.is_empty() {
                if !opts.json {
                    println!(
                        "Unresolved in {:?}: {}",
                        p.rel,
                        unresolved.join(", ")
                    );
                }
                n.unresolved = unresolved;
            }
        }
    }
    Ok(report)
}

/// Pass-one record of a note whose wikilinks await resolution.
struct PendingLinks {
    rel: String,
    entry: Uuid,
    overlay: AppliedOverlay,
    link_targets: Vec<String>,
    unresolved: Vec<String>,
    report_idx: usize,
}

/// The Obsidian dialect parse of one note, shaped for import
/// (SPEC §op-import): overlay tags/categories/todos, embed
/// attachments, dropped frontmatter, raw link targets for pass
/// two.
struct ObsidianExtras {
    tags: Vec<String>,
    categories: Vec<String>,
    todos: Vec<facts::TodoBody>,
    /// (filename, tree-relative source path) of resolved
    /// non-note embeds; bytes stream at attach time.
    attachments: Vec<(String, String)>,
    link_targets: Vec<String>,
    unresolved: Vec<String>,
    dropped: Vec<String>,
}

fn obsidian_extras(text: &str, args: &ImportArgs, all_files: &BTreeSet<String>) -> ObsidianExtras {
    let is_note = |rel: &str| facts::EXTS.contains(&rel.rsplit('.').next().unwrap_or(""));
    let p = crate::obsidian::parse(text);
    let mut x = ObsidianExtras {
        tags: p.tags,
        categories: p.categories,
        todos: Vec::new(),
        attachments: Vec::new(),
        link_targets: p.link_targets,
        unresolved: Vec::new(),
        dropped: Vec::new(),
    };
    if !args.freeze_todos {
        x.todos = p
            .tasks
            .into_iter()
            .map(|t| facts::TodoBody {
                raw: t.raw.clone(),
                action: None,
                description: t.raw,
                importance: 0,
                due: None,
                done: t.done,
                completed: None,
            })
            .collect();
    }
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw in p.embed_targets {
        match crate::obsidian::resolve_target(&raw, all_files) {
            // Embedded notes are links (their files import as
            // jots); non-note embeds become attachments. Basenames
            // go through the same filename rules as ENEX resources
            // (metatheca path facts require NFC, which the
            // sanitizer guarantees) and dedup within the note with
            // a numeric suffix before the extension.
            Some(path) if !is_note(&path) => {
                let rawname = path.rsplit('/').next().unwrap_or(&path);
                let base = enml::sanitize_filename(rawname).unwrap_or_else(|| {
                    let n = x.attachments.len() + 1;
                    match rawname
                        .rsplit_once('.')
                        .and_then(|(_, e)| enml::sanitize_filename(e))
                    {
                        Some(ext) => format!("embed_{n}.{ext}"),
                        None => format!("embed_{n}"),
                    }
                });
                let mut fname = base.clone();
                let mut k = 1usize;
                while used.contains(&fname) {
                    k += 1;
                    fname = match base.rsplit_once('.') {
                        Some((stem, ext)) => format!("{stem}_{k}.{ext}"),
                        None => format!("{base}_{k}"),
                    };
                }
                used.insert(fname.clone());
                x.attachments.push((fname, path));
            }
            Some(_) => x.link_targets.push(raw),
            None => x.unresolved.push(raw),
        }
    }
    // Frontmatter properties and aliases have no overlay slot
    // (SPEC §op-import): dropped, reported per note.
    for key in p.properties.keys() {
        x.dropped.push(format!("property {key}"));
    }
    if !p.aliases.is_empty() {
        x.dropped.push(format!("aliases ({})", p.aliases.len()));
    }
    x
}

/// One directory-import note: the create fact set plus cuj/import
/// provenance, one pinned commit. The Obsidian binding threads
/// its dialect parse in as `extras` (SPEC §op-import).
#[allow(clippy::too_many_arguments)]
fn import_file(
    app: &App,
    opts: &Opts,
    reads: &Reads<'_>,
    profile: &ProfileConfig,
    profile_name: &str,
    profile_entry: metatheca::Uuid,
    id: i64,
    dir: &Path,
    rel: &str,
    text: &str,
    created_ns: i64,
    fingerprint: &str,
    args: &ImportArgs,
    extras: Option<&ObsidianExtras>,
    parent: &Hash,
) -> Result<(ImportedNote, Hash, Uuid, AppliedOverlay)> {
    let ext = rel.rsplit('.').next().unwrap_or("md").to_string();
    let content = if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    };
    let parsed = app.parse_index(reads, profile, profile_name, &content);

    let mut overlay = AppliedOverlay {
        categories: args.categories.clone(),
        ..Default::default()
    };
    if let Some(x) = extras {
        overlay.tags.extend(x.tags.iter().cloned());
        overlay.categories.extend(x.categories.iter().cloned());
        overlay.todos.extend(x.todos.iter().cloned());
    }
    overlay.tags.sort();
    overlay.tags.dedup();
    overlay.categories.sort();
    overlay.categories.dedup();
    let has_overlay = !overlay.tags.is_empty()
        || !overlay.categories.is_empty()
        || !overlay.todos.is_empty();

    let eff = facts::effective_index(Some(&parsed), Some(&overlay));
    check_constraints(profile, &eff, opts.force)?;

    let ident = Ident {
        profile: profile_name.to_string(),
        id,
        ext: ext.clone(),
    };
    let birth = Birth {
        profile: ident.profile.clone(),
        id,
        ext,
        created_at_ns: created_ns,
    };
    let entry = app.vault.new_entry_id();
    let blob = app.vault.deposit_blob(content.as_bytes())?;

    let mut batch = vec![
        to_fact(facts::K_CUJ_JOT, entry, &birth)?,
        to_fact(facts::K_CUJ_ID, entry, &ident)?,
        to_fact(facts::K_JOT_INDEX, entry, &parsed)?,
        Fact::blob_ref(entry, &blob.to_hex(), content.len() as u64),
        Fact::path(entry, &jot_path(&ident), true),
        Fact::fs_metadata(
            entry,
            &FsMetadata {
                birthtime_ns: Some(created_ns),
                mtime_ns: Some(created_ns),
                ..Default::default()
            },
            None,
        ),
        Fact::mime(entry, mime_for_ext(&ident.ext)),
        to_fact(
            facts::K_CUJ_IMPORT,
            entry,
            &ImportBody {
                fingerprint: fingerprint.to_string(),
                source: rel.to_string(),
                imported_at_ns: now_ns(),
                updated_at_ns: Some(created_ns),
            },
        )?,
    ];
    if has_overlay {
        batch.push(to_fact(facts::K_CUJ_APPLIED, entry, &overlay)?);
    }
    // Resolved non-note embeds become attachments on the
    // referencing jot (Obsidian binding, SPEC §op-import).
    let mut attachment_names = Vec::new();
    if let Some(x) = extras {
        for (fname, src_rel) in &x.attachments {
            let att_entry = app.vault.new_entry_id();
            // Embed bytes stream from the tree, one at a time.
            let (ablob, size, mime) =
                app.deposit_file_streaming(&dir.join(src_rel), fname)?;
            batch.push(to_fact(
                facts::K_CUJ_ATTACHMENT,
                att_entry,
                &AttachmentBody {
                    jot: entry.hyphenated().to_string(),
                    filename: fname.clone(),
                    size,
                    added_at_ns: now_ns(),
                    detached: false,
                },
            )?);
            batch.push(Fact::blob_ref(att_entry, &ablob.to_hex(), size));
            batch.push(Fact::mime(att_entry, &mime));
            batch.push(Fact::path(
                att_entry,
                &att_path(profile_name, id, fname),
                true,
            ));
            attachment_names.push(fname.clone());
        }
    }
    batch.push(to_fact(
        facts::K_CUJ_COUNTER,
        profile_entry,
        &CounterBody { value: id },
    )?);
    let new_head = app.vault.commit_onto(&batch, parent)?;

    Ok((
        ImportedNote {
            id: Some(id),
            profile: profile_name.to_string(),
            title: rel.to_string(),
            file: rel.to_string(),
            tags: overlay.tags.clone(),
            categories: overlay.categories.clone(),
            attachments: attachment_names,
            created_at_ns: created_ns,
            dropped: extras.map(|x| x.dropped.clone()).unwrap_or_default(),
            ..Default::default()
        },
        new_head,
        entry,
        overlay,
    ))
}

fn skip(
    report: &mut ImportReport,
    opts: &Opts,
    title: String,
    file: String,
    existing: Option<String>,
) {
    if !opts.json {
        match &existing {
            Some(e) => println!("Skipping duplicate: {title:?} (already {e})"),
            None => println!("Skipping duplicate: {title:?} (already imported)"),
        }
    }
    report.skipped.push(SkippedNote {
        title,
        file,
        existing,
    });
}

/// Build and commit the one-batch fact set for a single note.
#[allow(clippy::too_many_arguments)]
fn import_note(
    app: &App,
    opts: &Opts,
    reads: &Reads<'_>,
    profile: &ProfileConfig,
    profile_name: &str,
    profile_entry: metatheca::Uuid,
    id: i64,
    note: &enex::EnexNote,
    args: &ImportArgs,
    source_file: &str,
    parent: &Hash,
) -> Result<(ImportedNote, Hash)> {
    let filenames = enml::assign_filenames(&note.resources);
    let media: HashMap<String, MediaRef> = note
        .resources
        .iter()
        .zip(&filenames)
        .filter(|(r, _)| !r.md5_hex.is_empty())
        .map(|(r, f)| {
            (
                r.md5_hex.clone(),
                MediaRef {
                    filename: f.clone(),
                    mime: r.mime.clone(),
                },
            )
        })
        .collect();

    let (html, _warnings) = enml::enml_to_html(
        &note.content,
        &EnmlContext {
            title: &note.title,
            source_url: note.source_url.as_deref(),
            freeze_todos: args.freeze_todos,
            media: &media,
        },
    )?;
    let content = binding::normalize_due_times(&html, profile);
    let parsed = app.parse_index(reads, profile, profile_name, &content);

    let mut overlay = AppliedOverlay {
        tags: note
            .tags
            .iter()
            .filter_map(|t| enml::sanitize_tag(t))
            .collect(),
        categories: args.categories.clone(),
        ..Default::default()
    };
    overlay.tags.sort();
    overlay.tags.dedup();
    overlay.categories.sort();
    overlay.categories.dedup();
    let has_overlay = !overlay.tags.is_empty() || !overlay.categories.is_empty();

    let eff = facts::effective_index(Some(&parsed), Some(&overlay));
    check_constraints(profile, &eff, opts.force)?;

    let created_ns = note.created_ns.unwrap_or_else(now_ns);
    let ident = Ident {
        profile: profile_name.to_string(),
        id,
        ext: "html".to_string(),
    };
    let birth = Birth {
        profile: ident.profile.clone(),
        id,
        ext: ident.ext.clone(),
        created_at_ns: created_ns,
    };
    let entry = app.vault.new_entry_id();
    let blob = app.vault.deposit_blob(content.as_bytes())?;

    let mut batch = vec![
        to_fact(facts::K_CUJ_JOT, entry, &birth)?,
        to_fact(facts::K_CUJ_ID, entry, &ident)?,
        to_fact(facts::K_JOT_INDEX, entry, &parsed)?,
        Fact::blob_ref(entry, &blob.to_hex(), content.len() as u64),
        Fact::path(entry, &jot_path(&ident), true),
        Fact::fs_metadata(
            entry,
            &FsMetadata {
                birthtime_ns: Some(created_ns),
                mtime_ns: note.updated_ns,
                ..Default::default()
            },
            None,
        ),
        Fact::mime(entry, mime_for_ext(&ident.ext)),
        to_fact(
            facts::K_CUJ_IMPORT,
            entry,
            &ImportBody {
                fingerprint: note.fingerprint.clone(),
                source: source_file.to_string(),
                imported_at_ns: now_ns(),
                updated_at_ns: note.updated_ns,
            },
        )?,
    ];
    if has_overlay {
        batch.push(to_fact(facts::K_CUJ_APPLIED, entry, &overlay)?);
    }
    for (r, fname) in note.resources.iter().zip(&filenames) {
        let att_entry = app.vault.new_entry_id();
        let ablob = app.vault.deposit_blob(&r.bytes)?;
        batch.push(to_fact(
            facts::K_CUJ_ATTACHMENT,
            att_entry,
            &AttachmentBody {
                jot: entry.hyphenated().to_string(),
                filename: fname.clone(),
                size: r.bytes.len() as u64,
                added_at_ns: now_ns(),
                detached: false,
            },
        )?);
        batch.push(Fact::blob_ref(
            att_entry,
            &ablob.to_hex(),
            r.bytes.len() as u64,
        ));
        batch.push(Fact::mime(att_entry, &mime_for_bytes(fname, &r.bytes)));
        batch.push(Fact::path(
            att_entry,
            &att_path(profile_name, id, fname),
            true,
        ));
        if let Some(ocr) = &r.ocr_text {
            let xentry = app.vault.new_entry_id();
            let xblob = app.vault.deposit_blob(ocr.as_bytes())?;
            batch.push(to_fact(
                facts::K_CUJ_EXTRACTION,
                xentry,
                &ExtractionBody {
                    of: att_entry.hyphenated().to_string(),
                    tool: EXTRACTION_TOOL.to_string(),
                    tool_version: String::new(),
                },
            )?);
            batch.push(Fact::blob_ref(xentry, &xblob.to_hex(), ocr.len() as u64));
        }
    }
    batch.push(to_fact(
        facts::K_CUJ_COUNTER,
        profile_entry,
        &CounterBody { value: id },
    )?);
    let new_head = app.vault.commit_onto(&batch, parent)?;

    Ok((
        ImportedNote {
            id: Some(id),
            profile: profile_name.to_string(),
            title: note.title.clone(),
            file: source_file.to_string(),
            tags: overlay.tags,
            categories: overlay.categories,
            attachments: filenames,
            created_at_ns: created_ns,
            ..Default::default()
        },
        new_head,
    ))
}

pub fn print_report(report: &ImportReport, opts: &Opts) {
    if opts.json {
        let v = serde_json::json!({
            "imported": report.imported.iter().map(|n| serde_json::json!({
                "id": n.id,
                "profile": n.profile,
                "title": n.title,
                "file": n.file,
                "tags": n.tags,
                "categories": n.categories,
                "attachments": n.attachments,
                "created_at_ns": n.created_at_ns,
                "references": n.references,
                "unresolved": n.unresolved,
                "dropped": n.dropped,
            })).collect::<Vec<_>>(),
            "skipped": report.skipped.iter().map(|s| serde_json::json!({
                "title": s.title,
                "file": s.file,
                "reason": "duplicate",
                "existing": s.existing,
            })).collect::<Vec<_>>(),
            "errors": report.errors.iter().map(|e| serde_json::json!({
                "title": e.title,
                "file": e.file,
                "error": e.error,
            })).collect::<Vec<_>>(),
            "dry_run": report.dry_run,
            "summary": {
                "imported": report.imported.len(),
                "skipped": report.skipped.len(),
                "errors": report.errors.len(),
            },
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    } else if report.dry_run {
        println!(
            "Dry run: would import {} note(s), {} duplicate(s) would be skipped, {} error(s).",
            report.imported.len(),
            report.skipped.len(),
            report.errors.len()
        );
    } else {
        println!(
            "Imported {} note(s), skipped {} duplicate(s), {} error(s).",
            report.imported.len(),
            report.skipped.len(),
            report.errors.len()
        );
    }
}
