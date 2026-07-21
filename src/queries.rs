//! Data-returning read queries (SPEC §10). Everything `read`
//! renders on stdout is computed here as owned, serializable
//! values, so library consumers — the cui TUI, a web backend —
//! get structured results instead of captured output.

use std::path::Path;

use metatheca::Uuid;
use serde::Serialize;
use taxopsis::{Atom, Cursor, Direction, Expr, Op, OrderBy, QueryValue};

use crate::app::{App, Opts, RefArg, Suffix, att_path, jot_path};
use crate::binding::{due_to_ns, due_to_ns_local};
use crate::error::{Error, Result};
use crate::ext;
use crate::facts::{self, Ident, TodoBody, UrlBody};
use crate::reads::{Reads, Resolution};

// ── jot views ─────────────────────────────────────────────────────

/// A reference edge with the target's *current* identity resolved
/// for display; `entry`/`as` are the stored edge (SPEC §5.5).
#[derive(Debug, Clone, Serialize)]
pub struct ReferenceView {
    pub entry: String,
    #[serde(rename = "as")]
    pub written_as: String,
    /// The edge's full category path when typed (SPEC §5.5: a
    /// target's typed edges are distinct effective edges — one
    /// element each).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub live: bool,
}

/// The jot object of SPEC §11.2. Field additions here are
/// backwards-compatible; removals or renames require a protocol
/// revision.
#[derive(Debug, Clone, Serialize)]
pub struct JotView {
    pub profile: String,
    pub id: i64,
    pub ext: String,
    pub entry: String,
    pub created_at_ns: Option<i64>,
    pub modified_at_ns: Option<i64>,
    pub deleted: bool,
    pub tags: Vec<String>,
    pub categories: Vec<String>,
    pub references: Vec<ReferenceView>,
    pub unresolved_references: Vec<String>,
    pub todos: Vec<TodoBody>,
    pub urls: Vec<UrlBody>,
    pub bookmarks: Vec<String>,
    pub attachments: Vec<String>,
    /// First non-empty content line, derived for display.
    pub summary: String,
}

pub fn jot_view(reads: &Reads<'_>, entry: Uuid) -> JotView {
    let ident = &reads.idents[&entry];
    let e = reads.effective(entry);
    // One element per effective edge (SPEC §5.5, §11.2): a target
    // with several typed edges appears once per edge, each with
    // its own written form — collapsing them would both hide edges
    // and report a stale `as` after a refreshing link.
    let references = e
        .edges
        .iter()
        .flat_map(|(target, infos)| {
            let current = Uuid::parse_str(target)
                .ok()
                .and_then(|u| reads.idents.get(&u).map(|i| (u, i)));
            infos.iter().map(move |info| ReferenceView {
                entry: target.clone(),
                written_as: info.written_as.clone(),
                category: info.annot.category.clone(),
                profile: current.as_ref().map(|(_, i)| i.profile.clone()),
                id: current.as_ref().map(|(_, i)| i.id),
                live: current.is_some_and(|(u, _)| reads.is_live(u)),
            })
        })
        .collect();
    JotView {
        profile: ident.profile.clone(),
        id: ident.id,
        ext: ident.ext.clone(),
        entry: entry.hyphenated().to_string(),
        created_at_ns: reads.births.get(&entry).map(|b| b.created_at_ns),
        modified_at_ns: reads.modified.get(&entry).copied(),
        deleted: !reads.is_live(entry),
        tags: e.tags.iter().cloned().collect(),
        categories: e.categories.iter().cloned().collect(),
        references,
        unresolved_references: e.unresolved_references.clone(),
        todos: e.todos.clone(),
        urls: e.urls.clone(),
        bookmarks: e.bookmarks.clone(),
        attachments: reads
            .attachments_of(entry)
            .iter()
            .map(|(_, a)| a.filename.clone())
            .collect(),
        summary: summary_line(reads, entry),
    }
}

/// An observed file (SPEC §file entries) as a view: named by its
/// space and path rather than a profile and ID.
#[derive(Debug, Clone, Serialize)]
pub struct FileJotView {
    pub space: String,
    pub path: String,
    pub entry: String,
    pub modified_at_ns: Option<i64>,
    pub tags: Vec<String>,
    pub categories: Vec<String>,
    pub todos: Vec<TodoBody>,
    pub urls: Vec<UrlBody>,
    pub bookmarks: Vec<String>,
    pub summary: String,
}

pub fn file_view(reads: &Reads<'_>, entry: Uuid) -> FileJotView {
    let (space, path) = reads.file_paths.get(&entry).cloned().unwrap_or_default();
    let e = reads.effective(entry);
    FileJotView {
        space,
        path,
        entry: entry.hyphenated().to_string(),
        modified_at_ns: reads.modified.get(&entry).copied(),
        tags: e.tags.iter().cloned().collect(),
        categories: e.categories.iter().cloned().collect(),
        todos: e.todos.clone(),
        urls: e.urls.clone(),
        bookmarks: e.bookmarks.clone(),
        summary: summary_line(reads, entry),
    }
}

/// A retrieval result: a numbered jot or an observed file.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum HitTarget {
    Jot(JotView),
    File(FileJotView),
}

impl HitTarget {
    /// The display reference: `--p--N` or `///space/path`.
    pub fn display_ref(&self, scope: Option<&str>) -> String {
        match self {
            HitTarget::Jot(j) => match scope {
                Some(p) if *p == j.profile => format!("--{}", j.id),
                _ => format!("--{}--{}", j.profile, j.id),
            },
            HitTarget::File(f) => format!("///{}/{}", f.space, f.path),
        }
    }

    pub fn summary(&self) -> &str {
        match self {
            HitTarget::Jot(j) => &j.summary,
            HitTarget::File(f) => &f.summary,
        }
    }
}

fn truncate72(line: &str) -> String {
    let line = line.trim();
    let mut s: String = line.chars().take(72).collect();
    if line.chars().count() > 72 {
        s.push('…');
    }
    s
}

/// First non-empty content line, truncated for display.
pub fn summary_line(reads: &Reads<'_>, entry: Uuid) -> String {
    let Ok(bytes) = reads.bytes(entry) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    truncate72(text.lines().find(|l| !l.trim().is_empty()).unwrap_or(""))
}

/// A display-time snippet from the hit blob (SPEC §10.10): the first
/// line containing a query term, falling back to the first
/// non-empty line.
fn snippet_line(reads: &Reads<'_>, hit: Uuid, terms: &[String]) -> String {
    let Ok(bytes) = reads.bytes(hit) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let matched = text.lines().filter(|l| !l.trim().is_empty()).find(|l| {
        let ll = l.to_lowercase();
        terms.iter().any(|t| ll.contains(t.as_str()))
    });
    truncate72(
        matched
            .or_else(|| text.lines().find(|l| !l.trim().is_empty()))
            .unwrap_or(""),
    )
}

pub(crate) fn ref_profile(app: &App, opts: &Opts, r: &RefArg) -> Result<String> {
    match &r.profile {
        Some(p) => Ok(p.clone()),
        None => opts
            .read_scope(&app.client)
            .map(String::from)
            .ok_or_else(|| {
                Error::Usage("bare --N is ambiguous under --profile all; use --profile--N".into())
            }),
    }
}

// ── show (SPEC §10.4) ─────────────────────────────────────────────

pub enum ShowOutcome {
    Shown {
        jot: Box<JotView>,
        /// The requested content or fragment.
        content: String,
    },
    Deleted {
        profile: String,
        id: i64,
    },
    Moved {
        profile: String,
        id: i64,
        current: Ident,
    },
}

pub fn show(app: &App, opts: &Opts, r: &RefArg, expand: bool) -> Result<ShowOutcome> {
    let reads = app.reads(opts)?;
    let profile = ref_profile(app, opts, r)?;
    let entry = match reads.resolve(&profile, r.id)? {
        Resolution::Live(e) => e,
        Resolution::Deleted(_) => {
            return Ok(ShowOutcome::Deleted { profile, id: r.id });
        }
        Resolution::Moved(_, cur) => {
            return Ok(ShowOutcome::Moved {
                profile,
                id: r.id,
                current: cur,
            });
        }
    };
    let bytes = reads.bytes(entry)?;
    let content = String::from_utf8_lossy(&bytes);
    let mut fragment = extract_fragment(&reads, entry, &content, r.suffix.as_ref())?;
    if expand {
        let mut path = vec![entry];
        fragment = expand_content(&reads, &profile, &fragment, EXPAND_DEPTH, &mut path);
    }
    Ok(ShowOutcome::Shown {
        jot: Box::new(jot_view(&reads, entry)),
        content: fragment,
    })
}

/// Show an observed file (SPEC §op-show): the file-jot view plus
/// its content.
pub fn show_file(
    app: &App,
    opts: &Opts,
    space: Option<&str>,
    path: &str,
) -> Result<(FileJotView, String)> {
    let reads = app.reads(opts)?;
    let space = resolve_space(&reads, app, opts, space)?;
    let entry = *reads
        .files
        .get(&(space.clone(), path.to_string()))
        .ok_or_else(|| Error::NotFound(format!("///{space}/{path}")))?;
    let content = String::from_utf8_lossy(&reads.bytes(entry)?).into_owned();
    Ok((file_view(&reads, entry), content))
}

/// The space a bare `//path` resolves in: explicit, else the read
/// scope's default_space (SPEC §file references).
fn resolve_space(reads: &Reads<'_>, app: &App, opts: &Opts, space: Option<&str>) -> Result<String> {
    if let Some(s) = space {
        return Ok(s.to_string());
    }
    let profile = opts
        .read_scope(&app.client)
        .unwrap_or(&app.client.active_profile);
    reads
        .profile(profile)?
        .default_space
        .clone()
        .ok_or_else(|| {
            Error::Usage(format!(
                "bare //path needs default_space on profile {profile:?} \
                 (config set --vault default_space <name>), or use ///<space>/<path>"
            ))
        })
}

/// Transclusion (SPEC §op-show): replace resolvable
/// suffix-carrying ID references with the referenced fragment,
/// depth-bounded and cycle-guarded.
const EXPAND_DEPTH: usize = 8;

fn expand_content(
    reads: &Reads<'_>,
    own_profile: &str,
    content: &str,
    depth: usize,
    path: &mut Vec<Uuid>,
) -> String {
    if depth == 0 {
        return content.to_string();
    }
    let profile_cfg = reads
        .profile(own_profile)
        .cloned()
        .unwrap_or_else(|_| crate::facts::ProfileConfig::new("expand"));
    let parsed = crate::binding::parse(content, &profile_cfg);

    // (byte start, byte end, replacement), applied back-to-front.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut consider = |profile: &str,
                        id: &str,
                        fragment: &Option<jots::Fragment>,
                        bookmark: &Option<String>,
                        loc: &Option<jots::Loc>,
                        raw: &Option<String>| {
        let (Some(loc), Some(raw)) = (loc, raw) else {
            return;
        };
        let suffix = match (fragment, bookmark) {
            // The parser attaches fragments it only diagnoses
            // (`:0`, `:3-1`); an invalid range must not reach the
            // slice arithmetic — leave the construct as written.
            (Some(f), _) if f.start < 1 || f.end < f.start => return,
            (Some(f), _) => Suffix::Lines(f.start as usize, f.end as usize),
            (None, Some(b)) => Suffix::Bookmark(b.clone()),
            // Suffix-less references are never expanded: the
            // suffix is the transclusion marker.
            (None, None) => return,
        };
        let Ok(id) = id.parse::<i64>() else { return };
        let Ok((entry, ident)) = reads.resolve_live(profile, id) else {
            return;
        };
        if path.contains(&entry) {
            return; // cycle guard: leave the construct as written
        }
        let Ok(bytes) = reads.bytes(entry) else {
            return;
        };
        let target = String::from_utf8_lossy(&bytes);
        let Ok(fragment) = extract_fragment(reads, entry, &target, Some(&suffix)) else {
            return;
        };
        let target_profile = ident.profile.clone();
        path.push(entry);
        let expanded = expand_content(reads, &target_profile, &fragment, depth - 1, path);
        path.pop();
        let framed = format!("── {raw} ──\n{}\n──\n", expanded.trim_end_matches('\n'));
        edits.push((loc.start, loc.end, framed));
    };
    for r in &parsed.references {
        consider(own_profile, &r.id, &r.fragment, &r.bookmark, &r.loc, &r.raw);
    }
    for r in &parsed.cross_references {
        consider(&r.space, &r.id, &r.fragment, &r.bookmark, &r.loc, &r.raw);
    }
    if edits.is_empty() {
        return content.to_string();
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = content.to_string();
    for (start, end, repl) in edits {
        if start <= end && end <= out.len() {
            out.replace_range(start..end, &repl);
        }
    }
    out
}

/// Fragment extraction (SPEC §10.4, §6.8): line ranges directly;
/// bookmarks from the anchor's line to the next bookmark or EOF.
pub(crate) fn extract_fragment(
    reads: &Reads<'_>,
    entry: Uuid,
    content: &str,
    suffix: Option<&Suffix>,
) -> Result<String> {
    match suffix {
        None => Ok(content.to_string()),
        Some(Suffix::Lines(a, b)) => {
            let lines: Vec<&str> = content.lines().collect();
            if *a > lines.len() {
                return Err(Error::NotFound(format!(
                    "line {a} (jot has {})",
                    lines.len()
                )));
            }
            let end = (*b).min(lines.len());
            Ok(lines[a - 1..end].join("\n") + "\n")
        }
        Some(Suffix::Bookmark(name)) => {
            let bookmarks = reads
                .parsed
                .get(&entry)
                .map(|p| p.bookmarks.clone())
                .unwrap_or_default();
            if !bookmarks.contains(name) {
                return Err(Error::NotFound(format!("bookmark #{name}")));
            }
            let anchor = format!("!!{name}");
            let mut out = Vec::new();
            let mut collecting = false;
            for line in content.lines() {
                if collecting {
                    let starts_new = line.split_whitespace().any(|w| {
                        w.starts_with("!!")
                            && w != anchor.as_str()
                            && bookmarks.iter().any(|b| w == format!("!!{b}"))
                    });
                    if starts_new {
                        break;
                    }
                    out.push(line);
                } else if line.split_whitespace().any(|w| w == anchor) {
                    collecting = true;
                    out.push(line);
                }
            }
            Ok(out.join("\n") + "\n")
        }
    }
}

// ── list (SPEC §10.5) ─────────────────────────────────────────────

pub fn list(app: &App, opts: &Opts, offset: usize, limit: Option<usize>) -> Result<Vec<JotView>> {
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    Ok(reads
        .list(scope)
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|(e, _)| jot_view(&reads, e))
        .collect())
}

// ── find (SPEC §10.6) ─────────────────────────────────────────────

pub struct FindArgs {
    pub tags: Vec<String>,
    pub categories: Vec<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub todo: bool,
    pub filter: Option<String>,
}

/// Parse find's filter-token grammar (SPEC §10.6) — shared by the
/// CLI, grep's filter prefix, and saved queries.
pub fn parse_find_args(tokens: &[String]) -> Result<FindArgs> {
    let mut f = FindArgs {
        tags: vec![],
        categories: vec![],
        since: None,
        until: None,
        todo: false,
        filter: None,
    };
    let mut it = tokens.iter();
    while let Some(t) = it.next() {
        if let Some(x) = t.strip_prefix("..") {
            f.tags.push(x.to_string());
        } else if let Some(x) = t.strip_prefix("::") {
            f.categories.push(x.to_string());
        } else {
            match t.as_str() {
                "--since" => {
                    f.since = Some(
                        it.next()
                            .ok_or_else(|| Error::Usage("--since needs a date".into()))?
                            .clone(),
                    )
                }
                "--until" => {
                    f.until = Some(
                        it.next()
                            .ok_or_else(|| Error::Usage("--until needs a date".into()))?
                            .clone(),
                    )
                }
                "--todo" => f.todo = true,
                "--filter" => {
                    f.filter = Some(
                        it.next()
                            .ok_or_else(|| Error::Usage("--filter needs an expression".into()))?
                            .clone(),
                    )
                }
                other => return Err(Error::Usage(format!("find: unexpected argument {other:?}"))),
            }
        }
    }
    Ok(f)
}

/// Run a saved query (SPEC §op-query): exactly find with the
/// stored tokens.
pub fn run_saved_query(
    app: &App,
    opts: &Opts,
    name: &str,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<FindPage> {
    let args = {
        let reads = app.reads(opts)?;
        match opts.read_scope(&app.client) {
            Some(p) => reads
                .queries
                .get(&(p.to_string(), name.to_string()))
                .map(|(_, q)| q.args.clone())
                .ok_or_else(|| Error::NotFound(format!("saved query {name:?}")))?,
            // --profile all widens: a name unique across profiles
            // runs; several holders are ambiguous, never silently
            // the active profile's.
            None => {
                let hits: Vec<(&String, Vec<String>)> = reads
                    .queries
                    .iter()
                    .filter(|((_, n), _)| n == name)
                    .map(|((p, _), (_, q))| (p, q.args.clone()))
                    .collect();
                match hits.len() {
                    0 => return Err(Error::NotFound(format!("saved query {name:?}"))),
                    1 => hits.into_iter().next().map(|(_, a)| a).unwrap_or_default(),
                    _ => {
                        return Err(Error::Usage(format!(
                            "saved query {name:?} is ambiguous under --profile all ({})",
                            hits.iter()
                                .map(|(p, _)| p.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    }
                }
            }
        }
    };
    let f = parse_find_args(&args)?;
    find(app, opts, &f, limit, cursor)
}

/// One page of find/backlinks results; `cursor` resumes the next
/// page against the same index state (taxopsis SPEC §6.8).
pub struct FindPage {
    pub hits: Vec<HitTarget>,
    pub cursor: Option<String>,
}

fn atom(predicate: &str, op: Op, value: QueryValue) -> Expr {
    Expr::Atom(Atom {
        predicate: predicate.to_string(),
        op,
        value,
    })
}

fn and(a: Expr, b: Expr) -> Expr {
    Expr::And(Box::new(a), Box::new(b))
}

fn or(a: Expr, b: Expr) -> Expr {
    Expr::Or(Box::new(a), Box::new(b))
}

/// Whether a non-eq atom is queryable in this index state: probes
/// with limit 1 and treats a LayoutMismatch (absent predicate) as
/// "no"; genuine errors resurface on the real query.
fn probe(chain: &taxopsis::Chain, sref: &str, a: &Expr) -> bool {
    !matches!(
        chain.query(a, sref, None, Some(1), None),
        Err(taxopsis::Error::LayoutMismatch(_))
    )
}

/// The document-predicate disjunction for one construct kind
/// (SPEC §10.6): the effective index is the union of the parsed and
/// applied documents.
fn both(member: &str, op: Op, value: &str) -> Expr {
    or(
        atom(
            &format!("jot/index/{member}"),
            op,
            QueryValue::Str(value.to_string()),
        ),
        atom(
            &format!("cuj/applied/{member}"),
            op,
            QueryValue::Str(value.to_string()),
        ),
    )
}

pub fn find(
    app: &App,
    opts: &Opts,
    args: &FindArgs,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<FindPage> {
    let (chain, sref) = taxopsis_at(app, opts)?;
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);

    // Range/prefix atoms on a predicate absent from the index
    // state are a taxopsis LayoutMismatch (its SPEC §6.5), so each
    // non-eq atom is probed first: an absent predicate contributes
    // the empty set (None here — conjoined, the whole result is
    // empty; in a disjunction, the side just drops out).
    let mut empty = false;
    let mut expr: Option<Expr> = None;
    let mut add = |e: Option<Expr>| match e {
        Some(e) => {
            expr = Some(match expr.take() {
                Some(prev) => and(prev, e),
                None => e,
            });
        }
        None => empty = true,
    };
    for t in &args.tags {
        // Three-way where dialect facts are in scope (SPEC §9.2):
        // jot/index, cuj/applied, obsidian/index.
        add(Some(or(
            both("tags", Op::Eq, &t.to_lowercase()),
            atom(
                "obsidian/index/tags",
                Op::Eq,
                QueryValue::Str(t.to_lowercase()),
            ),
        )));
    }
    for c in &args.categories {
        let c = c.to_lowercase();
        // Segment-boundary match: ::work selects work and work::*,
        // never the sibling ::workshop (a raw string prefix would).
        let sides: Vec<Expr> = [
            "jot/index/categories",
            "cuj/applied/categories",
            "obsidian/index/categories",
        ]
        .into_iter()
        .filter_map(|p| {
            let pref = atom(p, Op::Prefix, QueryValue::Str(format!("{c}::")));
            if !probe(&chain, &sref, &pref) {
                return None;
            }
            Some(or(atom(p, Op::Eq, QueryValue::Str(c.clone())), pref))
        })
        .collect();
        add(sides.into_iter().reduce(or));
    }
    if let Some(s) = &args.since {
        let ns = since_bound_ns(s)?;
        let a = atom("cuj/jot/created_at_ns", Op::Ge, QueryValue::Int(ns));
        add(probe(&chain, &sref, &a).then_some(a));
    }
    if let Some(u) = &args.until {
        let ns = until_bound_ns(u)?;
        let a = atom("cuj/jot/created_at_ns", Op::Le, QueryValue::Int(ns));
        add(probe(&chain, &sref, &a).then_some(a));
    }
    if args.todo {
        add(Some(both("todos/done", Op::Eq, "false")));
    }
    if let Some(f) = &args.filter {
        add(Some(taxopsis::parse_filter(f)?));
    }
    if empty {
        return Ok(FindPage {
            hits: Vec::new(),
            cursor: None,
        });
    }
    let expr = expr.ok_or_else(|| {
        Error::Usage(
            "find needs at least one filter (tag, category, date, --todo, --filter)".into(),
        )
    })?;

    let order = OrderBy {
        predicate: "cuj/id/id".into(),
        direction: Direction::Asc,
    };
    // The profile scope is NOT a query conjunct: file-jots carry no
    // cuj/id and appear in any scope (SPEC §8.6), so scoping
    // filters post-query, with plain-offset pagination over the
    // filtered set — same shape as backlinks.
    let mut all = query_page(&chain, &sref, &reads, &expr, &order, None, None)?.hits;
    if let Some(p) = scope {
        all.retain(|h| match h {
            HitTarget::Jot(v) => v.profile == p,
            HitTarget::File(_) => true,
        });
    }
    offset_page(all, limit, cursor)
}

/// Offset pagination over a post-filtered hit set ("o:<n>"
/// cursors): scope filtering happens outside taxopsis, so its
/// native cursors would skip filtered-out rows unpredictably.
fn offset_page(all: Vec<HitTarget>, limit: Option<usize>, cursor: Option<&str>) -> Result<FindPage> {
    let start = match cursor {
        Some(c) => c
            .strip_prefix("o:")
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(|| Error::Usage(format!("bad cursor {c:?}")))?,
        None => 0,
    };
    let total = all.len();
    let start = start.min(total);
    let hits: Vec<HitTarget> = all
        .into_iter()
        .skip(start)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let consumed = start + hits.len();
    Ok(FindPage {
        hits,
        cursor: (consumed < total).then(|| format!("o:{consumed}")),
    })
}

fn query_page(
    chain: &taxopsis::Chain,
    sref: &str,
    reads: &Reads<'_>,
    expr: &Expr,
    order: &OrderBy,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<FindPage> {
    let cursor = cursor.map(Cursor::decode).transpose()?;
    let page = match chain.query(expr, sref, Some(order), limit, cursor.as_ref()) {
        Ok(p) => p,
        // No document in this index state carries the order
        // predicate (e.g. a vault of only file-jots, which have no
        // cuj/id): serve unordered rather than failing.
        Err(taxopsis::Error::LayoutMismatch(_)) if cursor.is_none() => {
            chain.query(expr, sref, None, limit, None)?
        }
        Err(e) => return Err(e.into()),
    };
    let hits = page
        .rows
        .iter()
        .filter_map(|row| {
            if reads.idents.contains_key(&row.entry) {
                Some(HitTarget::Jot(jot_view(reads, row.entry)))
            } else if reads.file_paths.contains_key(&row.entry) {
                Some(HitTarget::File(file_view(reads, row.entry)))
            } else {
                None
            }
        })
        .collect();
    Ok(FindPage {
        hits,
        cursor: page.cursor.map(|c| c.encode()),
    })
}

/// The taxopsis chain and stateref serving this read: fresh (or
/// --stale) at head, or the index state matching --at (SPEC §10.35),
/// else Stale.
fn taxopsis_at(app: &App, opts: &Opts) -> Result<(taxopsis::Chain, String)> {
    if opts.at.is_some() {
        let target = app.vault.resolve(opts.at.as_deref().unwrap())?;
        let chain = ext::open_taxopsis(app.root())?;
        let head = chain.current_hash()?;
        for (h, state) in chain.walk(&head)? {
            if state.metatheca == target {
                let sref = h.to_hex();
                return Ok((chain, sref));
            }
        }
        Err(Error::Stale(format!(
            "no taxopsis index state for metatheca state {}",
            target.to_hex()
        )))
    } else {
        let chain = ext::taxopsis_fresh(app.root(), &app.vault, opts.stale, opts.stale_notes)?;
        let sref = chain.current_hash()?.to_hex();
        Ok((chain, sref))
    }
}

// ── grep (SPEC §10.9) ─────────────────────────────────────────────

/// Where a grep hit lives. Untagged: jot hits serialize exactly as
/// before ({profile, id, ...}); file hits carry {space, path, ...}.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum GrepSource {
    Jot { profile: String, id: i64 },
    File { space: String, path: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct GrepHit {
    #[serde(flatten)]
    pub source: GrepSource,
    /// Set when the hit is in an attachment's extracted text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
    pub line: usize,
    pub text: String,
}

pub fn grep(app: &App, opts: &Opts, pattern: &str, filters: &FindArgs) -> Result<Vec<GrepHit>> {
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|e| Error::Usage(format!("bad pattern: {e}")))?;
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    let matches_filters = |entry| {
        if filters.tags.is_empty() && filters.categories.is_empty() {
            return true;
        }
        let e = reads.effective(entry);
        filters
            .tags
            .iter()
            .all(|t| e.tags.contains(&t.to_lowercase()))
            && filters.categories.iter().all(|c| {
                let c = c.to_lowercase();
                e.categories
                    .iter()
                    .any(|x| x == &c || x.starts_with(&format!("{c}::")))
            })
    };
    let scan = |hits: &mut Vec<GrepHit>, source: &GrepSource, attachment: Option<&str>, text: &str| {
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                hits.push(GrepHit {
                    source: source.clone(),
                    attachment: attachment.map(String::from),
                    line: i + 1,
                    text: line.trim_end().to_string(),
                });
            }
        }
    };
    let mut hits = Vec::new();
    for (entry, ident) in reads.list(scope) {
        if !matches_filters(entry) {
            continue;
        }
        let source = GrepSource::Jot {
            profile: ident.profile.clone(),
            id: ident.id,
        };
        if let Ok(bytes) = reads.bytes(entry) {
            scan(&mut hits, &source, None, &String::from_utf8_lossy(&bytes));
        }
        for (att_entry, att) in reads.attachments_of(entry) {
            if let Some(x) = reads.extraction_of(att_entry) {
                if let Ok(bytes) = reads.bytes(x) {
                    scan(
                        &mut hits,
                        &source,
                        Some(&att.filename),
                        &String::from_utf8_lossy(&bytes),
                    );
                }
            }
        }
    }
    // File-jots are scanned in any scope (SPEC §8.6). Only textual
    // content: an observed binary would grep as garbage.
    for ((space, rel), entry) in &reads.files {
        if !matches_filters(*entry) {
            continue;
        }
        let Ok(bytes) = reads.bytes(*entry) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let source = GrepSource::File {
            space: space.clone(),
            path: rel.clone(),
        };
        scan(&mut hits, &source, None, text);
    }
    Ok(hits)
}

// ── search / similar (SPEC §10.10, 10.11) ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    #[serde(flatten)]
    pub target: HitTarget,
    pub score: f64,
    pub snippet: String,
    /// Which ranking served the query (SPEC §zetetes binding):
    /// "hybrid", "lexical", or "semantic".
    pub engine: &'static str,
}

pub enum SimilarTarget<'a> {
    Jot(&'a RefArg),
    Text(&'a str),
}

pub fn search(
    app: &App,
    opts: &Opts,
    query: &str,
    k: usize,
    offset: usize,
    lexical: bool,
) -> Result<Vec<SearchHit>> {
    if opts.at.is_some() {
        return Err(Error::Stale("search does not support --at yet".into()));
    }
    // Hybrid by default when the fusion chain exists (SPEC
    // §zetetes binding); --lexical forces the logopsis ranking.
    let zchain = if lexical {
        None
    } else {
        ext::zetetes_fresh(app.root(), &app.vault, opts.stale, opts.stale_notes)?
    };
    type Fetch = Box<dyn FnMut(usize) -> Result<Vec<(Uuid, f64)>>>;
    let engine: &'static str;
    let mut fetch: Fetch = match zchain {
        Some(mut chain) => {
            engine = "hybrid";
            let q = query.to_string();
            Box::new(move |want| {
                Ok(chain
                    .query(&q, want, "current")?
                    .into_iter()
                    .map(|h| (h.entry, h.score))
                    .collect())
            })
        }
        None => {
            let chain = ext::logopsis_fresh(app.root(), &app.vault, opts.stale, opts.stale_notes)?;
            let expr = logopsis::parse_query(query)?;
            engine = "lexical";
            Box::new(move |want| {
                Ok(chain
                    .query(&expr, want, "current")?
                    .into_iter()
                    .map(|h| (h.entry, h.score))
                    .collect())
            })
        }
    };
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.trim_matches('"').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    // Scope filtering happens in map_hits, so a fixed candidate
    // budget can starve a small profile next to a large one —
    // widen the ask until k in-scope hits or the ranking is dry.
    let mut want = (k + offset) * 4 + 16;
    loop {
        let raw = fetch(want)?;
        let exhausted = raw.len() < want;
        let hits = map_hits(&reads, scope, k, offset, &terms, engine, raw.into_iter());
        if hits.len() >= k || exhausted {
            return Ok(hits);
        }
        want = want.saturating_mul(4);
    }
}

pub fn similar(
    app: &App,
    opts: &Opts,
    target: SimilarTarget<'_>,
    k: usize,
    offset: usize,
) -> Result<Vec<SearchHit>> {
    if opts.at.is_some() {
        return Err(Error::Stale("similar does not support --at yet".into()));
    }
    let chain = ext::semopsis_fresh(app.root(), &app.vault, opts.stale, opts.stale_notes)?;
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    let (query_bytes, exclude): (Vec<u8>, Option<Uuid>) = match target {
        SimilarTarget::Text(t) => (t.as_bytes().to_vec(), None),
        SimilarTarget::Jot(r) => {
            let (entry, _) = app.resolve_live(&reads, r, opts)?;
            (reads.bytes(entry)?, Some(entry))
        }
    };
    // Same widening loop as search: post-hoc scope filtering must
    // not starve a small profile.
    let mut want = (k + offset) * 4 + 16;
    loop {
        let raw = chain.query(&query_bytes, want, None, "current")?;
        let exhausted = raw.len() < want;
        let hits = map_hits(
            &reads,
            scope,
            k,
            offset,
            &[],
            "semantic",
            // The query jot itself is always its own nearest
            // neighbor — not an answer to "similar to".
            raw.into_iter()
                .filter(|h| Some(h.entry) != exclude)
                .map(|h| (h.entry, -h.distance)),
        );
        if hits.len() >= k || exhausted {
            return Ok(hits);
        }
        want = want.saturating_mul(4);
    }
}

fn map_hits(
    reads: &Reads<'_>,
    scope: Option<&str>,
    k: usize,
    offset: usize,
    terms: &[String],
    engine: &'static str,
    hits: impl Iterator<Item = (Uuid, f64)>,
) -> Vec<SearchHit> {
    let mut seen = std::collections::HashSet::new();
    let mut skipped = 0usize;
    let mut out = Vec::new();
    for (entry, score) in hits {
        if out.len() >= k {
            break;
        }
        let Some(owner) = reads.owning_jot(entry) else {
            continue;
        };
        if !reads.is_live(owner) {
            continue;
        }
        // File-jots have no profile and appear in any scope (SPEC
        // §file retrieval); jots respect the profile scope.
        let target = if reads.idents.contains_key(&owner) {
            if let Some(p) = scope {
                if reads.idents[&owner].profile != p {
                    continue;
                }
            }
            HitTarget::Jot(jot_view(reads, owner))
        } else if reads.file_paths.contains_key(&owner) {
            HitTarget::File(file_view(reads, owner))
        } else {
            continue;
        };
        if !seen.insert(owner) {
            continue;
        }
        if skipped < offset {
            skipped += 1;
            continue;
        }
        // Snippets are derived at display time from the hit blob
        // (SPEC §10.10); term-less (semantic) hits fall back to the
        // owning jot's summary.
        let snippet = if terms.is_empty() {
            summary_line(reads, owner)
        } else {
            snippet_line(reads, entry, terms)
        };
        out.push(SearchHit {
            target,
            score,
            snippet,
            engine,
        });
    }
    out
}

// ── backlinks / references (SPEC §10.20) ──────────────────────────

pub fn backlinks(
    app: &App,
    opts: &Opts,
    r: &RefArg,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<FindPage> {
    let reads = app.reads(opts)?;
    let profile = ref_profile(app, opts, r)?;
    let (entry, _) = reads.resolve_live(&profile, r.id)?;
    let uuid = entry.hyphenated().to_string();
    let scope = opts.read_scope(&app.client);

    let mut all: Vec<HitTarget> = if opts.at.is_some() {
        // Historical reads answer from the transient view directly
        // (SPEC §10.35): the Stale allowance covers find/search/
        // similar only, and no taxopsis index state need match the
        // requested metatheca state.
        let mut out: Vec<HitTarget> = reads
            .list(None)
            .into_iter()
            .filter(|(e, _)| reads.effective(*e).edges.contains_key(&uuid))
            .map(|(e, _)| HitTarget::Jot(jot_view(&reads, e)))
            .collect();
        out.extend(
            reads
                .files
                .values()
                .filter(|e| reads.effective(**e).edges.contains_key(&uuid))
                .map(|e| HitTarget::File(file_view(&reads, *e))),
        );
        out
    } else {
        // SPEC §10.20: a taxopsis query on the two references/entry
        // predicates — no reverse index exists.
        let (chain, sref) = taxopsis_at(app, opts)?;
        let expr = both("references/entry", Op::Eq, &uuid);
        let order = OrderBy {
            predicate: "cuj/id/id".into(),
            direction: Direction::Asc,
        };
        query_page(&chain, &sref, &reads, &expr, &order, None, None)?.hits
    };
    // File-jots appear in any scope; jots respect it (SPEC §8.6).
    if let Some(p) = scope {
        all.retain(|h| match h {
            HitTarget::Jot(v) => v.profile == p,
            HitTarget::File(_) => true,
        });
    }
    offset_page(all, limit, cursor)
}

/// The outbound reference listing of a jot, partitioned by how
/// each edge resolves today.
#[derive(Debug, Clone, Serialize)]
pub struct ReferenceListing {
    pub resolved: Vec<JotView>,
    /// Known targets whose current identity is tombstoned.
    pub deleted: Vec<Ident>,
    /// Edges whose entry is unknown to the current view.
    pub unknown: Vec<String>,
    /// Recorded unresolved at write time (SPEC §3.4).
    pub unresolved: Vec<String>,
}

pub fn references(app: &App, opts: &Opts, r: &RefArg) -> Result<ReferenceListing> {
    let reads = app.reads(opts)?;
    let profile = ref_profile(app, opts, r)?;
    let (entry, _) = reads.resolve_live(&profile, r.id)?;
    let e = reads.effective(entry);
    let mut listing = ReferenceListing {
        resolved: Vec::new(),
        deleted: Vec::new(),
        unknown: Vec::new(),
        unresolved: e.unresolved_references.clone(),
    };
    for (target, infos) in &e.edges {
        match Uuid::parse_str(target).ok() {
            Some(u) if reads.idents.contains_key(&u) => {
                if reads.is_live(u) {
                    listing.resolved.push(jot_view(&reads, u));
                } else {
                    listing.deleted.push(reads.idents[&u].clone());
                }
            }
            _ => listing.unknown.extend(infos.iter().map(|i| i.written_as.clone())),
        }
    }
    Ok(listing)
}

// ── attachments (SPEC §10.22) ─────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AttachmentView {
    pub filename: String,
    pub size: u64,
    /// The stored core/mime value (SPEC §7.1); absent only in a
    /// vault written without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    pub added_at_ns: i64,
    pub extracted: bool,
}

pub fn attachments(app: &App, opts: &Opts, r: &RefArg) -> Result<Vec<AttachmentView>> {
    let reads = app.reads(opts)?;
    let profile = ref_profile(app, opts, r)?;
    let (entry, _) = reads.resolve_live(&profile, r.id)?;
    Ok(reads
        .attachments_of(entry)
        .into_iter()
        .map(|(e, a)| AttachmentView {
            filename: a.filename.clone(),
            size: a.size,
            mime: reads.mimes.get(&e).cloned(),
            added_at_ns: a.added_at_ns,
            extracted: reads.extraction_of(e).is_some(),
        })
        .collect())
}

// ── history / log (SPEC §10.26) ───────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEvent {
    pub at_ns: i64,
    pub kind: String,
    pub description: String,
}

pub fn history(app: &App, opts: &Opts, r: &RefArg) -> Result<Vec<HistoryEvent>> {
    let reads = app.reads(opts)?;
    let profile = ref_profile(app, opts, r)?;
    let entry = match reads.resolve(&profile, r.id)? {
        Resolution::Live(e) | Resolution::Deleted(e) | Resolution::Moved(e, _) => e,
    };
    let head = match &opts.at {
        Some(sref) => app.vault.resolve(sref)?,
        None => app.vault.current_state_hash()?,
    };
    let mut states = app.vault.walk(&head)?;
    states.reverse();
    let mut events = Vec::new();
    for (_h, state) in &states {
        for fh in &state.added_facts {
            let bytes = app.vault.get_fact_bytes(fh)?;
            let fact = metatheca::Fact::from_bytes(&bytes)?;
            if fact.entry != entry {
                continue;
            }
            events.push(HistoryEvent {
                at_ns: state.created_at_ns,
                kind: fact.kind.clone(),
                description: describe_fact(&fact),
            });
        }
    }
    Ok(events)
}

fn describe_fact(fact: &metatheca::Fact) -> String {
    use crate::facts as f;
    match fact.kind.as_str() {
        f::K_CUJ_JOT => {
            let p = fact
                .body
                .get("profile")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let id = fact.body.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            format!("created as --{p}--{id}")
        }
        f::K_CUJ_ID => {
            let p = fact
                .body
                .get("profile")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let id = fact.body.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            format!("identity --{p}--{id}")
        }
        f::K_JOT_INDEX => "content parsed (jot/index)".to_string(),
        f::K_CUJ_APPLIED => "overlay updated (cuj/applied)".to_string(),
        "core/blob-ref" => "content blob".to_string(),
        "core/path" => {
            let p = fact
                .body
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let linked = fact
                .body
                .get("linked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            format!("path {} {p}", if linked { "+" } else { "-" })
        }
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub state: String,
    pub at_ns: i64,
    pub facts: usize,
}

pub fn vault_log(
    app: &App,
    since: Option<&str>,
    until: Option<&str>,
    n: Option<usize>,
) -> Result<Vec<LogEntry>> {
    let since_ns = since
        .map(|s| due_to_ns(s).ok_or_else(|| Error::Usage(format!("bad date {s:?}"))))
        .transpose()?;
    let until_ns = until
        .map(|s| due_to_ns(s).ok_or_else(|| Error::Usage(format!("bad date {s:?}"))))
        .transpose()?;
    Ok(app
        .vault
        .log(since_ns, until_ns, n)?
        .into_iter()
        .map(|(h, state)| LogEntry {
            state: h.to_hex(),
            at_ns: state.created_at_ns,
            facts: state.added_facts.len(),
        })
        .collect())
}

// ── profiles (SPEC §10.27) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub name: String,
    pub default_ext: String,
    pub jots_frozen: bool,
    pub active: bool,
    pub jots: usize,
}

pub fn profiles(app: &App, opts: &Opts) -> Result<Vec<ProfileView>> {
    let reads = app.reads(opts)?;
    Ok(reads
        .profiles
        .values()
        .map(|(_, p)| ProfileView {
            name: p.name.clone(),
            default_ext: p.default_ext.clone(),
            jots_frozen: p.jots_frozen,
            active: p.name == app.client.active_profile,
            jots: reads.list(Some(&p.name)).len(),
        })
        .collect())
}

// ── export (SPEC §10.34) ──────────────────────────────────────────

/// A filesystem name taken from vault data may only be written
/// inside the export dir — a vault produced by another writer
/// could carry separators or dot-dots in a filename or extension.
fn safe_component(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\'])
}

/// Materialize the jot and attachment tree into `dir`; returns the
/// number of files written.
pub fn export(app: &App, opts: &Opts, dir: &Path) -> Result<usize> {
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    let mut count = 0usize;
    for (entry, ident) in reads.list(scope) {
        if !safe_component(&ident.profile) || !safe_component(&ident.ext) {
            return Err(Error::Substrate(format!(
                "refusing to export --{}--{}: unsafe profile or extension",
                ident.profile, ident.id
            )));
        }
        let Ok(bytes) = reads.bytes(entry) else {
            continue;
        };
        let path = dir.join(jot_path(ident));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &bytes)?;
        count += 1;
        for (att_entry, att) in reads.attachments_of(entry) {
            if !safe_component(&att.filename) {
                return Err(Error::Substrate(format!(
                    "refusing to export attachment {:?} of --{}--{}: unsafe filename",
                    att.filename, ident.profile, ident.id
                )));
            }
            // Attachments stream out — the large items of an
            // export; the verify-at-EOF read is driven to EOF by
            // the copy.
            let Some(hash) = reads.blobs.get(&att_entry) else {
                continue;
            };
            let Ok(mut r) = app.vault.get_blob_reader(hash) else {
                continue;
            };
            let apath = dir.join(att_path(&ident.profile, ident.id, &att.filename));
            if let Some(parent) = apath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&apath)?;
            std::io::copy(&mut r, &mut out)?;
            count += 1;
        }
    }
    Ok(count)
}

// ── spaces / files / templates / saved queries (SPEC §10 ops) ─────

#[derive(Debug, Clone, Serialize)]
pub struct SpaceView {
    pub name: String,
    /// The space's dialect, when declared (SPEC §dialects).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    /// This machine's directory binding, when configured.
    pub dir: Option<String>,
    pub files: usize,
}

pub fn spaces(app: &App, opts: &Opts) -> Result<Vec<SpaceView>> {
    let reads = app.reads(opts)?;
    Ok(reads
        .spaces
        .values()
        .map(|(_, s)| SpaceView {
            name: s.name.clone(),
            dialect: s.dialect.clone(),
            dir: app
                .client
                .spaces
                .get(&s.name)
                .map(|p| p.display().to_string()),
            files: reads
                .files
                .keys()
                .filter(|(space, _)| *space == s.name)
                .count(),
        })
        .collect())
}

pub fn files(app: &App, opts: &Opts, space: Option<&str>) -> Result<Vec<FileJotView>> {
    let reads = app.reads(opts)?;
    Ok(reads
        .files
        .iter()
        .filter(|((s, _), _)| space.is_none_or(|w| s == w))
        .map(|(_, entry)| file_view(&reads, *entry))
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct TemplateView {
    pub profile: String,
    pub name: String,
    pub ext: String,
    pub content: String,
}

pub fn templates(app: &App, opts: &Opts) -> Result<Vec<TemplateView>> {
    let reads = app.reads(opts)?;
    // --profile all widens like every read listing (SPEC §11.1);
    // it must not silently narrow to the active profile.
    let scope = opts.read_scope(&app.client).map(str::to_string);
    Ok(reads
        .templates
        .iter()
        .filter(|((p, _), _)| scope.as_deref().is_none_or(|s| p == s))
        .map(|((p, name), (entry, t))| TemplateView {
            profile: p.clone(),
            name: name.clone(),
            ext: t.ext.clone(),
            content: String::from_utf8_lossy(&reads.bytes(*entry).unwrap_or_default()).into_owned(),
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct SavedQueryView {
    pub profile: String,
    pub name: String,
    pub args: Vec<String>,
}

pub fn saved_queries(app: &App, opts: &Opts) -> Result<Vec<SavedQueryView>> {
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client).map(str::to_string);
    Ok(reads
        .queries
        .iter()
        .filter(|((p, _), _)| scope.as_deref().is_none_or(|s| p == s))
        .map(|((p, name), (_, q))| SavedQueryView {
            profile: p.clone(),
            name: name.clone(),
            args: q.args.clone(),
        })
        .collect())
}

// ── today / agenda (SPEC §op-agenda) ──────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AgendaItem {
    pub profile: String,
    pub id: i64,
    pub todo: TodoBody,
    pub due_ns: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TodayView {
    pub date: String,
    pub jots: Vec<JotView>,
    pub due: Vec<AgendaItem>,
}

/// The local day's [start, end) in epoch nanoseconds.
fn day_bounds(date: &str) -> Result<(i64, i64)> {
    use chrono::{Local, NaiveDate, TimeZone};
    // Some zones have no midnight on DST spring-forward days —
    // slide to the first hour that exists. The day ends where the
    // next day starts, so 23- and 25-hour days bound correctly.
    let first_instant = |d: NaiveDate| {
        (0..=3).find_map(|h| {
            Local
                .from_local_datetime(&d.and_hms_opt(h, 0, 0)?)
                .earliest()
        })
    };
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| Error::Usage(format!("bad date {date:?} (want YYYY-MM-DD)")))?;
    let bad = || Error::Usage(format!("bad date {date:?}"));
    let start = first_instant(d).ok_or_else(bad)?;
    let end = first_instant(d.succ_opt().ok_or_else(bad)?).ok_or_else(bad)?;
    Ok((
        start.timestamp_nanos_opt().unwrap_or(i64::MAX),
        end.timestamp_nanos_opt().unwrap_or(i64::MAX),
    ))
}

/// User-facing date bounds are local (SPEC §10.8): a bare date
/// names the whole local day; instant forms are exact.
fn since_bound_ns(s: &str) -> Result<i64> {
    if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() {
        Ok(day_bounds(s)?.0)
    } else {
        due_to_ns_local(s).ok_or_else(|| Error::Usage(format!("bad date {s:?}")))
    }
}

fn until_bound_ns(u: &str) -> Result<i64> {
    if chrono::NaiveDate::parse_from_str(u, "%Y-%m-%d").is_ok() {
        Ok(day_bounds(u)?.1 - 1)
    } else {
        due_to_ns_local(u).ok_or_else(|| Error::Usage(format!("bad date {u:?}")))
    }
}

pub fn today(app: &App, opts: &Opts, date: Option<&str>) -> Result<TodayView> {
    let date = match date {
        Some(d) => d.to_string(),
        None => chrono::Local::now().format("%Y-%m-%d").to_string(),
    };
    let (start, end) = day_bounds(&date)?;
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    let mut jots = Vec::new();
    let mut due = Vec::new();
    for (entry, ident) in reads.list(scope) {
        if let Some(b) = reads.births.get(&entry) {
            if b.created_at_ns >= start && b.created_at_ns < end {
                jots.push(jot_view(&reads, entry));
            }
        }
        for t in reads.effective(entry).todos {
            if t.done {
                continue;
            }
            // Local reading: a date-only due lands on that local
            // day, not on the UTC day (SPEC §10.8).
            let Some(ns) = t.due.as_deref().and_then(due_to_ns_local) else {
                continue;
            };
            if ns >= start && ns < end {
                due.push(AgendaItem {
                    profile: ident.profile.clone(),
                    id: ident.id,
                    due_ns: Some(ns),
                    todo: t,
                });
            }
        }
    }
    due.sort_by_key(|i| i.due_ns);
    Ok(TodayView { date, jots, due })
}

/// Open todos across the scope: dated first, ascending by due,
/// then undated (SPEC §op-agenda).
pub fn agenda(app: &App, opts: &Opts, until: Option<&str>) -> Result<Vec<AgendaItem>> {
    let until_ns = until.map(until_bound_ns).transpose()?;
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    let mut dated = Vec::new();
    let mut undated = Vec::new();
    for (entry, ident) in reads.list(scope) {
        for t in reads.effective(entry).todos {
            if t.done {
                continue;
            }
            let due_ns = t.due.as_deref().and_then(due_to_ns_local);
            let item = AgendaItem {
                profile: ident.profile.clone(),
                id: ident.id,
                due_ns,
                todo: t,
            };
            match due_ns {
                Some(ns) => {
                    if until_ns.is_none_or(|u| ns <= u) {
                        dated.push(item);
                    }
                }
                // --until bounds the dated listing only; undated
                // todos always follow (SPEC §10.8).
                None => undated.push(item),
            }
        }
    }
    dated.sort_by_key(|i| i.due_ns);
    dated.extend(undated);
    Ok(dated)
}

// ── graph (SPEC §op-graph) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub reference: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

pub fn graph(app: &App, opts: &Opts) -> Result<GraphView> {
    let reads = app.reads(opts)?;
    let scope = opts.read_scope(&app.client);
    let mut g = GraphView {
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let display = |e: Uuid| -> Option<String> {
        if let Some(i) = reads.idents.get(&e) {
            Some(format!("--{}--{}", i.profile, i.id))
        } else {
            reads.file_paths.get(&e).map(|(s, p)| format!("///{s}/{p}"))
        }
    };
    for (entry, _) in reads.list(scope) {
        // A live path without an identity is a vault-integrity
        // violation (fsck's job to report) — degrade, don't abort.
        let Some(from) = display(entry) else { continue };
        g.nodes.push(GraphNode {
            reference: from.clone(),
            label: summary_line(&reads, entry),
        });
        let eff = reads.effective(entry);
        for target in eff.edges.keys() {
            let Some(u) = Uuid::parse_str(target).ok() else {
                continue;
            };
            if !reads.is_live(u) {
                continue;
            }
            if let Some(to) = display(u) {
                g.edges.push(GraphEdge {
                    from: from.clone(),
                    to,
                });
            }
        }
        // Resolvable file-reference edges (SPEC §op-graph).
        let default_space = reads
            .idents
            .get(&entry)
            .and_then(|i| reads.profile(&i.profile).ok())
            .and_then(|p| p.default_space.clone());
        for x in &eff.cross_file_references {
            if reads.files.contains_key(&(x.space.clone(), x.path.clone())) {
                g.edges.push(GraphEdge {
                    from: from.clone(),
                    to: format!("///{}/{}", x.space, x.path),
                });
            }
        }
        if let Some(space) = default_space {
            for p in &eff.file_references {
                if reads.files.contains_key(&(space.clone(), p.path.clone())) {
                    g.edges.push(GraphEdge {
                        from: from.clone(),
                        to: format!("///{space}/{}", p.path),
                    });
                }
            }
        }
    }
    for ((space, rel), entry) in &reads.files {
        let from = format!("///{space}/{rel}");
        g.nodes.push(GraphNode {
            reference: from.clone(),
            label: summary_line(&reads, *entry),
        });
        // File-jot outgoing edges: effective-index entry edges and
        // resolvable cross-file references, which include dialect
        // links and embeds (SPEC §file retrieval).
        let eff = reads.effective(*entry);
        for target in eff.edges.keys() {
            let Some(u) = Uuid::parse_str(target).ok() else {
                continue;
            };
            if !reads.is_live(u) {
                continue;
            }
            if let Some(to) = display(u) {
                g.edges.push(GraphEdge {
                    from: from.clone(),
                    to,
                });
            }
        }
        for x in &eff.cross_file_references {
            if reads.files.contains_key(&(x.space.clone(), x.path.clone())) {
                g.edges.push(GraphEdge {
                    from: from.clone(),
                    to: format!("///{}/{}", x.space, x.path),
                });
            }
        }
    }
    Ok(g)
}

/// Backlinks of a file reference (SPEC §op-backlinks): taxopsis
/// narrows over the file-reference predicates, cuj verifies each
/// candidate's effective index (the two array predicates are
/// document-scoped, not element-paired).
pub fn backlinks_file(
    app: &App,
    opts: &Opts,
    space: Option<&str>,
    path: &str,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<FindPage> {
    let (chain, sref) = taxopsis_at(app, opts)?;
    let reads = app.reads(opts)?;
    let space = resolve_space(&reads, app, opts, space)?;

    // The paired (space, path) disjunction over the file-reference
    // predicates; in dialect spaces the dialect's links and embeds
    // join it (SPEC §op-backlinks).
    let paired = |pred: &str| {
        and(
            atom(
                &format!("{pred}/space"),
                Op::Eq,
                QueryValue::Str(space.clone()),
            ),
            atom(
                &format!("{pred}/path"),
                Op::Eq,
                QueryValue::Str(path.to_string()),
            ),
        )
    };
    let expr = or(
        or(
            paired("jot/index/cross_file_references"),
            // Bare file refs wire as plain strings when unannotated
            // and as objects when annotated (SPEC §5.1), so the
            // exploded documents carry the path at either tuple.
            or(
                atom(
                    "jot/index/file_references",
                    Op::Eq,
                    QueryValue::Str(path.to_string()),
                ),
                atom(
                    "jot/index/file_references/path",
                    Op::Eq,
                    QueryValue::Str(path.to_string()),
                ),
            ),
        ),
        or(
            paired("obsidian/index/links"),
            paired("obsidian/index/embeds"),
        ),
    );
    let order = OrderBy {
        predicate: "cuj/id/id".into(),
        direction: Direction::Asc,
    };
    let page = query_page(&chain, &sref, &reads, &expr, &order, limit, cursor)?;
    let hits = page
        .hits
        .into_iter()
        .filter(|h| {
            // Verify against the source's effective index, which
            // for file-jot sources includes their dialect links
            // (SPEC §file retrieval).
            let (entry, default_space) = match h {
                HitTarget::Jot(j) => {
                    let Ok(entry) = Uuid::parse_str(&j.entry) else {
                        return false;
                    };
                    (
                        entry,
                        reads
                            .profile(&j.profile)
                            .ok()
                            .and_then(|p| p.default_space.clone()),
                    )
                }
                HitTarget::File(f) => {
                    let Some(entry) = reads.files.get(&(f.space.clone(), f.path.clone())) else {
                        return false;
                    };
                    (*entry, None)
                }
            };
            let eff = reads.effective(entry);
            let cross = eff
                .cross_file_references
                .iter()
                .any(|x| x.space == space && x.path == path);
            let bare = eff.file_references.iter().any(|p| p.path == path)
                && default_space.as_deref() == Some(space.as_str());
            cross || bare
        })
        .collect();
    Ok(FindPage {
        hits,
        cursor: page.cursor,
    })
}

// ── fsck (SPEC §10.36) ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FsckLayer {
    pub name: String,
    pub summary: String,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FsckOutcome {
    pub layers: Vec<FsckLayer>,
    /// cuj-level invariant violations; empty when substrate checks
    /// already failed (they are not run).
    pub problems: Vec<String>,
    pub jots: usize,
    pub profiles: usize,
}

impl FsckOutcome {
    pub fn substrate_errors(&self) -> usize {
        self.layers.iter().map(|l| l.errors.len()).sum()
    }

    pub fn clean(&self) -> bool {
        self.substrate_errors() == 0 && self.problems.is_empty()
    }
}

pub fn fsck(app: &App) -> Result<FsckOutcome> {
    let mut layers = Vec::new();
    let r = app.vault.fsck()?;
    layers.push(FsckLayer {
        name: "metatheca".into(),
        summary: format!("{} states, {} facts, {} blobs", r.states, r.facts, r.blobs),
        errors: r.errors,
    });
    let r = ext::open_taxopsis(app.root())?.fsck()?;
    layers.push(FsckLayer {
        name: "taxopsis".into(),
        summary: format!("{} index states", r.index_states),
        errors: r.errors,
    });
    let r = ext::open_logopsis(app.root())?.fsck()?;
    layers.push(FsckLayer {
        name: "logopsis".into(),
        summary: format!("{} search states", r.search_states),
        errors: r.errors,
    });
    let r = ext::open_semopsis(app.root())?.fsck()?;
    layers.push(FsckLayer {
        name: "semopsis".into(),
        summary: format!("{} search states", r.search_states),
        errors: r.errors,
    });
    let mut out = FsckOutcome {
        layers,
        problems: Vec::new(),
        jots: 0,
        profiles: 0,
    };
    if out.substrate_errors() > 0 {
        return Ok(out);
    }

    // cuj-level invariants (SPEC §10.36).
    let reads = app.reads(&Opts::default())?;
    out.jots = reads.idents.len();
    out.profiles = reads.profiles.len();
    // Identity uniqueness fell out of id_map construction; recheck
    // explicitly by counting.
    if reads.id_map.len() != reads.idents.len() {
        out.problems.push(format!(
            "identity collision: {} identities over {} entries",
            reads.id_map.len(),
            reads.idents.len()
        ));
    }
    for (entry, ident) in reads.list(None) {
        if reads.bytes(entry).is_err() {
            out.problems.push(format!(
                "live jot --{}--{} has no content blob",
                ident.profile, ident.id
            ));
        }
        let expected = jot_path(ident);
        let paths = app
            .vault
            .paths_for(&entry.hyphenated().to_string())
            .unwrap_or_default();
        if !paths.contains(&expected) {
            out.problems.push(format!(
                "jot --{}--{}: core/path disagrees with cuj/id (expected {expected})",
                ident.profile, ident.id
            ));
        }
    }
    // Every live path must belong to an entry some projection
    // accounts for (SPEC §10.36): a jot path without cuj/id — or
    // any other unaccounted binding — is dark data no read can
    // reach.
    let mut known: std::collections::HashSet<Uuid> = reads
        .idents
        .keys()
        .copied()
        .chain(reads.attachments.keys().copied())
        .chain(reads.files.values().copied())
        .chain(reads.spaces.values().map(|(e, _)| *e))
        .chain(reads.profiles.values().map(|(e, _)| *e))
        .collect();
    for (root_entry, _) in app.vault.current_facts(facts::K_CUJ_VAULT)? {
        known.insert(root_entry);
    }
    for (path, entry) in app.vault.ls(None)? {
        let Ok(u) = Uuid::parse_str(&entry) else {
            continue;
        };
        if !known.contains(&u) {
            out.problems
                .push(format!("live path {path:?} bound to an unaccounted entry"));
        }
    }

    // Attachment path shape (SPEC §10.36): a live attachment's path
    // is exactly <profile>/att/<id>/<filename> of its owning live
    // jot; detached or orphaned attachments carry no live path.
    for (att_entry, att) in &reads.attachments {
        let paths = app
            .vault
            .paths_for(&att_entry.hyphenated().to_string())
            .unwrap_or_default();
        let owner = Uuid::parse_str(&att.jot)
            .ok()
            .filter(|u| reads.is_live(*u))
            .and_then(|u| reads.idents.get(&u));
        match owner {
            Some(ident) if !att.detached => {
                let expected = att_path(&ident.profile, ident.id, &att.filename);
                if !paths.contains(&expected) {
                    out.problems.push(format!(
                        "attachment {:?} of --{}--{}: expected path {expected} not bound",
                        att.filename, ident.profile, ident.id
                    ));
                }
                if let Some(stray) = paths.iter().find(|p| **p != expected) {
                    out.problems
                        .push(format!("attachment {:?}: stray path {stray}", att.filename));
                }
            }
            _ => {
                if !paths.is_empty() {
                    out.problems.push(format!(
                        "attachment {:?}: detached/orphaned but still bound at {}",
                        att.filename,
                        paths.join(", ")
                    ));
                }
            }
        }
    }
    // 0.5 registries: name/path agreement and space membership.
    for (name, (entry, s)) in &reads.spaces {
        let ok = facts::validate_short_name(name, "space name").is_ok()
            && &s.name == name
            && app
                .vault
                .paths_for(&entry.hyphenated().to_string())
                .unwrap_or_default()
                .contains(&facts::space_registry_path(name));
        if !ok {
            out.problems
                .push(format!("invalid space registry entry: {name:?}"));
        }
    }
    let mut unregistered: Vec<&str> = reads
        .files
        .keys()
        .map(|(space, _)| space.as_str())
        .filter(|s| !reads.spaces.contains_key(*s))
        .collect();
    unregistered.sort_unstable();
    unregistered.dedup();
    for s in unregistered {
        out.problems
            .push(format!("live file paths under unregistered space {s:?}"));
    }
    for ((profile, name), (entry, t)) in &reads.templates {
        let ok = &t.profile == profile
            && &t.name == name
            && app
                .vault
                .paths_for(&entry.hyphenated().to_string())
                .unwrap_or_default()
                .contains(&facts::template_path(profile, name));
        if !ok {
            out.problems
                .push(format!("invalid template entry: {profile}/{name}"));
        }
    }
    for ((profile, name), (entry, q)) in &reads.queries {
        let ok = &q.profile == profile
            && &q.name == name
            && app
                .vault
                .paths_for(&entry.hyphenated().to_string())
                .unwrap_or_default()
                .contains(&facts::query_path(profile, name));
        if !ok {
            out.problems
                .push(format!("invalid saved-query entry: {profile}/{name}"));
        }
    }
    for (name, (entry, p)) in &reads.profiles {
        if crate::facts::validate_profile_name(name).is_err() || &p.name != name {
            out.problems
                .push(format!("invalid profile registry entry: {name:?}"));
        }
        let max_id = reads
            .idents
            .values()
            .filter(|i| &i.profile == name)
            .map(|i| i.id)
            .max()
            .unwrap_or(0);
        let counter = reads.counters.get(entry).copied().unwrap_or(0);
        if counter < max_id {
            out.problems.push(format!(
                "profile {name:?}: counter {counter} below highest allocated id {max_id}"
            ));
        }
    }
    Ok(out)
}
