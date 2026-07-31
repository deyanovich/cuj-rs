//! End-to-end `cuj import` tests: ENEX → metatheca vault via the
//! library API, against tempdir vaults.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cuj::app::{App, Opts, parse_ref};
use cuj::client::ClientConfig;
use cuj::error::Error;
use cuj::facts;
use cuj::import::{ImportArgs, run};

fn client_for(tmp: &tempfile::TempDir) -> ClientConfig {
    ClientConfig {
        vault: tmp.path().join("vault"),
        active_profile: "default".into(),
        editor: None,
        extract: BTreeMap::new(),
        spaces: BTreeMap::new(),
        path: tmp.path().join("config.toml"),
    }
}

fn opts() -> Opts {
    Opts::default()
}

const CREATED_2024_NS: i64 = 1_710_493_200_000_000_000; // 20240315T090000Z

fn note_xml(title: &str, created: &str, body: &str, tags: &[&str], extra: &str) -> String {
    let tags: String = tags.iter().map(|t| format!("<tag>{t}</tag>")).collect();
    format!(
        r#"<note>
<title>{title}</title>
<content><![CDATA[<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd">
<en-note>{body}</en-note>]]></content>
<created>{created}</created>
<updated>{created}</updated>
{tags}{extra}
</note>"#
    )
}

fn enex(notes: &[String]) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-export SYSTEM "http://xml.evernote.com/pub/evernote-export4.dtd">
<en-export export-date="20240401T000000Z" application="Evernote" version="10.93.4">
{}
</en-export>"#,
        notes.join("\n")
    )
}

fn write_enex(tmp: &tempfile::TempDir, name: &str, content: &str) -> PathBuf {
    let p = tmp.path().join(name);
    std::fs::write(&p, content).unwrap();
    p
}

fn args(files: Vec<PathBuf>) -> ImportArgs {
    ImportArgs {
        files,
        ..Default::default()
    }
}

fn setup() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    (tmp, App::open(client).unwrap())
}

#[test]
fn typical_note() {
    let (tmp, app) = setup();
    let f = write_enex(
        &tmp,
        "batch.enex",
        &enex(&[note_xml(
            "Meeting Notes",
            "20240315T090000Z",
            "<div>Discussed the roadmap.</div>",
            &["work", "Q1 2024", "日本語"],
            "<note-attributes><source-url>https://example.com/a</source-url></note-attributes>",
        )]),
    );
    let report = run(&app, &opts(), &args(vec![f])).unwrap();
    assert_eq!(report.imported.len(), 1, "errors: {:?}", report.errors);
    assert!(report.errors.is_empty() && report.skipped.is_empty());
    assert_eq!(report.imported[0].id, Some(1));
    assert_eq!(report.imported[0].created_at_ns, CREATED_2024_NS);

    let reads = app.reads(&opts()).unwrap();
    let (e, ident) = reads.resolve_live("default", 1).unwrap();
    assert_eq!(ident.ext, "html");
    assert_eq!(reads.births[&e].created_at_ns, CREATED_2024_NS);

    let content = String::from_utf8(reads.bytes(e).unwrap()).unwrap();
    assert!(content.starts_with("<html><head><meta charset=\"UTF-8\"/></head>"));
    assert!(content.contains("<h1>Meeting Notes</h1>"));
    assert!(
        content
            .contains("<p>Source: <a href=\"https://example.com/a\">https://example.com/a</a></p>")
    );
    assert!(content.contains("<div>Discussed the roadmap.</div>"));
    assert!(content.ends_with("</html>\n"));
    // Tags live in the overlay, not the content.
    assert!(!content.contains("..work"));
    // The summary reads through the HTML: the heading, not the
    // <html><head> preamble.
    assert_eq!(cuj::queries::summary_line(&reads, e), "Meeting Notes");
    let eff = reads.effective(e);
    assert!(eff.tags.contains("work"));
    assert!(eff.tags.contains("q1_2024"));
    assert_eq!(
        eff.tags.len(),
        2,
        "unsanitizable tag discarded: {:?}",
        eff.tags
    );

    // Import provenance fact carries the fingerprint and source.
    let imports = app.vault.current_facts(facts::K_CUJ_IMPORT).unwrap();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].0, e);
    let body: facts::ImportBody = facts::from_body(&imports[0].1.body).unwrap();
    assert_eq!(body.source, "batch.enex");
    assert_eq!(body.fingerprint.len(), 64);
    assert_eq!(body.updated_at_ns, Some(CREATED_2024_NS));
}

#[test]
fn resources_recognition_and_unresolved_media() {
    let (tmp, app) = setup();
    // "hello world" base64, split across lines like Evernote does.
    let resource = r#"<resource>
<data encoding="base64">
aGVsbG8g
d29ybGQ=
</data>
<mime>image/png</mime>
<recognition><![CDATA[<?xml version="1.0" encoding="UTF-8"?>
<recoIndex docType="picture" objType="image">
<item x="1" y="2" w="3" h="4"><t w="96">Budget</t></item>
<item x="1" y="9" w="3" h="4"><t w="88">Q1 2024</t></item>
</recoIndex>]]></recognition>
<resource-attributes><file-name>q1 budget.png</file-name></resource-attributes>
</resource>"#;
    // md5("hello world") = 5eb63bbbe01eeed093cb22bb8f5acdc3
    let body = "<div><en-media type=\"image/png\" \
                hash=\"5EB63BBBE01EEED093CB22BB8F5ACDC3\"/></div>\
                <div><en-media type=\"image/png\" \
                hash=\"00000000000000000000000000000000\"/></div>";
    let f = write_enex(
        &tmp,
        "res.enex",
        &enex(&[note_xml(
            "With media",
            "20240315T090000Z",
            body,
            &[],
            resource,
        )]),
    );
    let report = run(&app, &opts(), &args(vec![f])).unwrap();
    assert_eq!(report.imported.len(), 1, "errors: {:?}", report.errors);
    assert_eq!(report.imported[0].attachments, vec!["q1_budget.png"]);

    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", 1).unwrap();
    let content = String::from_utf8(reads.bytes(e).unwrap()).unwrap();
    assert!(content.contains(
        "<figure><img src=\"//q1_budget.png\" alt=\"q1_budget.png\"/>\
         <figcaption> //q1_budget.png </figcaption></figure>"
    ));
    assert!(content.contains("<!-- en-media unresolved: 00000000000000000000000000000000 -->"));

    // Attachment entry: body, bytes, path.
    let atts = reads.attachments_of(e);
    assert_eq!(atts.len(), 1);
    let (att_entry, att) = (atts[0].0, atts[0].1);
    assert_eq!(att.filename, "q1_budget.png");
    assert_eq!(att.size, 11);
    assert!(!att.detached);
    assert_eq!(reads.bytes(att_entry).unwrap(), b"hello world");
    let paths = app.vault.ls(Some("default/att/1")).unwrap();
    assert!(
        paths
            .iter()
            .any(|(p, _)| p == "default/att/1/q1_budget.png"),
        "paths: {paths:?}"
    );

    // The parsed index carries the visible file ref.
    assert!(
        reads.parsed[&e]
            .file_references
            .iter()
            .any(|r| r.path == "q1_budget.png")
    );

    // OCR extraction entry with the transcript blob.
    let xentry = reads.extraction_of(att_entry).expect("extraction entry");
    assert_eq!(
        String::from_utf8(reads.bytes(xentry).unwrap()).unwrap(),
        "Budget\nQ1 2024\n"
    );
    let (_, xbody, _) = reads
        .extractions
        .iter()
        .find(|(x, _, _)| *x == xentry)
        .unwrap();
    assert_eq!(xbody.tool, "evernote-recognition");
}

#[test]
fn dedup_within_and_across_runs() {
    let (tmp, app) = setup();
    let note = note_xml("Dup", "20240315T090000Z", "<div>x</div>", &[], "");
    let f1 = write_enex(&tmp, "a.enex", &enex(std::slice::from_ref(&note)));
    let f2 = write_enex(&tmp, "b.enex", &enex(std::slice::from_ref(&note)));

    // Same note in two files within one invocation: imported once.
    let report = run(&app, &opts(), &args(vec![f1.clone(), f2.clone()])).unwrap();
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.skipped.len(), 1);

    // Re-import: everything skips, counter untouched.
    let report = run(&app, &opts(), &args(vec![f1.clone()])).unwrap();
    assert_eq!(report.imported.len(), 0);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].existing.as_deref(), Some("--default--1"));
    let reads = app.reads(&opts()).unwrap();
    assert_eq!(reads.next_id("default").unwrap(), 2);

    // Dedup is permanent: still skips after rm.
    app.rm(&opts(), &parse_ref("--1").unwrap()).unwrap();
    let report = run(&app, &opts(), &args(vec![f1])).unwrap();
    assert_eq!(report.imported.len(), 0);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].existing, None);
}

#[test]
fn dedup_discriminates_resources_and_tags() {
    let (tmp, app) = setup();
    let res = |b64: &str| {
        format!(
            "<resource><data encoding=\"base64\">{b64}</data>\
             <mime>text/plain</mime></resource>"
        )
    };
    let notes = vec![
        note_xml("Same", "20240315T090000Z", "<div>x</div>", &[], &res("YQ==")),
        note_xml("Same", "20240315T090000Z", "<div>x</div>", &[], &res("Yg==")),
        note_xml("Same", "20240315T090000Z", "<div>x</div>", &["one"], ""),
        note_xml("Same", "20240315T090000Z", "<div>x</div>", &["two"], ""),
        // True duplicate of the previous note: all fields equal.
        note_xml("Same", "20240315T090000Z", "<div>x</div>", &["two"], ""),
    ];
    let f = write_enex(&tmp, "fp.enex", &enex(&notes));
    let report = run(&app, &opts(), &args(vec![f])).unwrap();
    assert_eq!(report.imported.len(), 4, "errors: {:?}", report.errors);
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn legacy_fingerprint_fact_still_dedups() {
    let (tmp, app) = setup();
    let f = write_enex(
        &tmp,
        "legacy.enex",
        &enex(&[note_xml(
            "Old",
            "20240315T090000Z",
            "<div>x</div>",
            &["t"],
            "",
        )]),
    );
    // Seed an import fact carrying the pre-extension fingerprint,
    // as an older cuj would have stored it. The note has a tag, so
    // its extended fingerprint differs — the match must come from
    // the legacy form.
    let legacy = cuj::import::enex::parse_enex_file(&f, false).unwrap()[0]
        .legacy_fingerprint
        .clone();
    let entry = app.vault.new_entry_id();
    let fact = facts::to_fact(
        facts::K_CUJ_IMPORT,
        entry,
        &facts::ImportBody {
            fingerprint: legacy,
            source: "legacy.enex".into(),
            imported_at_ns: facts::now_ns(),
            updated_at_ns: None,
        },
    )
    .unwrap();
    let head = app.vault.current_state_hash().unwrap();
    app.vault.commit_onto(&[fact], &head).unwrap();

    let report = run(&app, &opts(), &args(vec![f])).unwrap();
    assert_eq!(report.imported.len(), 0, "errors: {:?}", report.errors);
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn chronological_ids_across_files() {
    let (tmp, app) = setup();
    let f1 = write_enex(
        &tmp,
        "one.enex",
        &enex(&[
            note_xml("c", "20240303T000000Z", "<div>c</div>", &[], ""),
            note_xml("a", "20240101T000000Z", "<div>a</div>", &[], ""),
        ]),
    );
    let f2 = write_enex(
        &tmp,
        "two.enex",
        &enex(&[note_xml("b", "20240202T000000Z", "<div>b</div>", &[], "")]),
    );
    let report = run(&app, &opts(), &args(vec![f1, f2])).unwrap();
    let order: Vec<(Option<i64>, String)> = report
        .imported
        .iter()
        .map(|n| (n.id, n.title.clone()))
        .collect();
    assert_eq!(
        order,
        vec![
            (Some(1), "a".to_string()),
            (Some(2), "b".to_string()),
            (Some(3), "c".to_string())
        ]
    );
}

#[test]
fn todos_live_and_frozen() {
    let (tmp, app) = setup();
    let body = "<div><en-todo checked=\"false\"/>Call Alice about budget</div>\
                <div><en-todo checked=\"true\"/>Send follow-up</div>";
    let f = write_enex(
        &tmp,
        "todos.enex",
        &enex(&[note_xml("Todos", "20240315T090000Z", body, &[], "")]),
    );
    run(&app, &opts(), &args(vec![f])).unwrap();

    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", 1).unwrap();
    let eff = reads.effective(e);
    assert_eq!(eff.todos.len(), 2, "todos: {:?}", eff.todos);
    let open: Vec<_> = eff.todos.iter().filter(|t| !t.done).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].action.as_deref(), Some("todo"));
    assert_eq!(open[0].description, "Call Alice about budget");
    assert!(
        eff.todos
            .iter()
            .any(|t| t.done && t.description == "Send follow-up")
    );

    // Imported todos are live: `cuj todo done` rewrites the content.
    let msg = app
        .todo_done(&opts(), &parse_ref("--1").unwrap(), "1")
        .unwrap();
    assert!(msg.contains("content"), "{msg}");
    let reads = app.reads(&opts()).unwrap();
    assert!(reads.effective(e).todos.iter().all(|t| t.done));

    // --freeze-todos: unicode checkboxes, nothing indexed.
    let body2 = "<div><en-todo checked=\"false\"/>inert</div>";
    let f2 = write_enex(
        &tmp,
        "frozen.enex",
        &enex(&[note_xml("Frozen", "20240316T090000Z", body2, &[], "")]),
    );
    let mut a = args(vec![f2]);
    a.freeze_todos = true;
    run(&app, &opts(), &a).unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e2, _) = reads.resolve_live("default", 2).unwrap();
    let content = String::from_utf8(reads.bytes(e2).unwrap()).unwrap();
    assert!(content.contains("☐inert"));
    assert!(reads.effective(e2).todos.is_empty());
}

#[test]
fn categories_and_validation() {
    let (tmp, app) = setup();
    let f = write_enex(
        &tmp,
        "cat.enex",
        &enex(&[note_xml("Cat", "20240315T090000Z", "<div>x</div>", &[], "")]),
    );
    let mut a = args(vec![f.clone()]);
    a.categories = vec![
        cuj::import::parse_category("::Evernote").unwrap(),
        cuj::import::parse_category("::work::archive").unwrap(),
    ];
    run(&app, &opts(), &a).unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", 1).unwrap();
    let eff = reads.effective(e);
    assert!(eff.categories.contains("evernote"));
    assert!(eff.categories.contains("work::archive"));

    // Invalid categories fail preflight, before any import.
    assert!(matches!(
        cuj::import::parse_category("evernote"),
        Err(Error::Usage(_))
    ));
    let mut bad = args(vec![f]);
    bad.categories = vec!["ever note".into()];
    assert!(matches!(run(&app, &opts(), &bad), Err(Error::Usage(_))));
}

#[test]
fn dry_run_commits_nothing() {
    let (tmp, app) = setup();
    let f = write_enex(
        &tmp,
        "dry.enex",
        &enex(&[note_xml(
            "Dry",
            "20240315T090000Z",
            "<div>x</div>",
            &["t"],
            "",
        )]),
    );
    let mut a = args(vec![f.clone()]);
    a.dry_run = true;
    let report = run(&app, &opts(), &a).unwrap();
    assert!(report.dry_run);
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.imported[0].id, None);
    assert_eq!(report.imported[0].tags, vec!["t"]);

    let reads = app.reads(&opts()).unwrap();
    assert_eq!(reads.next_id("default").unwrap(), 1);
    assert!(
        app.vault
            .current_facts(facts::K_CUJ_IMPORT)
            .unwrap()
            .is_empty()
    );

    // Dry-run against existing imports reports duplicates.
    run(&app, &opts(), &args(vec![f.clone()])).unwrap();
    let mut a = args(vec![f]);
    a.dry_run = true;
    let report = run(&app, &opts(), &a).unwrap();
    assert_eq!(report.imported.len(), 0);
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn per_note_and_per_file_error_isolation() {
    let (tmp, app) = setup();
    // Middle note has malformed ENML inside its CDATA.
    let broken = r#"<note>
<title>Broken</title>
<content><![CDATA[<en-note><b>unclosed</en-note>]]></content>
<created>20240202T000000Z</created>
</note>"#
        .to_string();
    let good1 = note_xml("Good1", "20240101T000000Z", "<div>1</div>", &[], "");
    let good2 = note_xml("Good2", "20240303T000000Z", "<div>2</div>", &[], "");
    let f = write_enex(&tmp, "mixed.enex", &enex(&[good1, broken, good2]));
    // A second, entirely malformed file.
    let f_bad = write_enex(&tmp, "bad.enex", "<en-export><note></export>");

    let report = run(&app, &opts(), &args(vec![f, f_bad])).unwrap();
    assert_eq!(report.imported.len(), 2, "errors: {:?}", report.errors);
    assert_eq!(report.errors.len(), 2);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.title.as_deref() == Some("Broken"))
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.title.is_none() && e.file == "bad.enex")
    );
    // IDs stay dense: the failed note claims no ID.
    let ids: Vec<Option<i64>> = report.imported.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![Some(1), Some(2)]);
}

#[test]
fn untitled_note_and_ext_override_rejection() {
    let (tmp, app) = setup();
    let f = write_enex(
        &tmp,
        "untitled.enex",
        &enex(&[note_xml("", "20240315T090000Z", "<div>x</div>", &[], "")]),
    );

    let mut o = opts();
    o.ext_override = Some("md".into());
    assert!(matches!(
        run(&app, &o, &args(vec![f.clone()])),
        Err(Error::Usage(_))
    ));

    run(&app, &opts(), &args(vec![f])).unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", 1).unwrap();
    let content = String::from_utf8(reads.bytes(e).unwrap()).unwrap();
    assert!(!content.contains("<h1>"));
}

#[test]
fn missing_file_fails_preflight() {
    let (tmp, app) = setup();
    let good = write_enex(
        &tmp,
        "ok.enex",
        &enex(&[note_xml("x", "20240315T090000Z", "<div>x</div>", &[], "")]),
    );
    let err = run(
        &app,
        &opts(),
        &args(vec![good, tmp.path().join("nope.enex")]),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Usage(_)), "{err}");
    // Nothing imported.
    let reads = app.reads(&opts()).unwrap();
    assert_eq!(reads.next_id("default").unwrap(), 1);
}

#[test]
fn obsidian_vault_import() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    let vault = tmp.path().join("obsvault");
    std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
    std::fs::write(vault.join(".obsidian/app.json"), "{}").unwrap();
    std::fs::write(
        vault.join("a.md"),
        "---\ntags: [Alpha, work/deep]\nstatus: draft\naliases: [The A]\n---\n\
         First note, see [[b]].\n\n- [x] ship it\n",
    )
    .unwrap();
    std::fs::write(vault.join("b.md"), "Second note #beta [[ghost]]\n").unwrap();
    std::fs::write(vault.join("c.md"), "Third: ![[img.png]] and [[a]].\n").unwrap();
    std::fs::write(vault.join("img.png"), b"\x89PNG bytes").unwrap();

    // Auto-detected via .obsidian/.
    let args = ImportArgs {
        files: vec![vault.clone()],
        ..Default::default()
    };
    let report = run(&app, &opts(), &args).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.imported.len(), 3);

    let reads = app.reads(&opts()).unwrap();
    let by_title = |t: &str| {
        let n = report.imported.iter().find(|n| n.title == t).unwrap();
        reads.resolve_live("default", n.id.unwrap()).unwrap().0
    };
    let (a, b, c) = (by_title("a.md"), by_title("b.md"), by_title("c.md"));

    // Content is verbatim — wikilinks stay inert text.
    let content = String::from_utf8(reads.bytes(a).unwrap()).unwrap();
    assert!(content.contains("[[b]]"));

    // Dialect tags and categories land in the overlay.
    let eff = reads.effective(a);
    assert!(eff.tags.contains("alpha"));
    assert!(eff.categories.contains("work::deep"));
    // Checkbox tasks became overlay todos.
    assert_eq!(eff.todos.len(), 1);
    assert!(eff.todos[0].done);
    assert_eq!(eff.todos[0].raw, "ship it");
    // Wikilinks became entry-resolved overlay references.
    assert!(eff.edges.contains_key(&b.hyphenated().to_string()));
    let eff_c = reads.effective(c);
    assert!(eff_c.edges.contains_key(&a.hyphenated().to_string()));

    // The unresolved wikilink is reported, not fabricated.
    let eff_b = reads.effective(b);
    assert!(eff_b.edges.is_empty());
    assert!(eff_b.tags.contains("beta"));
    let nb = report.imported.iter().find(|n| n.title == "b.md").unwrap();
    assert_eq!(nb.unresolved, vec!["ghost"]);

    // The embed became an attachment on the referencing jot.
    let atts = reads.attachments_of(c);
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0].1.filename, "img.png");
    assert_eq!(reads.bytes(atts[0].0).unwrap(), b"\x89PNG bytes");

    // Properties and aliases dropped, reported per note.
    let na = report.imported.iter().find(|n| n.title == "a.md").unwrap();
    assert!(na.dropped.iter().any(|d| d.contains("status")), "{:?}", na.dropped);
    assert!(na.dropped.iter().any(|d| d.contains("aliases")), "{:?}", na.dropped);
    assert_eq!(na.references, vec![format!("default:{}", nb.id.unwrap())]);

    // Re-import is a fingerprint no-op.
    let report = run(&app, &opts(), &args).unwrap();
    assert_eq!(report.skipped.len(), 3);
    assert!(report.imported.is_empty());

    // --dialect none forces the plain directory binding.
    let tmp2 = tempfile::tempdir().unwrap();
    let client2 = client_for(&tmp2);
    App::init(client2.clone(), None).unwrap();
    let app2 = App::open(client2).unwrap();
    let args2 = ImportArgs {
        files: vec![vault],
        dialect: Some("none".into()),
        ..Default::default()
    };
    let report2 = run(&app2, &opts(), &args2).unwrap();
    assert_eq!(report2.imported.len(), 3);
    let reads2 = app2.reads(&opts()).unwrap();
    let n = report2.imported.iter().find(|n| n.title == "a.md").unwrap();
    let (e, _) = reads2.resolve_live("default", n.id.unwrap()).unwrap();
    let eff2 = reads2.effective(e);
    assert!(!eff2.tags.contains("alpha"), "plain binding must not parse dialect");
    assert!(eff2.edges.is_empty());
}

#[test]
fn obsidian_embed_filenames_sanitized_and_deduped() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    let vault = tmp.path().join("obsvault");
    std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
    std::fs::create_dir_all(vault.join("a")).unwrap();
    std::fs::create_dir_all(vault.join("b")).unwrap();
    // Decomposed (non-NFC) name, as macOS vaults produce.
    let decomposed = "re\u{301}sume\u{301}.png";
    std::fs::write(vault.join(decomposed), b"resume bytes").unwrap();
    std::fs::write(vault.join("a/img.png"), b"first").unwrap();
    std::fs::write(vault.join("b/img.png"), b"second").unwrap();
    std::fs::write(
        vault.join("n.md"),
        format!("![[{decomposed}]] then ![[a/img.png]] and ![[b/img.png]]\n"),
    )
    .unwrap();

    let args = ImportArgs {
        files: vec![vault],
        ..Default::default()
    };
    let report = run(&app, &opts(), &args).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.imported.len(), 1);
    assert_eq!(
        report.imported[0].attachments,
        vec!["re_sume_.png", "img.png", "img_2.png"]
    );

    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads
        .resolve_live("default", report.imported[0].id.unwrap())
        .unwrap();
    let atts = reads.attachments_of(e);
    assert_eq!(atts.len(), 3);
    let bytes_of = |name: &str| {
        let (ae, _) = atts.iter().find(|(_, a)| a.filename == name).unwrap();
        reads.bytes(*ae).unwrap()
    };
    assert_eq!(bytes_of("re_sume_.png"), b"resume bytes");
    assert_eq!(bytes_of("img.png"), b"first");
    assert_eq!(bytes_of("img_2.png"), b"second");
}
