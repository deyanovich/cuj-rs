//! Jot Syntax binding (SPEC §6): parser invocation, the
//! ParseResult -> jot/index mapping, due-time normalization at
//! creation, and construct rewriting for promote/demote/todo-done.

use chrono::{Local, Offset};
use jots::ParseResult;
use serde_json::Value;

use metatheca::Uuid;

use crate::facts::{
    self, EdgeAnnot, EdgeBody, FileRefBody, InterJotResourceBody, ParsedIndex, ProfileConfig,
    ResourceBody, TodoBody, UrlBody, XFileRefBody,
};

/// Parser version recorded in jot/index (SPEC §5.1). The jots crate
/// version pins the grammar revision.
pub fn jots_version() -> String {
    format!("jots {}", jots::VERSION)
}

/// Build the parser config from a profile's parse flags (SPEC §6.1).
/// Location/raw emission is always on: rewrites and todo matching
/// need spans.
pub fn parser_config(profile: &ProfileConfig) -> jots::Config {
    let mut cfg = jots::Config::new();
    cfg.set_bool("emit_loc", true);
    for (k, v) in &profile.parse {
        if let Value::Bool(b) = v {
            cfg.set_bool(k, *b);
        }
    }
    cfg
}

pub fn parse(text: &str, profile: &ProfileConfig) -> ParseResult {
    jots::parse(text, &parser_config(profile))
}

fn todo_body(t: &jots::Todo, done: bool) -> TodoBody {
    TodoBody {
        raw: t.raw.clone().unwrap_or_default(),
        action: if t.action.is_empty() {
            None
        } else {
            Some(t.action.clone())
        },
        description: t.description.clone(),
        importance: t.importance,
        due: t.due.clone(),
        done,
        completed: t.completed.clone(),
    }
}

/// The edge annotations of a parsed reference-kind construct,
/// canonicalized like jot-level tags and categories (lowercase).
fn annot_of(
    category: &Option<String>,
    tags: &[String],
    bookmark: &Option<String>,
    fragment: &Option<jots::Fragment>,
    range: &Option<jots::BookmarkRange>,
) -> EdgeAnnot {
    EdgeAnnot {
        category: category.as_ref().map(|c| c.to_lowercase()),
        tags: sorted_dedup(tags.iter().map(|t| t.to_lowercase()).collect()),
        bookmark: bookmark.clone(),
        fragment: fragment.as_ref().map(|f| f.raw.clone()),
        bookmark_range: range.as_ref().map(|r| facts::BookmarkRangeAnnot {
            start: r.start_bookmark.clone(),
            end: r.end_bookmark.clone(),
        }),
    }
}

fn sorted_dedup(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// Map a ParseResult onto the jot/index fact body (SPEC §5.1),
/// resolving references to entries at write time (SPEC §3.4) via
/// the caller-supplied lookup.
pub fn to_parsed_index(
    r: &ParseResult,
    own_profile: &str,
    resolve: &dyn Fn(&str, i64) -> Option<Uuid>,
) -> ParsedIndex {
    // Constructs nested inside todos contribute to the jot-level
    // arrays like top-level ones (SPEC §5.1) — the todo body
    // records raw text; blockers stay todo-local machinery.
    let todos_all = || r.todos.iter().chain(r.done_todos.iter());
    let mut idx = ParsedIndex {
        jots_version: jots_version(),
        tags: sorted_dedup(
            r.tags
                .iter()
                .chain(todos_all().flat_map(|t| &t.tags))
                .map(|t| t.to_lowercase())
                .collect(),
        ),
        categories: sorted_dedup(
            r.categories
                .iter()
                .chain(todos_all().flat_map(|t| &t.categories))
                .map(|c| c.to_lowercase())
                .collect(),
        ),
        ..Default::default()
    };
    let mut edge = |profile: &str, raw_id: &str, annot: EdgeAnnot| {
        let resolved = raw_id
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0 && facts::validate_profile_name(profile).is_ok())
            .and_then(|n| resolve(profile, n).map(|e| (e, n)));
        match resolved {
            Some((entry, n)) => {
                let b = EdgeBody {
                    entry: entry.hyphenated().to_string(),
                    written_as: format!("{profile}:{n}"),
                    annot,
                };
                if !idx.references.contains(&b) {
                    idx.references.push(b);
                }
            }
            None => {
                let raw = if profile == own_profile {
                    raw_id.to_string()
                } else {
                    format!("{profile}--{raw_id}")
                };
                if !idx.unresolved_references.contains(&raw) {
                    idx.unresolved_references.push(raw);
                }
            }
        }
    };
    for x in r
        .references
        .iter()
        .chain(todos_all().flat_map(|t| &t.references))
    {
        edge(
            own_profile,
            &x.id,
            annot_of(&x.category, &x.tags, &x.bookmark, &x.fragment, &x.bookmark_range),
        );
    }
    for x in r
        .cross_references
        .iter()
        .chain(todos_all().flat_map(|t| &t.cross_references))
    {
        edge(
            &x.space,
            &x.id,
            annot_of(&x.category, &x.tags, &x.bookmark, &x.fragment, &x.bookmark_range),
        );
    }
    for x in r
        .file_references
        .iter()
        .chain(todos_all().flat_map(|t| &t.file_references))
    {
        let b = FileRefBody {
            path: x.filepath.clone(),
            annot: annot_of(&x.category, &x.tags, &x.bookmark, &x.fragment, &x.bookmark_range),
        };
        if !idx.file_references.contains(&b) {
            idx.file_references.push(b);
        }
    }
    for x in r
        .cross_file_references
        .iter()
        .chain(todos_all().flat_map(|t| &t.cross_file_references))
    {
        let b = XFileRefBody {
            space: x.space.clone(),
            path: x.filepath.clone(),
            annot: annot_of(&x.category, &x.tags, &x.bookmark, &x.fragment, &x.bookmark_range),
        };
        if !idx.cross_file_references.contains(&b) {
            idx.cross_file_references.push(b);
        }
    }
    for x in r
        .resources
        .iter()
        .chain(todos_all().flat_map(|t| &t.resources))
    {
        let b = ResourceBody {
            name: x.filename.clone(),
            annot: annot_of(&x.category, &x.tags, &x.bookmark, &x.fragment, &x.bookmark_range),
        };
        if !idx.resources.contains(&b) {
            idx.resources.push(b);
        }
    }
    for x in r
        .inter_jot_resources
        .iter()
        .chain(todos_all().flat_map(|t| &t.inter_jot_resources))
    {
        // The owning jot resolves at write time like reference
        // edges (SPEC §3.4); filepath-located resources have no
        // entry spine and keep entry = None.
        let entry = x.id.as_ref().and_then(|raw| {
            let profile = x.space.as_deref().unwrap_or(own_profile);
            raw.parse::<i64>()
                .ok()
                .filter(|n| *n > 0 && facts::validate_profile_name(profile).is_ok())
                .and_then(|n| resolve(profile, n))
                .map(|e| e.hyphenated().to_string())
        });
        let b = InterJotResourceBody {
            id: x.id.clone(),
            space: x.space.clone(),
            entry,
            name: x.filename.clone(),
            annot: annot_of(
                &x.category,
                &x.tags,
                &x.resource_bookmark,
                &x.resource_fragment,
                &x.resource_bookmark_range,
            ),
        };
        if !idx.inter_jot_resources.contains(&b) {
            idx.inter_jot_resources.push(b);
        }
    }
    for x in r.urls.iter().chain(todos_all().flat_map(|t| &t.urls)) {
        let b = UrlBody {
            url: x.url.clone(),
            label: x.label.clone(),
        };
        if !idx.urls.contains(&b) {
            idx.urls.push(b);
        }
    }
    for x in &r.bookmarks {
        if !idx.bookmarks.contains(&x.name) {
            idx.bookmarks.push(x.name.clone());
        }
    }
    for t in &r.todos {
        let b = todo_body(t, false);
        if !idx.todos.contains(&b) {
            idx.todos.push(b);
        }
    }
    for t in &r.done_todos {
        let b = todo_body(t, true);
        if !idx.todos.contains(&b) {
            idx.todos.push(b);
        }
    }
    idx.diagnostics_count = r.diagnostics.len() as u64;
    idx
}

/// Due-time normalization at creation (SPEC §6.6): a due specifier
/// carrying a time-of-day but no UTC offset gets the client's
/// current offset appended in the content itself. Date-only dues
/// are untouched. Returns the (possibly rewritten) content.
pub fn normalize_due_times(text: &str, profile: &ProfileConfig) -> String {
    let r = parse(text, profile);
    // Collect (absolute_byte_pos_of_due_end, offset_string) edits.
    let mut edits: Vec<(usize, String)> = Vec::new();
    let offset = local_offset_string();
    let mut collect = |raw: &Option<String>, loc: &Option<jots::Loc>, due: &Option<String>| {
        let (Some(raw), Some(loc), Some(due)) = (raw, loc, due) else {
            return;
        };
        if !due_needs_offset(due) {
            return;
        }
        // Find the due substring inside the raw construct text and
        // compute its absolute end position.
        if let Some(rel) = raw.find(due.as_str()) {
            edits.push((loc.start + rel + due.len(), offset.clone()));
        }
    };
    for t in &r.todos {
        collect(&t.raw, &t.loc, &t.due);
    }
    for t in &r.done_todos {
        collect(&t.raw, &t.loc, &t.due);
    }
    if edits.is_empty() {
        return text.to_string();
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0)); // apply back-to-front
    let mut out = text.to_string();
    for (pos, off) in edits {
        if pos <= out.len() {
            out.insert_str(pos, &off);
        }
    }
    out
}

/// A due specifier needs an offset iff it has a time-of-day (a 'T'
/// separator) and no trailing zone (Z or +/-HH:MM).
fn due_needs_offset(due: &str) -> bool {
    if !due.contains('T') {
        return false;
    }
    if due.ends_with('Z') || due.ends_with('z') {
        return false;
    }
    // +HH:MM / -HH:MM suffix after the time part?
    let bytes = due.as_bytes();
    if due.len() >= 6 {
        let tail = &bytes[due.len() - 6..];
        if (tail[0] == b'+' || tail[0] == b'-')
            && tail[1].is_ascii_digit()
            && tail[2].is_ascii_digit()
            && tail[3] == b':'
            && tail[4].is_ascii_digit()
            && tail[5].is_ascii_digit()
        {
            return false;
        }
    }
    true
}

fn local_offset_string() -> String {
    let off = Local::now().offset().fix().local_minus_utc();
    let sign = if off < 0 { '-' } else { '+' };
    let off = off.abs();
    format!("{sign}{:02}:{:02}", off / 3600, (off % 3600) / 60)
}

/// Parse an ISO-8601 due specifier into epoch nanoseconds (UTC) for
/// the taxopsis jot/todo-due tuple (SPEC §9.2): date-only converts
/// to the starting instant of that date in UTC.
pub fn due_to_ns(due: &str) -> Option<i64> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(due) {
        return dt.with_timezone(&Utc).timestamp_nanos_opt();
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(due, "%Y-%m-%dT%H:%M:%S") {
        return Utc.from_utc_datetime(&ndt).timestamp_nanos_opt();
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(due, "%Y-%m-%dT%H:%M") {
        return Utc.from_utc_datetime(&ndt).timestamp_nanos_opt();
    }
    if let Ok(nd) = NaiveDate::parse_from_str(due, "%Y-%m-%d") {
        return Utc
            .from_utc_datetime(&nd.and_hms_opt(0, 0, 0)?)
            .timestamp_nanos_opt();
    }
    None
}

/// The local-time reading of a due specifier, for user-facing day
/// arithmetic (`today`, `agenda`; SPEC §10.8 speaks in local
/// dates). Date-only and offset-less forms are taken as local;
/// explicit offsets are exact. The indexed tuple stays UTC
/// (`due_to_ns`, SPEC §9.2) — only display-side comparisons use
/// this.
pub fn due_to_ns_local(due: &str) -> Option<i64> {
    use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(due) {
        return dt.with_timezone(&Utc).timestamp_nanos_opt();
    }
    let naive = NaiveDateTime::parse_from_str(due, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(due, "%Y-%m-%dT%H:%M"))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(due, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })?;
    Local
        .from_local_datetime(&naive)
        .earliest()?
        .timestamp_nanos_opt()
}

/// Rewrite every whole-construct occurrence of a tag/category in
/// content (SPEC §10.19). `from_raw` is the exact construct text
/// (e.g. "..urgent"), `to` its replacement. The parser exposes no
/// per-tag spans, so this is a boundary-checked text scan mirroring
/// the Jot Syntax boundary model: left boundary start-of-text or
/// whitespace, right boundary end / whitespace / trim-set char.
/// A trim char that opens a construct continuation (`::` deepens a
/// category, `..` chains a tag) is NOT a boundary: the match is a
/// prefix of a longer construct and rewriting it would corrupt the
/// longer one. Todos are rewritten by parsed span (`rewrite_todo`),
/// not through this scan.
pub fn rewrite_constructs(text: &str, from_raw: &str, to: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut rest = text;
    let mut abs = 0usize;
    while let Some(pos) = rest.find(from_raw) {
        let start = abs + pos;
        let end = start + from_raw.len();
        let left_ok = start == 0
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let tail = &text[end..];
        let right_ok = tail.is_empty()
            || (!tail.starts_with("::")
                && !tail.starts_with("..")
                && tail
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_whitespace() || ".,;:!?)]}\"'".contains(c)));
        out.push_str(&rest[..pos]);
        if left_ok && right_ok {
            out.push_str(to);
            count += 1;
        } else {
            out.push_str(from_raw);
        }
        rest = &rest[pos + from_raw.len()..];
        abs = end;
    }
    out.push_str(rest);
    (out, count)
}

/// Rewrite the open todos whose raw construct text equals
/// `from_raw`, by parsed span. Todo raws contain whitespace, so a
/// text scan cannot tell `??call bob` from the distinct todo
/// `??call bob today`; the parser's spans can.
pub fn rewrite_todo(
    text: &str,
    profile: &ProfileConfig,
    from_raw: &str,
    to: &str,
) -> (String, usize) {
    let r = parse(text, profile);
    let mut spans: Vec<(usize, usize)> = r
        .todos
        .iter()
        .filter(|t| t.raw.as_deref() == Some(from_raw))
        .filter_map(|t| t.loc.as_ref().map(|l| (l.start, l.end)))
        .collect();
    if spans.is_empty() {
        return (text.to_string(), 0);
    }
    spans.sort_by_key(|s| std::cmp::Reverse(s.0));
    let mut out = text.to_string();
    let mut count = 0usize;
    for (start, end) in spans {
        if start <= end && end <= out.len() && &out[start..end] == from_raw {
            out.replace_range(start..end, to);
            count += 1;
        }
    }
    (out, count)
}

/// Guess a MIME type for jot content by extension (SPEC §10.2).
pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "md" => "text/markdown",
        "txt" => "text/plain",
        "tex" => "application/x-tex",
        "html" => "text/html",
        _ => "text/plain",
    }
}

pub fn mime_for_bytes(name: &str, bytes: &[u8]) -> String {
    if let Some(k) = infer::get(bytes) {
        return k.mime_type().to_string();
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "md" => "text/markdown".into(),
        "txt" => "text/plain".into(),
        "html" | "htm" => "text/html".into(),
        "json" => "application/json".into(),
        _ if std::str::from_utf8(bytes).is_ok() => "text/plain".into(),
        _ => "application/octet-stream".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_skips_deeper_category() {
        // ::work inside ::work::alpha is a prefix of a longer
        // construct, not an occurrence.
        let (out, n) = rewrite_constructs("::work ::work::alpha", "::work", "..work");
        assert_eq!(out, "..work ::work::alpha");
        assert_eq!(n, 1);
    }

    #[test]
    fn rewrite_skips_chained_tag() {
        let (out, n) = rewrite_constructs("..work..extra", "..work", "::work");
        assert_eq!(out, "..work..extra");
        assert_eq!(n, 0);
    }

    #[test]
    fn rewrite_accepts_sentence_punctuation() {
        // Trailing '.' is a boundary; a non-whitespace left
        // neighbor is not an occurrence.
        let (out, n) = rewrite_constructs("see ..urgent. and (..urgent)", "..urgent", "::urgent");
        assert_eq!(out, "see ::urgent. and (..urgent)");
        assert_eq!(n, 1);
    }

    #[test]
    fn rewrite_accepts_single_colon_boundary() {
        let (out, n) = rewrite_constructs("..work: done", "..work", "::work");
        assert_eq!(out, "::work: done");
        assert_eq!(n, 1);
    }

    #[test]
    fn rewrite_todo_leaves_longer_raw_alone() {
        // A raw that is a whitespace-boundary prefix of a distinct
        // todo must not complete it.
        let text = "??call bob today\n??call bob\n";
        let profile = ProfileConfig::new("t");
        let (out, n) = rewrite_todo(text, &profile, "??call bob", "??.call bob");
        assert_eq!(out, "??call bob today\n??.call bob\n");
        assert_eq!(n, 1);
    }

    #[test]
    fn rewrite_todo_missing_raw_is_zero() {
        let profile = ProfileConfig::new("t");
        let (out, n) = rewrite_todo("??other things\n", &profile, "??call bob", "??.call bob");
        assert_eq!(out, "??other things\n");
        assert_eq!(n, 0);
    }
}
