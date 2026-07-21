//! CLI-layer tests through the real binary: argv parsing, global
//! flags, dispatch, exit codes, and output shapes (SPEC §11).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Cli {
    config: PathBuf,
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Cli {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!("vault = \"{}\"\n", dir.path().join("vault").display()),
        )
        .unwrap();
        Cli { config, dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cuj"))
            .args(args)
            .env("CUJ_CONFIG", &self.config)
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "cuj {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn fails(&self, args: &[&str], exit: i32) -> String {
        let out = self.run(args);
        assert_eq!(
            out.status.code(),
            Some(exit),
            "cuj {args:?}: expected exit {exit}, got {:?}: {}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

fn seeded() -> Cli {
    let cli = Cli::new();
    cli.ok(&["init"]);
    cli.ok(&["--", "alpha", "note", "..work", "::proj::one"]);
    cli.ok(&["--", "beta", "with", "a", "ref", "--1"]);
    cli
}

#[test]
fn usage_and_unknown_command() {
    let cli = Cli::new();
    let usage = cli.ok(&[]);
    assert!(usage.contains("usage:"));
    let err = cli.fails(&["frobnicate"], 2);
    assert!(err.contains("unknown command"));
}

#[test]
fn create_show_and_fragments() {
    let cli = Cli::new();
    cli.ok(&["init"]);
    let out = cli.ok(&["--", "top line\nmiddle !!mark\nbottom line"]);
    assert_eq!(out.trim(), "--default--1");

    // Bare-ref show.
    let shown = cli.ok(&["--1"]);
    assert!(shown.contains("top line"));
    assert!(shown.contains("created  "));
    assert!(shown.contains("modified "));

    // Line-range fragment.
    let frag = cli.ok(&["show", "--1:2-2"]);
    assert!(frag.contains("middle"));
    assert!(!frag.contains("top line\nmiddle"));

    // Bookmark fragment runs to EOF.
    let frag = cli.ok(&["show", "--1#mark"]);
    assert!(frag.contains("middle") && frag.contains("bottom"));

    // Missing bookmark: NotFound exit code (SPEC §11.3).
    let err = cli.fails(&["show", "--1#nope"], 3);
    assert!(err.contains("bookmark"));
}

#[test]
fn grep_with_filters() {
    let cli = seeded();
    let hits = cli.ok(&["grep", "alpha"]);
    assert!(hits.contains("--1:1: alpha note"));
    // Filter narrows to tagged jots only.
    let hits = cli.ok(&["grep", "..work", "--", "note"]);
    assert!(hits.contains("--1") && !hits.contains("--2"));
    cli.fails(&["grep", ""], 2);
}

#[test]
fn history_and_log() {
    let cli = seeded();
    let hist = cli.ok(&["history", "--1"]);
    assert!(hist.contains("created as --default--1"));
    assert!(hist.contains("content blob"));
    let log = cli.ok(&["log", "-n", "2"]);
    assert_eq!(log.lines().count(), 2);
    assert!(log.contains("fact(s)"));
}

#[test]
fn attach_detach_attachments() {
    let cli = seeded();
    let file = cli.path().join("paper.txt");
    std::fs::write(&file, "attached words here").unwrap();
    let out = cli.ok(&["attach", file.to_str().unwrap(), "--1"]);
    assert!(out.contains("attached paper.txt"));
    let listing = cli.ok(&["attachments", "--1"]);
    assert!(listing.contains("paper.txt") && listing.contains("19 bytes"));
    // The stored core/mime is listed (SPEC §10.22, §7.1).
    assert!(listing.contains("text/plain"), "{listing}");
    // Re-attach replaces.
    let out = cli.ok(&["attach", file.to_str().unwrap(), "--1"]);
    assert!(out.contains("(replaced)"));
    let out = cli.ok(&["detach", "paper.txt", "--1"]);
    assert!(out.contains("detached paper.txt"));
    assert_eq!(cli.ok(&["attachments", "--1"]).trim(), "");
    cli.ok(&["fsck"]);
}

#[test]
fn promote_demote_and_profiles() {
    let cli = seeded();
    // tag -> category rewrites content.
    let out = cli.ok(&["promote", "..work"]);
    assert!(out.contains("--default--1 rewritten"));
    assert!(cli.ok(&["--1"]).contains("::work"));
    let out = cli.ok(&["demote", "::work"]);
    assert!(out.contains("rewritten"));
    assert!(cli.ok(&["--1"]).contains("..work"));

    // category -> profile moves the jot.
    let out = cli.ok(&["promote", "::proj::one"]);
    assert!(out.contains("created profile \"one\""));
    assert!(out.contains("--default--1 -> --one--1"));
    let profiles = cli.ok(&["profiles"]);
    assert!(profiles.contains("one"));
    // The old reference is NotFound (SPEC §5.3) with a courtesy
    // hint in the message.
    let out = cli.run(&["show", "--1"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("moved to --one--1"));
    cli.ok(&["fsck"]);
}

#[test]
fn mv_and_scopes() {
    let cli = seeded();
    cli.ok(&["init", "work"]);
    let out = cli.ok(&["mv", "--to", "work", "--2"]);
    assert!(out.contains("--default--2 -> --work--1"));
    // --profile all lists across profiles; bare refs are ambiguous.
    let all = cli.ok(&["--profile", "all", "list"]);
    assert!(all.contains("--default--1") && all.contains("--work--1"));
    let err = cli.fails(&["--profile", "all", "show", "--1"], 2);
    assert!(err.contains("ambiguous"));
    cli.ok(&["--profile", "all", "show", "--work--1"]);
}

#[test]
fn todo_cli_round_trip() {
    let cli = Cli::new();
    cli.ok(&["init"]);
    let out = cli.ok(&["todo", "??call", "the", "office"]);
    assert_eq!(out.trim(), "--default--1");
    cli.ok(&["todo", "--1", "??file the report"]);
    let shown = cli.ok(&["--1"]);
    assert!(shown.contains("1. [ ]") && shown.contains("2. [ ]"));
    let out = cli.ok(&["todo", "done", "--1", "2"]);
    assert!(out.contains("todo done"));
    assert!(cli.ok(&["--1"]).contains("[x]"));
}

#[test]
fn config_switch_ext() {
    let cli = seeded();
    assert_eq!(
        cli.ok(&["config", "get", "active_profile"]).trim(),
        "default"
    );
    cli.ok(&["config", "set", "editor", "true"]);
    assert_eq!(cli.ok(&["config", "get", "editor"]).trim(), "true");
    cli.ok(&["config", "set", "extract.docx", "pandoc {in} -o {out}"]);
    assert!(
        cli.ok(&["config", "get", "extract.docx"])
            .contains("pandoc")
    );
    cli.fails(&["config", "get", "extract.nope"], 3);
    cli.fails(&["config", "set", "--vault", "bogus", "x"], 3);
    cli.fails(&["config", "set", "--vault", "jots_frozen", "maybe"], 2);
    cli.fails(&["config", "set", "--vault", "required", "notaprefix"], 2);
    cli.ok(&["config", "set", "--vault", "default_ext", "txt"]);
    assert_eq!(cli.ok(&["ext"]).trim(), "txt");
    cli.ok(&["ext", "set", "--md"]);
    assert_eq!(cli.ok(&["ext"]).trim(), "md");

    cli.ok(&["init", "second"]);
    cli.ok(&["switch", "second"]);
    assert_eq!(cli.ok(&["profile"]).trim(), "second");
    cli.fails(&["switch", "nonexistent"], 3);
}

#[test]
fn json_rejected_on_writes_accepted_on_reads() {
    let cli = seeded();
    for args in [
        vec!["--json", "--", "text"],
        vec!["--json", "label", "--1", "..x"],
        vec!["--json", "link", "--1", "--2"],
        vec!["--json", "todo", "--1", "??x"],
        vec!["--json", "rm", "--1"],
        vec!["--json", "promote", "..work"],
        vec!["--json", "switch", "default"],
        vec!["--json", "reindex"],
        vec!["--json", "config", "set", "editor", "vi"],
    ] {
        let out = cli.run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?} must reject --json");
        // Errors under --json are the documented object (SPEC §11.2).
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON error object");
        assert_eq!(v["error"]["code"], "Usage");
    }
    // Reads accept it and print one JSON value.
    let v: serde_json::Value = serde_json::from_str(&cli.ok(&["--json", "list"])).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);
    let v: serde_json::Value = serde_json::from_str(&cli.ok(&["--json", "show", "--1"])).unwrap();
    assert!(v["content"].as_str().unwrap().contains("alpha"));
    assert!(v["modified_at_ns"].is_i64());
}

#[test]
fn export_and_backlinks() {
    let cli = seeded();
    let dir = cli.path().join("out");
    let out = cli.ok(&["export", dir.to_str().unwrap()]);
    assert!(out.contains("exported 2 file(s)"));
    assert!(dir.join("default/1.md").is_file());

    let back = cli.ok(&["backlinks", "--1"]);
    assert!(back.contains("--2"));
    let refs = cli.ok(&["references", "--2"]);
    assert!(refs.contains("--1"));
}

#[test]
fn label_rejects_bad_tokens() {
    let cli = seeded();
    let err = cli.fails(&["label", "--1", ".."], 2);
    assert!(err.contains("invalid tag"));
    let err = cli.fails(&["label", "--1", "::a b"], 2);
    assert!(err.contains("invalid category"));
}

#[test]
fn log_and_fsck_json() {
    let cli = seeded();
    let v: serde_json::Value = serde_json::from_str(&cli.ok(&["--json", "log"])).unwrap();
    assert!(!v.as_array().unwrap().is_empty());
    assert!(v[0]["state"].is_string());
    let v: serde_json::Value = serde_json::from_str(&cli.ok(&["--json", "fsck"])).unwrap();
    assert!(v["problems"].as_array().unwrap().is_empty());
    assert!(v["jots"].as_u64().unwrap() >= 2);
}

#[test]
fn backlinks_respects_profile_scope() {
    let cli = seeded();
    cli.ok(&["init", "work"]);
    cli.ok(&["--profile", "work", "--", "target note"]);
    cli.ok(&["--", "see --work--1"]);
    // The source jot lives in default: visible from the default
    // scope, filtered out of the work scope, back under all.
    assert!(cli.ok(&["backlinks", "--work--1"]).contains("--3"));
    assert_eq!(
        cli.ok(&["--profile", "work", "backlinks", "--1"]).trim(),
        ""
    );
    assert!(
        cli.ok(&["--profile", "all", "backlinks", "--work--1"])
            .contains("--3")
    );
}

#[test]
fn todo_trailing_ref_is_ambiguous() {
    let cli = seeded();
    // No leading ref + a ref-shaped token after the construct:
    // refused, not guessed (C3 ruling: strict).
    let err = cli.fails(&["todo", "??pay", "--1"], 2);
    assert!(err.contains("ambiguous"), "{err}");
    let err = cli.fails(&["todo", "??remind", "about", "--1", "tomorrow"], 2);
    assert!(err.contains("ambiguous"), "{err}");
    // A leading ref signals the convention; embedded refs pass.
    cli.ok(&["todo", "--1", "??check", "--2"]);
}

#[test]
fn references_json_one_element_per_typed_edge() {
    let cli = seeded();
    cli.ok(&["--", "typed --1::cites::weak and --1::uses"]);
    let v: serde_json::Value =
        serde_json::from_str(&cli.ok(&["--json", "show", "--3"])).unwrap();
    let refs = v["references"].as_array().unwrap().clone();
    assert_eq!(refs.len(), 2, "{refs:?}");
    let cats: Vec<&str> = refs
        .iter()
        .map(|r| r["category"].as_str().unwrap())
        .collect();
    assert!(cats.contains(&"cites::weak") && cats.contains(&"uses"));
    // Typed edges are distinct from the bare edge (SPEC 5.5), so a
    // bare link still commits and appears as its own element with
    // its own written form.
    cli.ok(&["link", "--3", "--1"]);
    let v: serde_json::Value =
        serde_json::from_str(&cli.ok(&["--json", "show", "--3"])).unwrap();
    let refs = v["references"].as_array().unwrap().clone();
    assert_eq!(refs.len(), 3, "{refs:?}");
    assert!(
        refs.iter()
            .any(|r| r["category"].is_null() && r["as"] == "default:1"),
        "{refs:?}"
    );
}

#[test]
fn import_json_report() {
    let cli = Cli::new();
    cli.ok(&["init"]);
    let enex = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-export SYSTEM "http://xml.evernote.com/pub/evernote-export4.dtd">
<en-export export-date="20240401T000000Z" application="Evernote" version="10.93.4">
<note>
<title>hello note</title>
<content><![CDATA[<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE en-note SYSTEM "http://xml.evernote.com/pub/enml2.dtd">
<en-note><div>hello body</div></en-note>]]></content>
<created>20240315T090000Z</created>
<updated>20240315T090000Z</updated>
<tag>alpha</tag>
</note>
</en-export>"#;
    let path = cli.path().join("one.enex");
    std::fs::write(&path, enex).unwrap();
    // import is the one write command sanctioned for --json
    // (SPEC §10.33): one documented JSON report on stdout.
    let out = cli.ok(&["--json", "import", path.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["imported"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["imported"][0]["title"], "hello note");
    assert_eq!(v["imported"][0]["id"], 1);
    // Re-import dedups; the report says so in the same shape.
    let out = cli.ok(&["--json", "import", path.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["imported"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(v["skipped"].as_array().unwrap().len(), 1, "{v}");
}
