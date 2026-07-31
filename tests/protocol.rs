//! Current-protocol (1.0-draft.5) features end-to-end: migration
//! gating, the former-spelling marker alias, file spaces, the
//! capture inbox, templates, saved queries, hybrid search,
//! transclusion, agenda views, graph export, and the
//! notes-directory import binding.

use std::collections::BTreeMap;

use cuj::app::{App, Opts, parse_ref};
use cuj::client::ClientConfig;
use cuj::error::Error;
use cuj::facts;
use cuj::queries::{self, HitTarget};

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

/// Rewind a fresh vault's protocol marker to an older spelling.
fn rewind_marker(app: &App, marker: &str) {
    let (root, _) = app
        .vault
        .current_facts(facts::K_CUJ_VAULT)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let mut body = serde_json::Map::new();
    body.insert(
        "protocol".into(),
        serde_json::Value::String(marker.into()),
    );
    app.vault
        .commit(&[metatheca::Fact::new(facts::K_CUJ_VAULT, root, body).unwrap()])
        .unwrap();
}

fn downgrade(app: &App) {
    rewind_marker(app, "0.4");
}

#[test]
fn former_spelling_is_the_same_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    rewind_marker(&app, "0.5");

    // Full surface, no gating: a current-only write works.
    app.create(&opts(), "alias vault jot ..tag").unwrap();
    app.template_set(&opts(), "daily", "x").unwrap();

    // migrate normalizes the marker in one committed fact.
    let msg = app.migrate().unwrap();
    assert!(msg.contains("renamed"), "{msg}");
    assert!(msg.contains(facts::PROTOCOL), "{msg}");
    let app = App::open(client_for(&tmp)).unwrap();
    let reads = app.reads(&opts()).unwrap();
    assert_eq!(reads.protocol.as_deref(), Some(facts::PROTOCOL));
    assert!(app.migrate().unwrap().contains("already"));
}

#[test]
fn migration_gates_05_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    downgrade(&app);

    // The 0.4 surface still serves.
    app.create(&opts(), "works on 0.4 ..old").unwrap();
    assert_eq!(queries::list(&app, &opts(), 0, None).unwrap().len(), 1);

    // 0.5-only writes answer Unsupported until migration.
    let err = app.space_add("wiki", None).unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");
    let err = app.template_set(&opts(), "daily", "x").unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err}");

    let msg = app.migrate().unwrap();
    assert!(msg.contains("migrated"), "{msg}");
    assert!(app.migrate().unwrap().contains("already"));
    app.space_add("wiki", None).unwrap();
}

#[test]
fn file_spaces_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = client_for(&tmp);
    let wiki = tmp.path().join("wiki");
    std::fs::create_dir_all(wiki.join("sub")).unwrap();
    std::fs::write(wiki.join("ideas.md"), "garden planning ..outdoor\n").unwrap();
    std::fs::write(wiki.join("sub/deep.md"), "nested note\n").unwrap();
    std::fs::write(wiki.join(".hidden.md"), "never observed\n").unwrap();
    client.spaces.insert("wiki".into(), wiki.clone());

    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.space_add("wiki", None).unwrap();
    let report = app.observe("wiki").unwrap();
    assert_eq!(report.len(), 2, "{report:?}");

    // Observed files are listed with their parsed tags.
    let files = queries::files(&app, &opts(), Some("wiki")).unwrap();
    assert_eq!(files.len(), 2);
    let ideas = files.iter().find(|f| f.path == "ideas.md").unwrap();
    assert!(ideas.tags.contains(&"outdoor".to_string()));

    // find matches file-jot tags under --profile all.
    let mut all = opts();
    all.profile = Some("all".into());
    let f = queries::parse_find_args(&["..outdoor".to_string()]).unwrap();
    let page = queries::find(&app, &all, &f, None, None).unwrap();
    assert!(
        page.hits
            .iter()
            .any(|h| matches!(h, HitTarget::File(f) if f.path == "ideas.md")),
        "file hit expected"
    );
    // ...and under the default profile scope: file-jots have no
    // profile and appear in any scope (SPEC §8.6).
    let page = queries::find(&app, &opts(), &f, None, None).unwrap();
    assert!(
        page.hits
            .iter()
            .any(|h| matches!(h, HitTarget::File(f) if f.path == "ideas.md")),
        "file hit expected under profile scope"
    );
    // grep scans file-jots in any scope (SPEC §8.6).
    let empty = queries::parse_find_args(&[]).unwrap();
    let g = queries::grep(&app, &opts(), "garden", &empty).unwrap();
    assert!(
        g.iter().any(|h| matches!(
            &h.source,
            queries::GrepSource::File { path, .. } if path == "ideas.md"
        )),
        "grep must scan observed files"
    );

    // Jots reference files; backlinks resolve them.
    app.create(&opts(), "see the plan in ///wiki/ideas.md")
        .unwrap();
    let back =
        queries::backlinks_file(&app, &opts(), Some("wiki"), "ideas.md", None, None).unwrap();
    assert_eq!(back.hits.len(), 1);

    // Bare //path resolves via the profile's default_space.
    app.update_profile("default", |p| p.default_space = Some("wiki".into()))
        .unwrap();
    app.create(&opts(), "also //ideas.md applies").unwrap();
    let back = queries::backlinks_file(&app, &opts(), None, "ideas.md", None, None).unwrap();
    assert_eq!(back.hits.len(), 2);

    // show_file returns content; edits round-trip on re-observe.
    let (view, content) = queries::show_file(&app, &opts(), Some("wiki"), "ideas.md").unwrap();
    assert!(content.contains("garden planning"));
    let m0 = view.modified_at_ns.unwrap();
    std::fs::write(wiki.join("ideas.md"), "garden planning v2 ..outdoor\n").unwrap();
    let report = app.observe("wiki").unwrap();
    assert_eq!(report.len(), 1);
    let (view, content) = queries::show_file(&app, &opts(), Some("wiki"), "ideas.md").unwrap();
    assert!(content.contains("v2"));
    assert!(view.modified_at_ns.unwrap() > m0);

    // Deletion retracts on the next observe; space rm clears all.
    std::fs::remove_file(wiki.join("sub/deep.md")).unwrap();
    app.observe("wiki").unwrap();
    assert_eq!(
        queries::files(&app, &opts(), Some("wiki")).unwrap().len(),
        1
    );
    let retracted = app.space_rm("wiki").unwrap();
    assert_eq!(retracted, 1);
    assert!(queries::files(&app, &opts(), None).unwrap().is_empty());

    let fsck = queries::fsck(&app).unwrap();
    assert!(fsck.clean(), "{:?}", fsck.problems);
}

#[test]
fn inbox_ingest() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    App::init(client.clone(), Some("work")).unwrap();
    let app = App::open(client).unwrap();

    let inbox = app.root().join("inbox");
    std::fs::create_dir_all(inbox.join("work")).unwrap();
    std::fs::write(inbox.join("thought.md"), "captured on the go ..mobile").unwrap();
    std::fs::write(inbox.join("work/task.txt"), "work capture ??follow up").unwrap();
    std::fs::write(inbox.join(".partial.md"), "still being written").unwrap();
    std::fs::write(inbox.join("bad.md"), b"\xff\xfe not text".as_slice()).unwrap();

    let report = app.ingest(&opts()).unwrap();
    assert_eq!(report.len(), 3, "{report:?}");
    assert!(report.iter().any(|l| l.contains("thought.md ->")));
    assert!(report.iter().any(|l| l.contains("task.txt ->")));
    assert!(report.iter().any(|l| l.contains("bad.md: ")));

    // Ingested captures became jots in their target profiles; the
    // failing one stayed in place.
    let reads = app.reads(&opts()).unwrap();
    let (e, i) = reads.resolve_live("default", 1).unwrap();
    assert_eq!(i.ext, "md");
    assert!(reads.effective(e).tags.contains("mobile"));
    let (_, i) = reads.resolve_live("work", 1).unwrap();
    assert_eq!(i.ext, "txt");
    assert!(!inbox.join("thought.md").exists());
    assert!(inbox.join("bad.md").exists());
    assert!(inbox.join(".partial.md").exists());

    // Re-running ingests nothing new (bad.md fails again).
    let report = app.ingest(&opts()).unwrap();
    assert_eq!(report.len(), 1);
}

#[test]
fn templates_and_new() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    app.template_set(
        &opts(),
        "standup",
        "::work::standup {{date}}\n{{text}}\n??follow up on notes",
    )
    .unwrap();
    let views = queries::templates(&app, &opts()).unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].name, "standup");

    let ident = app
        .new_from_template(&opts(), "standup", "shipped the parser")
        .unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", ident.id).unwrap();
    let content = String::from_utf8_lossy(&reads.bytes(e).unwrap()).into_owned();
    assert!(content.contains("shipped the parser"));
    assert!(
        !content.contains("{{"),
        "placeholders substituted: {content}"
    );
    let eff = reads.effective(e);
    assert!(eff.categories.contains("work::standup"));
    assert_eq!(eff.todos.len(), 1);

    // Text appends when the template has no {{text}} slot.
    app.template_set(&opts(), "plain", "::inbox").unwrap();
    let ident = app
        .new_from_template(&opts(), "plain", "the thought")
        .unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", ident.id).unwrap();
    assert!(String::from_utf8_lossy(&reads.bytes(e).unwrap()).contains("the thought"));

    app.template_rm(&opts(), "plain").unwrap();
    assert_eq!(queries::templates(&app, &opts()).unwrap().len(), 1);
}

#[test]
fn saved_queries_run_as_find() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    app.create(&opts(), "urgent thing ..urgent").unwrap();
    app.create(&opts(), "calm thing ..calm").unwrap();
    app.query_save(&opts(), "fires", &["..urgent".to_string()])
        .unwrap();

    let page = queries::run_saved_query(&app, &opts(), "fires", None, None).unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(queries::saved_queries(&app, &opts()).unwrap().len(), 1);
    app.query_rm(&opts(), "fires").unwrap();
    assert!(matches!(
        queries::run_saved_query(&app, &opts(), "fires", None, None),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn hybrid_search_and_engines() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.create(&opts(), "quiet gardens and soil").unwrap();
    app.create(&opts(), "loud machines and metal").unwrap();

    // Vault init created the zetetes chain: search is hybrid.
    let hits = queries::search(&app, &opts(), "gardens", 5, 0, false).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].engine, "hybrid");
    assert!(matches!(&hits[0].target, HitTarget::Jot(j) if j.id == 1));

    // --lexical forces the BM25 ranking.
    let hits = queries::search(&app, &opts(), "gardens", 5, 0, true).unwrap();
    assert_eq!(hits[0].engine, "lexical");

    let hits =
        queries::similar(&app, &opts(), queries::SimilarTarget::Text("gardens"), 5, 0).unwrap();
    assert!(!hits.is_empty());
    assert_eq!(hits[0].engine, "semantic");

    // reindex advances ALL chains (SPEC §9.6), the fusion pins
    // included: right after it, the zetetes state is at head, so
    // search --stale is not stale.
    app.create(&opts(), "brand new thought about rivers").unwrap();
    app.reindex().unwrap();
    let z = cuj::ext::open_zetetes(app.root()).unwrap().unwrap();
    assert_eq!(
        z.get_state(&z.current_hash().unwrap()).unwrap().metatheca,
        app.vault.current_state_hash().unwrap()
    );
    let mut stale = opts();
    stale.stale = true;
    let hits = queries::search(&app, &stale, "rivers", 5, 0, false).unwrap();
    assert!(
        hits.iter()
            .any(|h| matches!(&h.target, HitTarget::Jot(j) if j.id == 3)),
        "post-reindex --stale search must see the new jot"
    );
}

#[test]
fn transclusion_expands_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    app.create(
        &opts(),
        "intro line\n!!core the essential point\nafter core\n!!outro closing words",
    )
    .unwrap();
    app.create(
        &opts(),
        "digest:\nsee --1#core here\nand --1:1-1 too\nplain ref --1 stays",
    )
    .unwrap();

    let r = parse_ref("--2").unwrap();
    match queries::show(&app, &opts(), &r, true).unwrap() {
        queries::ShowOutcome::Shown { content, .. } => {
            assert!(content.contains("the essential point"), "{content}");
            assert!(content.contains("intro line"), "{content}");
            // The suffix-less reference is not expanded.
            assert!(content.contains("plain ref --1 stays"), "{content}");
        }
        _ => panic!("expected Shown"),
    }

    // Cycles are guarded: two jots transcluding each other.
    app.create(&opts(), "a !!x --4#y loop\nbody a").unwrap();
    app.create(&opts(), "b !!y --3#x loop\nbody b").unwrap();
    let r = parse_ref("--3").unwrap();
    match queries::show(&app, &opts(), &r, true).unwrap() {
        queries::ShowOutcome::Shown { content, .. } => {
            assert!(content.contains("body b"), "one level expands: {content}");
        }
        _ => panic!("expected Shown"),
    }
}

#[test]
fn today_agenda_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    app.create(&opts(), "morning note ??call@2030-01-02 alice")
        .unwrap();
    app.create(&opts(), "second ??undated task").unwrap();
    app.create(&opts(), "links to --1").unwrap();

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let view = queries::today(&app, &opts(), Some(&today)).unwrap();
    assert_eq!(view.jots.len(), 3, "all created today");

    let items = queries::agenda(&app, &opts(), None).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].due_ns.is_some(), "dated first");
    assert!(items[1].due_ns.is_none());

    let g = queries::graph(&app, &opts()).unwrap();
    assert_eq!(g.nodes.len(), 3);
    assert!(
        g.edges
            .iter()
            .any(|e| e.from == "--default--3" && e.to == "--default--1")
    );
}

#[test]
fn directory_import() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    let notes = tmp.path().join("notes");
    std::fs::create_dir_all(notes.join("sub")).unwrap();
    std::fs::write(notes.join("one.md"), "first note ..imported\n").unwrap();
    std::fs::write(notes.join("sub/two.txt"), "second note\n").unwrap();
    std::fs::write(notes.join("skip.bin"), b"\x00\x01").unwrap();

    let args = cuj::import::ImportArgs {
        files: vec![notes.clone()],
        categories: vec!["evermore".into()],
        ..Default::default()
    };
    let report = cuj::import::run(&app, &opts(), &args).unwrap();
    assert_eq!(report.imported.len(), 2, "{:?}", report.errors);
    assert!(report.errors.is_empty());

    let reads = app.reads(&opts()).unwrap();
    let (e1, i1) = reads.resolve_live("default", 1).unwrap();
    assert_eq!(i1.ext, "md");
    let eff = reads.effective(e1);
    assert!(eff.tags.contains("imported"));
    assert!(eff.categories.contains("evermore"));

    // Re-import is a fingerprint no-op.
    let report = cuj::import::run(&app, &opts(), &args).unwrap();
    assert_eq!(report.skipped.len(), 2);
    assert!(report.imported.is_empty());
}

#[test]
fn obsidian_dialect_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = client_for(&tmp);
    let wiki = tmp.path().join("wiki");
    std::fs::create_dir_all(wiki.join("sub")).unwrap();
    std::fs::write(
        wiki.join("ideas.md"),
        "---\ntags: [Outdoor, hobby/climbing]\nstatus: draft\n---\n\
         Plan the season. See [[Plans]].\n\n- [ ] pack rope\n",
    )
    .unwrap();
    std::fs::write(
        wiki.join("sub/Plans.md"),
        "Route list #trip\n\n![[topo.png]] and [[missing]]\n",
    )
    .unwrap();
    std::fs::write(wiki.join("topo.png"), b"\x89PNG fake").unwrap();
    client.spaces.insert("wiki".into(), wiki.clone());

    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.space_add("wiki", Some("obsidian")).unwrap();
    app.observe("wiki").unwrap();

    // The dialect facts land on md file entries.
    let reads = app.reads(&opts()).unwrap();
    let ideas = reads.files[&("wiki".into(), "ideas.md".into())];
    let o = &reads.obsidian[&ideas];
    assert_eq!(o.tags, vec!["outdoor"]);
    assert_eq!(o.categories, vec!["hobby::climbing"]);
    assert_eq!(o.links.len(), 1);
    assert_eq!(o.links[0].space, "wiki");
    assert_eq!(o.links[0].path, "sub/Plans.md");
    assert_eq!(o.links[0].written_as, "Plans");
    assert_eq!(o.properties["status"], serde_json::json!("draft"));
    assert_eq!(o.tasks.len(), 1);
    assert!(!o.tasks[0].done);
    let plans = reads.files[&("wiki".into(), "sub/Plans.md".into())];
    let op = &reads.obsidian[&plans];
    assert_eq!(op.tags, vec!["trip"]);
    assert_eq!(op.embeds[0].path, "topo.png");
    assert_eq!(op.unresolved_links, vec!["missing"]);

    // Obsidian tags and categories join the effective index and
    // the find disjunction.
    let mut all = opts();
    all.profile = Some("all".into());
    let f = queries::parse_find_args(&["..outdoor".to_string()]).unwrap();
    let page = queries::find(&app, &all, &f, None, None).unwrap();
    assert!(
        page.hits
            .iter()
            .any(|h| matches!(h, HitTarget::File(f) if f.path == "ideas.md")),
        "obsidian tag should find the file-jot"
    );
    let f = queries::parse_find_args(&["::hobby".to_string()]).unwrap();
    let page = queries::find(&app, &all, &f, None, None).unwrap();
    assert_eq!(page.hits.len(), 1);

    // Wikilink backlinks answer alongside Jot Syntax ones; embeds
    // count too.
    let back =
        queries::backlinks_file(&app, &opts(), Some("wiki"), "sub/Plans.md", None, None).unwrap();
    assert!(
        back.hits
            .iter()
            .any(|h| matches!(h, HitTarget::File(f) if f.path == "ideas.md")),
        "wikilink backlink expected"
    );
    let back =
        queries::backlinks_file(&app, &opts(), Some("wiki"), "topo.png", None, None).unwrap();
    assert!(
        back.hits
            .iter()
            .any(|h| matches!(h, HitTarget::File(f) if f.path == "sub/Plans.md")),
        "embed backlink expected"
    );

    // Graph carries the wikilink edge.
    let g = queries::graph(&app, &opts()).unwrap();
    assert!(
        g.edges
            .iter()
            .any(|e| e.from == "///wiki/ideas.md" && e.to == "///wiki/sub/Plans.md"),
        "{:?}",
        g.edges
    );

    // Re-observe with nothing changed is a no-op.
    assert!(app.observe("wiki").unwrap().is_empty());
}

#[test]
fn obsidian_dialect_reconciliation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = client_for(&tmp);
    let wiki = tmp.path().join("wiki");
    std::fs::create_dir_all(&wiki).unwrap();
    std::fs::write(wiki.join("note.md"), "hello #tagged\n").unwrap();
    client.spaces.insert("wiki".into(), wiki.clone());

    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    // Plain space: no dialect facts.
    app.space_add("wiki", None).unwrap();
    app.observe("wiki").unwrap();
    let reads = app.reads(&opts()).unwrap();
    let entry = reads.files[&("wiki".into(), "note.md".into())];
    assert!(!reads.obsidian.contains_key(&entry));

    // Declaring the dialect reconciles unchanged files on the
    // next observe.
    assert_eq!(app.space_add("wiki", Some("obsidian")).unwrap(), "updated");
    let report = app.observe("wiki").unwrap();
    assert_eq!(report, vec!["indexed  ///wiki/note.md".to_string()]);
    let reads = app.reads(&opts()).unwrap();
    assert_eq!(reads.obsidian[&entry].tags, vec!["tagged"]);
    // Converged: nothing further to emit.
    assert!(app.observe("wiki").unwrap().is_empty());

    // Removing the dialect retracts via the empty snapshot.
    assert_eq!(app.space_add("wiki", None).unwrap(), "updated");
    let report = app.observe("wiki").unwrap();
    assert_eq!(report, vec!["indexed  ///wiki/note.md".to_string()]);
    let reads = app.reads(&opts()).unwrap();
    assert!(!reads.obsidian.contains_key(&entry));
    assert!(app.observe("wiki").unwrap().is_empty());

    // Idempotent re-add.
    assert_eq!(app.space_add("wiki", None).unwrap(), "unchanged");

    let fsck = queries::fsck(&app).unwrap();
    assert!(fsck.clean(), "{:?}", fsck.problems);
}
