//! ENEX (Evernote XML export) parsing. One streaming pass per file;
//! `decode: false` is the cheap metadata scan (no base64 decode —
//! resource data is only hashed into the fingerprint — no OCR
//! parse, content dropped after fingerprinting), `decode: true`
//! materializes everything a note import needs.
//!
//! Security: the external DTD referenced by `<!DOCTYPE en-export
//! SYSTEM "http://xml.evernote.com/...">` is never fetched —
//! quick-xml surfaces the declaration as a skippable event and does
//! no I/O of its own (no XXE, no network).

use std::io::BufReader;
use std::path::Path;

use base64::Engine as _;
use md5::{Digest, Md5};
use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};
use sha2::Sha256;

use crate::error::{Error, Result};

#[derive(Debug, Default)]
pub struct EnexNote {
    pub title: String,
    /// Raw `<created>` text (fingerprint input), e.g. `20240315T090000Z`.
    pub created_raw: String,
    pub created_ns: Option<i64>,
    pub updated_ns: Option<i64>,
    pub source_url: Option<String>,
    /// Raw Evernote tag names (unsanitized).
    pub tags: Vec<String>,
    /// Raw ENML document from the `<content>` CDATA (empty after a
    /// scan pass — hashed into the fingerprint, then dropped).
    pub content: String,
    /// Dedup fingerprint: SHA-256 hex of `title \0 created_raw \0
    /// content`, extended with each tag (sorted) and each
    /// resource's data hash (document order), `\0`-prefixed —
    /// notes differing only in tags or attachments don't collide.
    pub fingerprint: String,
    /// The pre-extension form (`title \0 created_raw \0 content`
    /// only), matched against facts stored by older imports.
    pub legacy_fingerprint: String,
    pub resources: Vec<EnexResource>,
}

#[derive(Debug, Default)]
pub struct EnexResource {
    /// Raw `<resource-attributes><file-name>` (unsanitized).
    pub file_name: Option<String>,
    pub mime: String,
    /// Decoded bytes (empty in scan mode).
    pub bytes: Vec<u8>,
    /// MD5 hex of the decoded bytes — the `<en-media hash=…>` key
    /// (empty in scan mode).
    pub md5_hex: String,
    /// MD5 hex of the whitespace-stripped base64 text — the
    /// fingerprint contribution, computed without decoding so the
    /// scan and decode passes agree.
    pub data_fp: String,
    /// Text extracted from `<recognition>` (None in scan mode or
    /// when the resource carries no recognition data).
    pub ocr_text: Option<String>,
}

/// Parse a `YYYYMMDDTHHmmssZ` ENEX timestamp to UTC epoch ns.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y%m%dT%H%M%SZ")
        .ok()?
        .and_utc()
        .timestamp_nanos_opt()
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Resolve a general/character reference to its text; unknown named
/// entities keep their literal `&name;` form.
pub(crate) fn resolve_ref(r: &BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    match r.decode().ok().as_deref() {
        Some("amp") => "&".into(),
        Some("lt") => "<".into(),
        Some("gt") => ">".into(),
        Some("quot") => "\"".into(),
        Some("apos") => "'".into(),
        Some(other) => format!("&{other};"),
        None => String::new(),
    }
}

fn xml_err(path: &Path, pos: u64, e: impl std::fmt::Display) -> Error {
    Error::Substrate(format!(
        "{}: XML parse error at byte {pos}: {e}",
        path.display()
    ))
}

/// Leaf elements whose text we accumulate.
fn captures(name: &[u8], decode: bool) -> bool {
    match name {
        b"title" | b"content" | b"created" | b"updated" | b"tag" | b"source-url" | b"mime"
        | b"file-name" | b"data" => true,
        b"recognition" => decode,
        _ => false,
    }
}

pub fn parse_enex_file(path: &Path, decode: bool) -> Result<Vec<EnexNote>> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::Substrate(format!("{}: {e}", path.display())))?;
    let mut reader = Reader::from_reader(BufReader::new(file));

    let mut notes: Vec<EnexNote> = Vec::new();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut note: Option<EnexNote> = None;
    let mut resource: Option<EnexResource> = None;
    let mut text = String::new();
    let mut capture = false;
    let mut buf = Vec::new();

    loop {
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| xml_err(path, reader.buffer_position(), e))?;
        match ev {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_vec();
                match name.as_slice() {
                    b"note" if stack.len() == 1 => note = Some(EnexNote::default()),
                    b"resource" if note.is_some() => resource = Some(EnexResource::default()),
                    _ => {}
                }
                capture = note.is_some() && captures(&name, decode);
                if capture {
                    text.clear();
                }
                stack.push(name);
            }
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                let parent: &[u8] = stack.last().map(Vec::as_slice).unwrap_or(b"");
                capture = false;
                let taken = std::mem::take(&mut text);
                match (name.as_slice(), parent) {
                    (b"title", b"note") => {
                        if let Some(n) = &mut note {
                            n.title = taken;
                        }
                    }
                    (b"content", b"note") => {
                        if let Some(n) = &mut note {
                            n.content = taken;
                        }
                    }
                    (b"created", b"note") => {
                        if let Some(n) = &mut note {
                            n.created_raw = taken;
                        }
                    }
                    (b"updated", b"note") => {
                        if let Some(n) = &mut note {
                            n.updated_ns = parse_timestamp(&taken);
                        }
                    }
                    (b"tag", b"note") => {
                        if let Some(n) = &mut note {
                            n.tags.push(taken);
                        }
                    }
                    (b"source-url", b"note-attributes") => {
                        if let Some(n) = &mut note {
                            let url = taken.trim().to_string();
                            if !url.is_empty() {
                                n.source_url = Some(url);
                            }
                        }
                    }
                    (b"mime", b"resource") => {
                        if let Some(r) = &mut resource {
                            r.mime = taken.trim().to_string();
                        }
                    }
                    (b"file-name", b"resource-attributes") => {
                        if let Some(r) = &mut resource {
                            let f = taken.trim().to_string();
                            if !f.is_empty() {
                                r.file_name = Some(f);
                            }
                        }
                    }
                    (b"data", b"resource") => {
                        if let Some(r) = &mut resource {
                            let cleaned: Vec<u8> =
                                taken.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
                            r.data_fp = hex(&Md5::digest(&cleaned));
                            if decode {
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(&cleaned)
                                    .map_err(|e| {
                                        xml_err(
                                            path,
                                            reader.buffer_position(),
                                            format!("bad base64 resource data: {e}"),
                                        )
                                    })?;
                                r.md5_hex = hex(&Md5::digest(&bytes));
                                r.bytes = bytes;
                            }
                        }
                    }
                    (b"recognition", b"resource") if decode => {
                        if let Some(r) = &mut resource {
                            r.ocr_text = extract_ocr_text(&taken);
                        }
                    }
                    (b"resource", _) => {
                        if let (Some(n), Some(r)) = (&mut note, resource.take()) {
                            n.resources.push(r);
                        }
                    }
                    (b"note", _) => {
                        if let Some(mut n) = note.take() {
                            let mut h = Sha256::new();
                            h.update(n.title.as_bytes());
                            h.update([0u8]);
                            h.update(n.created_raw.as_bytes());
                            h.update([0u8]);
                            h.update(n.content.as_bytes());
                            n.legacy_fingerprint = hex(&h.clone().finalize());
                            let mut tags = n.tags.clone();
                            tags.sort();
                            for t in &tags {
                                h.update([0u8]);
                                h.update(t.as_bytes());
                            }
                            for r in &n.resources {
                                h.update([0u8]);
                                h.update(r.data_fp.as_bytes());
                            }
                            n.fingerprint = hex(&h.finalize());
                            n.created_ns = parse_timestamp(&n.created_raw);
                            if !decode {
                                n.content = String::new();
                            }
                            notes.push(n);
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(_) => {}
            Event::Text(e) if capture => {
                text.push_str(
                    &e.decode()
                        .map_err(|er| xml_err(path, reader.buffer_position(), er))?,
                );
            }
            Event::CData(e) if capture => {
                text.push_str(
                    &e.decode()
                        .map_err(|er| xml_err(path, reader.buffer_position(), er))?,
                );
            }
            Event::GeneralRef(e) if capture => text.push_str(&resolve_ref(&e)),
            Event::Eof => break,
            // Decl, DocType (never fetched), PI, Comment, and text
            // outside captured leaves.
            _ => {}
        }
        buf.clear();
    }
    Ok(notes)
}

/// Extract the OCR transcript from Evernote `<recognition>` XML:
/// every `<t>` text value, one per line, in document order.
pub fn extract_ocr_text(reco_xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(reco_xml);
    let mut out = String::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" => {
                depth += 1;
                cur.clear();
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"t" && depth > 0 => {
                depth -= 1;
                if !cur.trim().is_empty() {
                    out.push_str(cur.trim());
                    out.push('\n');
                }
            }
            Ok(Event::Text(e)) if depth > 0 => {
                cur.push_str(&e.decode().ok()?);
            }
            Ok(Event::CData(e)) if depth > 0 => {
                cur.push_str(&e.decode().ok()?);
            }
            Ok(Event::GeneralRef(e)) if depth > 0 => cur.push_str(&resolve_ref(&e)),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            // Salvage what parsed before a malformed tail.
            Err(_) => break,
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(content: &str) -> tempfile::NamedTempFile {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    const ENEX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-export SYSTEM "http://xml.evernote.com/pub/evernote-export4.dtd">
<en-export export-date="20240315T120000Z" application="Evernote" version="10.93.4">
<note>
<title>Meeting Notes &amp; Plans</title>
<content><![CDATA[<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd">
<en-note><p>Hello</p></en-note>]]></content>
<created>20240315T090000Z</created>
<updated>20240315T143000Z</updated>
<tag>work</tag>
<tag>Q1 2024</tag>
<note-attributes><source-url>https://example.com/a</source-url></note-attributes>
<resource>
<data encoding="base64">
aGVsbG8g
d29ybGQ=
</data>
<mime>text/plain</mime>
<recognition><![CDATA[<?xml version="1.0" encoding="UTF-8"?>
<recoIndex docType="picture" objType="image">
<item x="10" y="20" w="100" h="30"><t w="96">Budget</t><t w="42">Budge7</t></item>
<item x="10" y="60" w="80" h="30"><t w="88">Q1 2024</t></item>
</recoIndex>]]></recognition>
<resource-attributes><file-name>hello.txt</file-name></resource-attributes>
</resource>
</note>
</en-export>
"#;

    #[test]
    fn parses_notes_with_doctype_untouched() {
        let f = write_tmp(ENEX);
        let notes = parse_enex_file(f.path(), true).unwrap();
        assert_eq!(notes.len(), 1);
        let n = &notes[0];
        assert_eq!(n.title, "Meeting Notes & Plans");
        assert_eq!(n.created_raw, "20240315T090000Z");
        assert!(n.created_ns.is_some());
        assert!(n.updated_ns.is_some());
        assert_eq!(n.source_url.as_deref(), Some("https://example.com/a"));
        assert_eq!(n.tags, vec!["work", "Q1 2024"]);
        assert!(n.content.contains("<en-note><p>Hello</p></en-note>"));
        assert_eq!(n.resources.len(), 1);
        let r = &n.resources[0];
        assert_eq!(r.bytes, b"hello world");
        assert_eq!(r.md5_hex, hex(&Md5::digest(b"hello world")));
        assert_eq!(r.mime, "text/plain");
        assert_eq!(r.file_name.as_deref(), Some("hello.txt"));
        assert_eq!(r.ocr_text.as_deref(), Some("Budget\nBudge7\nQ1 2024\n"));
    }

    #[test]
    fn scan_mode_skips_decode_but_fingerprints_identically() {
        let f = write_tmp(ENEX);
        let scanned = parse_enex_file(f.path(), false).unwrap();
        let decoded = parse_enex_file(f.path(), true).unwrap();
        assert_eq!(scanned[0].fingerprint, decoded[0].fingerprint);
        assert!(scanned[0].content.is_empty());
        assert!(scanned[0].resources[0].bytes.is_empty());
        assert!(scanned[0].resources[0].ocr_text.is_none());
        assert_eq!(
            scanned[0].resources[0].file_name.as_deref(),
            Some("hello.txt")
        );
    }

    #[test]
    fn fingerprint_discriminates_tags_and_resources() {
        let note = |tags: &str, res: &str| {
            format!(
                "<note><title>T</title>\
                 <content><![CDATA[<en-note><p>x</p></en-note>]]></content>\
                 <created>20240315T090000Z</created>{tags}{res}</note>"
            )
        };
        let res = |b64: &str| {
            format!(
                "<resource><data encoding=\"base64\">{b64}</data>\
                 <mime>text/plain</mime></resource>"
            )
        };
        let enex = format!(
            "<en-export>{}{}{}{}{}</en-export>",
            note("", ""),
            note("<tag>a</tag>", ""),
            note("<tag>b</tag>", ""),
            note("", &res("YQ==")),
            note("", &res("Yg==")),
        );
        let f = write_tmp(&enex);
        let notes = parse_enex_file(f.path(), false).unwrap();
        // Same title/created/content: one shared legacy fingerprint,
        // five distinct extended ones... except the bare note, whose
        // extended form equals its legacy form.
        let legacy: Vec<_> = notes.iter().map(|n| &n.legacy_fingerprint).collect();
        assert!(legacy.iter().all(|l| *l == legacy[0]));
        let mut fps: Vec<_> = notes.iter().map(|n| n.fingerprint.clone()).collect();
        fps.sort();
        fps.dedup();
        assert_eq!(fps.len(), 5);
        assert_eq!(notes[0].fingerprint, notes[0].legacy_fingerprint);
    }

    #[test]
    fn timestamp_parsing() {
        assert_eq!(
            parse_timestamp("20240315T090000Z"),
            Some(1_710_493_200_000_000_000)
        );
        assert!(parse_timestamp("garbage").is_none());
        assert!(parse_timestamp("").is_none());
    }

    #[test]
    fn malformed_file_errors() {
        let f = write_tmp("<en-export><note><title>x</не-title></note></en-export>");
        assert!(parse_enex_file(f.path(), false).is_err());
    }
}
