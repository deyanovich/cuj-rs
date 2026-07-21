//! The Obsidian space dialect (SPEC §obsidian/index): parse an
//! `md` file's Obsidian metadata — YAML frontmatter, inline
//! `#tags`, `[[wikilinks]]` / `![[embeds]]`, checkbox tasks — and
//! resolve link targets within the containing space.
//!
//! The dialect never touches `jot/index` (the Jot Syntax parse)
//! and covers Obsidian's stock metadata surface only; plugin
//! conventions are out of scope (SPEC §obsidian/index).

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::facts::{DialectLink, DialectTask, ObsidianIndex};

/// The version string recorded as `dialect_version` (SPEC
/// §obsidian/index): bodies are a deterministic function of
/// (content blob, dialect parser version).
pub fn dialect_version() -> String {
    format!("cuj-obsidian {}", env!("CARGO_PKG_VERSION"))
}

/// The space-independent parse of one file's Obsidian metadata;
/// link targets are raw (as written) until resolved against the
/// space's file set.
#[derive(Debug, Default, PartialEq)]
pub struct Parse {
    /// Flat tags, canonical-lowercase, deduplicated, sorted.
    pub tags: Vec<String>,
    /// Nested tags (`#a/b`) as `a::b` categories, canonicalized.
    pub categories: Vec<String>,
    /// Frontmatter aliases, as written, source order, deduped.
    pub aliases: Vec<String>,
    /// Wikilink targets as written, content order, deduped.
    pub link_targets: Vec<String>,
    /// Embed targets as written, content order, deduped.
    pub embed_targets: Vec<String>,
    /// Frontmatter minus the consumed `tags`/`aliases` keys.
    pub properties: Map<String, Value>,
    pub tasks: Vec<DialectTask>,
}

pub fn parse(text: &str) -> Parse {
    let mut p = Parse::default();
    let body = match split_frontmatter(text) {
        Some((yaml, body)) => {
            parse_frontmatter(yaml, &mut p);
            body
        }
        None => text,
    };
    parse_body(body, &mut p);
    p.tags = sorted_dedup(p.tags);
    p.categories = sorted_dedup(p.categories);
    dedup_keep_order(&mut p.aliases);
    dedup_keep_order(&mut p.link_targets);
    dedup_keep_order(&mut p.embed_targets);
    p
}

/// Resolve one raw wikilink/embed target within a space (SPEC
/// §obsidian/index): strip the alias pipe and any heading or
/// block anchor, then (a) exact space-relative path, as written
/// and with an appended `.md`, else (b) unique case-insensitive
/// basename match among the space's files.
pub fn resolve_target(target: &str, files: &BTreeSet<String>) -> Option<String> {
    let t = target.split('|').next().unwrap_or("");
    let t = t.split('#').next().unwrap_or("").trim();
    if t.is_empty() {
        return None;
    }
    if files.contains(t) {
        return Some(t.to_string());
    }
    let with_md = format!("{t}.md");
    if files.contains(&with_md) {
        return Some(with_md);
    }
    let t_low = t.to_lowercase();
    let md_low = format!("{t_low}.md");
    let mut hit: Option<&String> = None;
    for f in files {
        let base = f.rsplit('/').next().unwrap_or(f).to_lowercase();
        if base == t_low || base == md_low {
            if hit.is_some() {
                return None; // not unique
            }
            hit = Some(f);
        }
    }
    hit.cloned()
}

/// Assemble the `obsidian/index` fact body for one file: parse,
/// then resolve links and embeds against the space's file set.
pub fn index_for(text: &str, space: &str, files: &BTreeSet<String>) -> ObsidianIndex {
    let p = parse(text);
    let mut o = ObsidianIndex {
        dialect_version: dialect_version(),
        tags: p.tags,
        categories: p.categories,
        aliases: p.aliases,
        properties: p.properties,
        tasks: p.tasks,
        ..Default::default()
    };
    for raw in p.link_targets {
        match resolve_target(&raw, files) {
            Some(path) => o.links.push(DialectLink {
                space: space.to_string(),
                path,
                written_as: raw,
            }),
            None => o.unresolved_links.push(raw),
        }
    }
    for raw in p.embed_targets {
        match resolve_target(&raw, files) {
            Some(path) => o.embeds.push(DialectLink {
                space: space.to_string(),
                path,
                written_as: raw,
            }),
            None => o.unresolved_embeds.push(raw),
        }
    }
    o
}

// ── frontmatter ─────────────────────────────────────────────────

/// Split a leading `---` YAML frontmatter block off the content:
/// the opener is `---` as the very first line, the closer `---`
/// or `...` on its own line.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    let rest = rest.strip_prefix('\n')?;
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn parse_frontmatter(yaml: &str, p: &mut Parse) {
    let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml) else {
        return;
    };
    let Value::Object(map) = yaml_to_json(&v) else {
        return;
    };
    for (key, value) in map {
        match key.as_str() {
            // Obsidian's `tag` singular is a legacy synonym.
            "tags" | "tag" => {
                for t in string_items(&value) {
                    push_tag(t.trim().trim_start_matches('#'), p);
                }
            }
            // `alias` singular likewise.
            "aliases" | "alias" => {
                p.aliases.extend(string_items(&value));
            }
            _ => {
                p.properties.insert(key, value);
            }
        }
    }
}

/// Frontmatter values that hold name lists: a sequence of
/// scalars, or a single comma-separated string.
fn string_items(v: &Value) -> Vec<String> {
    match v {
        Value::Array(items) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .collect(),
        Value::String(s) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// Map YAML to canonical JSON: tags/anchors are resolved by the
/// YAML parser; non-string mapping keys take their scalar string
/// form (SPEC §obsidian/index: keys as written).
fn yaml_to_json(v: &serde_yaml_ng::Value) -> Value {
    use serde_yaml_ng::Value as Y;
    match v {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(*b),
        Y::Number(n) => serde_json::to_value(n).unwrap_or(Value::Null),
        Y::String(s) => Value::String(s.clone()),
        Y::Sequence(items) => Value::Array(items.iter().map(yaml_to_json).collect()),
        Y::Mapping(m) => {
            let mut out = Map::new();
            for (k, val) in m {
                let key = match k {
                    Y::String(s) => s.clone(),
                    Y::Bool(b) => b.to_string(),
                    Y::Number(n) => n.to_string(),
                    Y::Null => "null".to_string(),
                    other => serde_yaml_ng::to_string(other)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default(),
                };
                out.insert(key, yaml_to_json(val));
            }
            Value::Object(out)
        }
        Y::Tagged(t) => yaml_to_json(&t.value),
    }
}

// ── body ────────────────────────────────────────────────────────

fn parse_body(body: &str, p: &mut Parse) {
    let mut fence: Option<&'static str> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        // Fenced code blocks are inert (Obsidian ignores tags and
        // links inside them).
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        scan_task(line, p);
        scan_links(line, p);
        scan_tags(line, p);
    }
}

/// `- [ ]` / `- [x]` checkbox list items (SPEC §obsidian/index):
/// `raw` is the item text after the marker.
fn scan_task(line: &str, p: &mut Parse) {
    let t = line.trim_start();
    let Some(rest) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    else {
        return;
    };
    let Some(boxed) = rest.strip_prefix('[') else {
        return;
    };
    let mut chars = boxed.chars();
    let (mark, tail) = match (chars.next(), chars.as_str().strip_prefix("] ")) {
        (Some(m), Some(tail)) => (m, tail),
        _ => return,
    };
    let done = match mark {
        ' ' => false,
        'x' | 'X' => true,
        _ => return,
    };
    p.tasks.push(DialectTask {
        raw: tail.trim_end().to_string(),
        done,
    });
}

/// `[[target]]`, `[[target|alias]]`, `![[embed]]` — targets kept
/// as written (anchor and all); resolution happens per space.
fn scan_links(line: &str, p: &mut Parse) {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(start) = line[i..].find("[[").map(|o| i + o) else {
            break;
        };
        let Some(end) = line[start + 2..].find("]]").map(|o| start + 2 + o) else {
            break;
        };
        let target = &line[start + 2..end];
        if !target.is_empty() && !target.contains('[') {
            let embed = start > 0 && bytes[start - 1] == b'!';
            if embed {
                p.embed_targets.push(target.to_string());
            } else {
                p.link_targets.push(target.to_string());
            }
        }
        i = end + 2;
    }
}

/// Inline `#tag` tokens: preceded by start-of-line or whitespace,
/// payload of word chars / `-` / `/` with at least one
/// non-numeric character (Obsidian's tag rule). Nested tags
/// become categories with `/` rewritten to `::` (SPEC
/// §obsidian/index).
fn scan_tags(line: &str, p: &mut Parse) {
    let mut prev: Option<char> = None;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '#' && prev.is_none_or(char::is_whitespace) {
            let rest = &line[i + 1..];
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '/'))
                .unwrap_or(rest.len());
            let payload = &rest[..end];
            if !payload.is_empty() && payload.chars().any(|c| !c.is_ascii_digit()) {
                push_tag(payload, p);
            }
            // Skip past the payload so `#a/b#c` doesn't re-match.
            while chars.peek().is_some_and(|(j, _)| *j < i + 1 + end) {
                chars.next();
            }
            prev = Some('x');
            continue;
        }
        prev = Some(c);
    }
}

/// Route one tag payload: flat tags land in `tags`, nested
/// (`a/b`) in `categories` as `a::b`, both canonical-lowercase.
fn push_tag(payload: &str, p: &mut Parse) {
    let payload = payload.trim_matches('/');
    if payload.is_empty() {
        return;
    }
    let canon = payload.to_lowercase();
    if canon.contains('/') {
        let cat = canon
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("::");
        if !cat.is_empty() {
            p.categories.push(cat);
        }
    } else {
        p.tags.push(canon);
    }
}

// ── helpers ─────────────────────────────────────────────────────

fn sorted_dedup(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

fn dedup_keep_order(v: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn frontmatter_tags_aliases_properties() {
        let p = parse(
            "---\ntags: [Project, work/client]\naliases:\n  - My Note\nstatus: draft\nrating: 5\n---\nBody.\n",
        );
        assert_eq!(p.tags, vec!["project"]);
        assert_eq!(p.categories, vec!["work::client"]);
        assert_eq!(p.aliases, vec!["My Note"]);
        assert_eq!(p.properties.get("status"), Some(&Value::from("draft")));
        assert_eq!(p.properties.get("rating"), Some(&Value::from(5)));
        assert!(!p.properties.contains_key("tags"));
    }

    #[test]
    fn frontmatter_comma_string_tags() {
        let p = parse("---\ntags: alpha, Beta\n---\n");
        assert_eq!(p.tags, vec!["alpha", "beta"]);
    }

    #[test]
    fn inline_tags_flat_and_nested() {
        let p = parse("Met the team #meeting and #work/projects/alpha today. #123 no.\n");
        assert_eq!(p.tags, vec!["meeting"]);
        assert_eq!(p.categories, vec!["work::projects::alpha"]);
    }

    #[test]
    fn tag_needs_boundary() {
        let p = parse("see issue#42 and https://x.example/#frag\n");
        assert!(p.tags.is_empty());
        assert!(p.categories.is_empty());
    }

    #[test]
    fn wikilinks_and_embeds() {
        let p = parse("See [[Other Note]] and [[sub/Deep#Heading|label]], plus ![[img.png]].\n");
        assert_eq!(p.link_targets, vec!["Other Note", "sub/Deep#Heading|label"]);
        assert_eq!(p.embed_targets, vec!["img.png"]);
    }

    #[test]
    fn fenced_code_is_inert() {
        let p = parse("```\n#nottag [[NotLink]]\n- [ ] not a task\n```\n#real\n");
        assert_eq!(p.tags, vec!["real"]);
        assert!(p.link_targets.is_empty());
        assert!(p.tasks.is_empty());
    }

    #[test]
    fn tasks() {
        let p = parse("- [ ] open thing\n- [x] done thing\n* [X] also done\n- [>] skipped\n");
        assert_eq!(
            p.tasks,
            vec![
                DialectTask {
                    raw: "open thing".into(),
                    done: false
                },
                DialectTask {
                    raw: "done thing".into(),
                    done: true
                },
                DialectTask {
                    raw: "also done".into(),
                    done: true
                },
            ]
        );
    }

    #[test]
    fn resolve_exact_then_basename() {
        let fs = files(&["notes/Other Note.md", "img.png", "a/b.md", "c/B.md"]);
        assert_eq!(
            resolve_target("notes/Other Note", &fs),
            Some("notes/Other Note.md".into())
        );
        assert_eq!(
            resolve_target("Other Note", &fs),
            Some("notes/Other Note.md".into())
        );
        assert_eq!(resolve_target("img.png", &fs), Some("img.png".into()));
        // ambiguous basename: a/b.md vs c/B.md
        assert_eq!(resolve_target("b", &fs), None);
        assert_eq!(resolve_target("missing", &fs), None);
        // anchor and alias stripping
        assert_eq!(
            resolve_target("Other Note#Sec|shown", &fs),
            Some("notes/Other Note.md".into())
        );
        assert_eq!(resolve_target("#self-anchor", &fs), None);
    }

    #[test]
    fn index_for_resolves_and_records_unresolved() {
        let fs = files(&["one.md", "two.md", "img.png"]);
        let o = index_for(
            "---\ntags: [x]\n---\n[[Two]] [[ghost]] ![[img.png]] #a/b\n",
            "wiki",
            &fs,
        );
        assert_eq!(o.tags, vec!["x"]);
        assert_eq!(o.categories, vec!["a::b"]);
        assert_eq!(o.links.len(), 1);
        assert_eq!(o.links[0].path, "two.md");
        assert_eq!(o.links[0].space, "wiki");
        assert_eq!(o.links[0].written_as, "Two");
        assert_eq!(o.unresolved_links, vec!["ghost"]);
        assert_eq!(o.embeds[0].path, "img.png");
        assert!(!o.dialect_version.is_empty());
    }
}
