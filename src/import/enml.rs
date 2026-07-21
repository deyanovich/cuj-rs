//! ENML → HTML transform and the import-side sanitizers.
//!
//! The transform is a streaming event rewrite: everything passes
//! through byte-for-byte except the ENML-specific elements
//! (`en-note`, `en-todo`, `en-media`), the stripped XML
//! decl/doctype, and active content — ENML-prohibited elements
//! are dropped whole, `on*` attributes and unsafe-scheme
//! href/src values are stripped (the output is stored HTML and
//! must not execute). Benign content is lossless.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::enex::EnexResource;
use crate::error::{Error, Result};

/// A resolved resource an `<en-media>` element can point at.
#[derive(Debug, Clone)]
pub struct MediaRef {
    pub filename: String,
    pub mime: String,
}

pub struct EnmlContext<'a> {
    pub title: &'a str,
    pub source_url: Option<&'a str>,
    pub freeze_todos: bool,
    /// lowercase MD5 hex → media.
    pub media: &'a HashMap<String, MediaRef>,
}

fn esc_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn esc_attr(s: &str) -> String {
    esc_text(s).replace('"', "&quot;")
}

fn attr_val(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.try_get_attribute(name)
        .ok()
        .flatten()
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

fn raw_str<'a>(bytes: &'a [u8]) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy(bytes)
}

/// ENML-prohibited elements that carry active content — dropped
/// entirely, children included.
fn is_active_element(name: &[u8]) -> bool {
    matches!(
        name.to_ascii_lowercase().as_slice(),
        b"script" | b"iframe" | b"object" | b"embed" | b"applet" | b"form" | b"input" | b"button"
    )
}

/// Scheme allowlist for emitted href/src values: http/https/ftp/
/// mailto and relative URLs pass, as does `data:image/` (inert
/// inline images; en-media never produces them but pasted HTML
/// may). Everything else — javascript:, other data:, vbscript:,
/// … — drops. Whitespace and control chars are stripped before
/// scheme detection (HTML parsers ignore them there).
fn url_is_safe(url: &str) -> bool {
    let cleaned: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    let lower = cleaned.to_ascii_lowercase();
    match lower.split_once(':') {
        None => true,
        // A ':' after '/', '?', or '#' is inside the path/query
        // of a relative URL, not a scheme.
        Some((scheme, _)) if scheme.contains(['/', '?', '#']) => true,
        Some((scheme, rest)) => {
            matches!(scheme, "http" | "https" | "ftp" | "mailto")
                || (scheme == "data" && rest.starts_with("image/"))
        }
    }
}

/// Emit a start/empty tag with `on*` event-handler attributes
/// stripped and href/src values gated by `url_is_safe`. Attribute
/// values keep their original escaping (`"` re-escaped for the
/// double-quoted output form), so benign tags survive losslessly
/// up to quoting/whitespace normalization.
fn push_sanitized_tag(out: &mut String, name: &str, e: &BytesStart<'_>, self_close: bool) {
    out.push('<');
    out.push_str(name);
    for a in e.attributes().with_checks(false).flatten() {
        let key = a.key.as_ref();
        if key.len() >= 2 && key[..2].eq_ignore_ascii_case(b"on") {
            continue;
        }
        if key.eq_ignore_ascii_case(b"href") || key.eq_ignore_ascii_case(b"src") {
            let v = a.unescape_value().unwrap_or_else(|_| raw_str(&a.value));
            if !url_is_safe(v.trim()) {
                continue;
            }
        }
        out.push(' ');
        out.push_str(&raw_str(key));
        out.push_str("=\"");
        out.push_str(&raw_str(&a.value).replace('"', "&quot;"));
        out.push('"');
    }
    out.push_str(if self_close { "/>" } else { ">" });
}

/// Transform an ENML document into a standalone HTML document.
/// Returns the HTML and any non-fatal warnings (e.g. unresolved
/// `<en-media>` hashes).
pub fn enml_to_html(enml: &str, ctx: &EnmlContext<'_>) -> Result<(String, Vec<String>)> {
    let mut reader = Reader::from_str(enml.trim());
    let mut out = String::with_capacity(enml.len() + 256);
    let mut warnings: Vec<String> = Vec::new();
    out.push_str("<html><head><meta charset=\"UTF-8\"/></head>");

    let header = {
        let mut h = String::new();
        if !ctx.title.trim().is_empty() {
            h.push_str(&format!("<h1>{}</h1>", esc_text(ctx.title)));
        }
        if let Some(u) = ctx.source_url {
            if url_is_safe(u.trim()) {
                h.push_str(&format!(
                    "<p>Source: <a href=\"{}\">{}</a></p>",
                    esc_attr(u),
                    esc_text(u)
                ));
            } else {
                h.push_str(&format!("<p>Source: {}</p>", esc_text(u)));
            }
        }
        h
    };

    // A live todo line stays open until the next markup event; the
    // inserted newline is its right boundary (jots todos run to
    // end-of-line).
    let mut todo_open = false;
    // Depth counter while swallowing `<en-media>` inner content
    // (no meaningful children) and dropped active elements.
    let mut skip_depth = 0usize;
    let mut saw_en_note = false;

    macro_rules! close_todo {
        () => {
            if todo_open {
                out.push('\n');
                todo_open = false;
            }
        };
    }
    macro_rules! emit_media {
        ($e:expr) => {{
            close_todo!();
            let hash = attr_val($e, b"hash").unwrap_or_default().to_lowercase();
            match ctx.media.get(&hash) {
                // The visible ` //name ` refs are whitespace-delimited
                // on both sides — the jots payload scanner consumes
                // whole tokens, so an adjacent `<` would invalidate
                // the ref. The src/href copies deliberately don't
                // parse (no left boundary after `"`).
                Some(m) if m.mime.starts_with("image/") => {
                    let f = &m.filename;
                    out.push_str(&format!(
                        "<figure><img src=\"//{f}\" alt=\"{f}\"/>\
                         <figcaption> //{f} </figcaption></figure>"
                    ));
                }
                Some(m) => {
                    let f = &m.filename;
                    out.push_str(&format!("<p><a href=\"//{f}\"> //{f} </a></p>"));
                }
                None => {
                    let safe: String = hash.chars().filter(char::is_ascii_alphanumeric).collect();
                    out.push_str(&format!("<!-- en-media unresolved: {safe} -->"));
                    warnings.push(format!("unresolved en-media hash {safe:?}"));
                }
            }
        }};
    }
    macro_rules! emit_todo {
        ($e:expr) => {{
            let checked = attr_val($e, b"checked").as_deref() == Some("true");
            if ctx.freeze_todos {
                out.push_str(if checked { "☑" } else { "☐" });
            } else {
                // A preceding open todo line ends here (the emitted
                // marker starts with its own newline).
                out.push_str(if checked { "\n??.todo " } else { "\n??todo " });
                todo_open = true;
            }
        }};
    }
    macro_rules! emit_body_open {
        ($e:expr) => {{
            saw_en_note = true;
            push_sanitized_tag(&mut out, "body", $e, false);
            out.push_str(&header);
        }};
    }

    loop {
        let ev = reader.read_event().map_err(|e| {
            Error::Substrate(format!(
                "ENML parse error at byte {}: {e}",
                reader.buffer_position()
            ))
        })?;
        if skip_depth > 0 {
            match ev {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Decl(_) | Event::DocType(_) => {}
            Event::Start(e) => match e.local_name().as_ref() {
                b"en-note" => emit_body_open!(&e),
                b"en-media" => {
                    emit_media!(&e);
                    skip_depth = 1;
                }
                b"en-todo" => emit_todo!(&e),
                name if is_active_element(name) => skip_depth = 1,
                _ => {
                    close_todo!();
                    push_sanitized_tag(&mut out, &raw_str(e.name().as_ref()), &e, false);
                }
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                b"en-note" => {
                    emit_body_open!(&e);
                    out.push_str("</body>");
                }
                b"en-media" => emit_media!(&e),
                b"en-todo" => emit_todo!(&e),
                name if is_active_element(name) => {}
                _ => {
                    close_todo!();
                    push_sanitized_tag(&mut out, &raw_str(e.name().as_ref()), &e, true);
                }
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"en-note" => {
                    close_todo!();
                    out.push_str("</body>");
                }
                b"en-todo" => {}
                _ => {
                    close_todo!();
                    out.push_str("</");
                    out.push_str(&raw_str(e.name().as_ref()));
                    out.push('>');
                }
            },
            Event::Text(e) => out.push_str(&raw_str(e.as_ref())),
            Event::CData(e) => {
                out.push_str("<![CDATA[");
                out.push_str(&raw_str(e.as_ref()));
                out.push_str("]]>");
            }
            Event::GeneralRef(e) => {
                out.push('&');
                out.push_str(&raw_str(e.as_ref()));
                out.push(';');
            }
            Event::Comment(e) => {
                close_todo!();
                out.push_str("<!--");
                out.push_str(&raw_str(e.as_ref()));
                out.push_str("-->");
            }
            Event::PI(e) => {
                close_todo!();
                out.push_str("<?");
                out.push_str(&raw_str(e.as_ref()));
                out.push_str("?>");
            }
            Event::Eof => break,
        }
    }
    if todo_open {
        out.push('\n');
    }
    if !saw_en_note {
        // Empty or bodyless content: still produce a valid document.
        out.push_str("<body>");
        out.push_str(&header);
        out.push_str("</body>");
    }
    out.push_str("</html>\n");
    Ok((out, warnings))
}

// -- sanitizers --------------------------------------------------------

/// Map an Evernote tag name onto the jots tag alphabet
/// (`[a-z0-9_]`): trim, lowercase, whitespace → `_`, drop the rest,
/// strip/collapse underscores. `None` when nothing survives.
pub fn sanitize_tag(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_us = true; // strips leading underscores
    for c in raw.trim().to_lowercase().chars() {
        let mapped = if c.is_whitespace() {
            Some('_')
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            Some(c)
        } else {
            None
        };
        match mapped {
            Some('_') if !prev_us => {
                out.push('_');
                prev_us = true;
            }
            // A run of underscores collapses to one.
            Some('_') => {}
            Some(c) => {
                out.push(c);
                prev_us = false;
            }
            None => {}
        }
    }
    let out = out.trim_end_matches('_').to_string();
    if out.is_empty() { None } else { Some(out) }
}

/// Map an Evernote attachment filename onto the jots FILENAME
/// grammar (dot-separated segments of `[A-Za-z0-9_~,@!()]` first
/// char, `-` also allowed after), which is also trivially NFC as
/// metatheca path facts require. `None` when nothing meaningful
/// survives (caller falls back to a generated name).
pub fn sanitize_filename(raw: &str) -> Option<String> {
    let mut s = String::new();
    let mut prev_us = false;
    for c in raw.trim().chars() {
        let mapped = if c.is_whitespace() {
            '_'
        } else if c.is_ascii_alphanumeric() || "_~,@!()-.".contains(c) {
            c
        } else {
            '_'
        };
        if mapped == '_' {
            if !prev_us {
                s.push('_');
            }
            prev_us = true;
        } else {
            s.push(mapped);
            prev_us = false;
        }
    }
    // Per-segment repair: no empty segments, no leading '-'.
    let segments: Vec<String> = s
        .split('.')
        .map(|seg| seg.trim_start_matches('-').to_string())
        .filter(|seg| !seg.is_empty())
        .collect();
    let out = segments.join(".");
    // A stem with no alphanumeric left (e.g. `報告書.pdf` → `_.pdf`)
    // carries no information — fall back to a generated name.
    let stem = out.rsplit_once('.').map(|(s, _)| s).unwrap_or(&out);
    if out.is_empty() || !stem.chars().any(|c| c.is_ascii_alphanumeric()) {
        None
    } else {
        Some(out)
    }
}

/// File extension for a generated resource filename.
pub fn ext_for_mime(mime: &str) -> &'static str {
    match mime.trim().to_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        "image/tiff" => "tif",
        "image/svg+xml" => "svg",
        "application/pdf" => "pdf",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" => "mp3",
        "audio/amr" => "amr",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/msword" => "doc",
        _ => "bin",
    }
}

/// Final filenames for a note's resources: sanitized `<file-name>`
/// or `resource_{n}.{ext}`, deduplicated with `_2`, `_3`, … before
/// the extension. Deterministic — pass 1 (dry-run display) and
/// pass 2 (actual import) agree.
pub fn assign_filenames(resources: &[EnexResource]) -> Vec<String> {
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut names = Vec::with_capacity(resources.len());
    for (i, r) in resources.iter().enumerate() {
        let base = r
            .file_name
            .as_deref()
            .and_then(sanitize_filename)
            .unwrap_or_else(|| format!("resource_{}.{}", i + 1, ext_for_mime(&r.mime)));
        let mut name = base.clone();
        let mut k = 1usize;
        while used.contains(&name) {
            k += 1;
            name = match base.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}_{k}.{ext}"),
                None => format!("{base}_{k}"),
            };
        }
        used.insert(name.clone());
        names.push(name);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::ProfileConfig;

    fn ctx<'a>(media: &'a HashMap<String, MediaRef>) -> EnmlContext<'a> {
        EnmlContext {
            title: "A & B",
            source_url: Some("https://example.com/x?a=1&b=2"),
            freeze_todos: false,
            media,
        }
    }

    const ENML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd">
<en-note style="word-wrap: break-word;"><div>Plain <b>bold</b> &amp; entity</div>
<en-todo checked="false"/>Call Alice about budget<br/>
<en-todo checked="true"/>Send follow-up email<br/>
<en-media type="image/png" hash="D41D8CD98F00B204E9800998ECF8427E"/>
<en-media type="application/pdf" hash="ffffffffffffffffffffffffffffffff"/>
<table><tr><td>kept &lt;verbatim&gt;</td></tr></table>
</en-note>"#;

    fn media_map() -> HashMap<String, MediaRef> {
        HashMap::from([(
            "d41d8cd98f00b204e9800998ecf8427e".to_string(),
            MediaRef {
                filename: "q1-budget.png".into(),
                mime: "image/png".into(),
            },
        )])
    }

    #[test]
    fn transform_shape() {
        let media = media_map();
        let (html, warnings) = enml_to_html(ENML, &ctx(&media)).unwrap();
        assert!(html.starts_with("<html><head><meta charset=\"UTF-8\"/></head>"));
        assert!(html.contains("<body style=\"word-wrap: break-word;\">"));
        assert!(html.contains("<h1>A &amp; B</h1>"));
        assert!(html.contains(
            "<p>Source: <a href=\"https://example.com/x?a=1&amp;b=2\">\
             https://example.com/x?a=1&amp;b=2</a></p>"
        ));
        assert!(!html.contains("<?xml"));
        assert!(!html.contains("DOCTYPE"));
        assert!(!html.contains("en-note"));
        assert!(html.contains("\n??todo Call Alice about budget\n"));
        assert!(html.contains("\n??.todo Send follow-up email\n"));
        assert!(html.contains(
            "<figure><img src=\"//q1-budget.png\" alt=\"q1-budget.png\"/>\
             <figcaption> //q1-budget.png </figcaption></figure>"
        ));
        assert!(html.contains("<!-- en-media unresolved:"));
        assert_eq!(warnings.len(), 1);
        // Verbatim passthrough keeps escapes and structure untouched.
        assert!(html.contains("Plain <b>bold</b> &amp; entity"));
        assert!(html.contains("<td>kept &lt;verbatim&gt;</td>"));
        assert!(html.ends_with("</body></html>\n"));
    }

    #[test]
    fn todos_parse_as_live_jots_constructs() {
        let media = media_map();
        let (html, _) = enml_to_html(ENML, &ctx(&media)).unwrap();
        let profile = ProfileConfig::new("default");
        let r = crate::binding::parse(&html, &profile);
        assert_eq!(r.todos.len(), 1, "one open todo: {:?}", r.todos);
        assert_eq!(r.todos[0].action, "todo");
        assert_eq!(r.todos[0].description, "Call Alice about budget");
        assert_eq!(r.done_todos.len(), 1, "one done todo: {:?}", r.done_todos);
        assert_eq!(r.done_todos[0].description, "Send follow-up email");
        // The visible attachment ref parses; the src="//…" copy doesn't
        // double it (parser dedup aside, it has no left boundary).
        assert!(
            r.file_references
                .iter()
                .any(|f| f.filepath == "q1-budget.png")
        );
    }

    #[test]
    fn frozen_todos_are_inert() {
        let media = media_map();
        let mut c = ctx(&media);
        c.freeze_todos = true;
        let (html, _) = enml_to_html(ENML, &c).unwrap();
        assert!(html.contains("☐Call Alice about budget"));
        assert!(html.contains("☑Send follow-up email"));
        let profile = ProfileConfig::new("default");
        let r = crate::binding::parse(&html, &profile);
        assert!(r.todos.is_empty() && r.done_todos.is_empty());
    }

    #[test]
    fn empty_content_still_yields_document() {
        let media = HashMap::new();
        let (html, _) = enml_to_html("", &ctx(&media)).unwrap();
        assert!(html.contains("<body><h1>A &amp; B</h1>"));
        assert!(html.ends_with("</body></html>\n"));
    }

    #[test]
    fn active_elements_dropped_with_content() {
        let media = HashMap::new();
        let enml = "<en-note><div>before</div>\
                    <script>alert(1)</script>\
                    <iframe src=\"https://evil.example/\"><p>framed</p></iframe>\
                    <input type=\"text\"/>\
                    <div>after</div></en-note>";
        let (html, _) = enml_to_html(enml, &ctx(&media)).unwrap();
        assert!(!html.contains("script") && !html.contains("alert(1)"));
        assert!(!html.contains("iframe") && !html.contains("framed"));
        assert!(!html.contains("input"));
        assert!(html.contains("<div>before</div><div>after</div>"));
    }

    #[test]
    fn event_handler_attributes_stripped() {
        let media = HashMap::new();
        let enml = "<en-note onload=\"evil()\" style=\"x\">\
                    <div onclick=\"evil()\" class=\"c\">t</div>\
                    <img src=\"https://e.com/i.png\" onerror=\"evil()\"/>\
                    </en-note>";
        let (html, _) = enml_to_html(enml, &ctx(&media)).unwrap();
        assert!(!html.contains("evil("));
        assert!(html.contains("<body style=\"x\">"));
        assert!(html.contains("<div class=\"c\">t</div>"));
        assert!(html.contains("<img src=\"https://e.com/i.png\"/>"));
    }

    #[test]
    fn unsafe_url_attributes_dropped() {
        let media = HashMap::new();
        let enml = "<en-note>\
                    <a href=\"javascript:alert(1)\">j</a>\
                    <a href=\"JAVA&#83;CRIPT:alert(1)\">e</a>\
                    <a href=\"java\tscript:alert(1)\">w</a>\
                    <a href=\"data:text/html,x\">d</a>\
                    <img src=\"data:image/png;base64,AA==\"/>\
                    <a href=\"/rel/path\">r</a>\
                    <a href=\"mailto:x@y.z\">m</a>\
                    </en-note>";
        let (html, _) = enml_to_html(enml, &ctx(&media)).unwrap();
        assert!(html.contains("<a>j</a>"));
        assert!(html.contains("<a>e</a>"));
        assert!(html.contains("<a>w</a>"));
        assert!(html.contains("<a>d</a>"));
        assert!(html.contains("<img src=\"data:image/png;base64,AA==\"/>"));
        assert!(html.contains("<a href=\"/rel/path\">r</a>"));
        assert!(html.contains("<a href=\"mailto:x@y.z\">m</a>"));
    }

    #[test]
    fn unsafe_source_url_gets_no_anchor() {
        let media = HashMap::new();
        let mut c = ctx(&media);
        c.source_url = Some("javascript:alert(1)");
        let (html, _) = enml_to_html("<en-note><div>x</div></en-note>", &c).unwrap();
        assert!(!html.contains("href"));
        assert!(html.contains("<p>Source: javascript:alert(1)</p>"));
    }

    #[test]
    fn benign_content_byte_identical() {
        let media = HashMap::new();
        let body = "<div class=\"x\">a <b>b</b> &amp; \
                    <a href=\"https://e.com/?a=1&amp;b=2\">l</a> \
                    <span data-k=\"v\">s</span></div>";
        let enml = format!("<en-note>{body}</en-note>");
        let (html, _) = enml_to_html(&enml, &ctx(&media)).unwrap();
        assert!(html.contains(body), "{html}");
    }

    #[test]
    fn tag_sanitization_table() {
        assert_eq!(
            sanitize_tag("Meeting Notes").as_deref(),
            Some("meeting_notes")
        );
        assert_eq!(sanitize_tag("Q1 2024").as_deref(), Some("q1_2024"));
        assert_eq!(
            sanitize_tag("project/alpha").as_deref(),
            Some("projectalpha")
        );
        assert_eq!(sanitize_tag("  urgent!! ").as_deref(), Some("urgent"));
        assert_eq!(sanitize_tag("日本語"), None);
        assert_eq!(sanitize_tag("__foo__bar__").as_deref(), Some("foo_bar"));
        assert_eq!(sanitize_tag(""), None);
        assert_eq!(sanitize_tag("   "), None);
    }

    #[test]
    fn filename_sanitization() {
        assert_eq!(
            sanitize_filename("my photo (1).png").as_deref(),
            Some("my_photo_(1).png")
        );
        assert_eq!(sanitize_filename("報告書.pdf"), None);
        assert_eq!(sanitize_filename("...").as_deref(), None);
        assert_eq!(sanitize_filename("a..b.txt").as_deref(), Some("a.b.txt"));
        assert_eq!(sanitize_filename("-lead.pdf").as_deref(), Some("lead.pdf"));
        assert_eq!(
            sanitize_filename("Ré su mé.pdf").as_deref(),
            Some("R_su_m_.pdf")
        );
    }

    #[test]
    fn filename_assignment_dedup() {
        use super::super::enex::EnexResource;
        let mk = |file_name: Option<&str>, mime: &str| EnexResource {
            file_name: file_name.map(String::from),
            mime: mime.to_string(),
            ..Default::default()
        };
        let names = assign_filenames(&[
            mk(None, "image/png"),
            mk(None, "image/png"),
            mk(Some("shot.png"), "image/png"),
            mk(Some("shot.png"), "image/png"),
            mk(Some("nodot"), "application/octet-stream"),
            mk(Some("nodot"), "application/octet-stream"),
        ]);
        assert_eq!(
            names,
            vec![
                "resource_1.png",
                "resource_2.png",
                "shot.png",
                "shot_2.png",
                "nodot",
                "nodot_2"
            ]
        );
    }
}
