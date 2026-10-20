//! The Quarb binding (SPEC appendix): the vault as an arbor,
//! queried through `cuj::arbor::VaultArbor` + the quarb engine.

use std::collections::BTreeMap;

use cuj::app::{App, Opts, parse_ref};
use cuj::arbor::VaultArbor;
use cuj::client::ClientConfig;
use quarb::QueryResult;

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

/// Vault with two profiles, tagged/linked/todo jots, and an
/// observed space with a nested file referenced from a jot.
fn seeded() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = client_for(&tmp);
    let wiki = tmp.path().join("wiki");
    std::fs::create_dir_all(wiki.join("plans")).unwrap();
    std::fs::write(wiki.join("plans/roadmap.md"), "the roadmap ..vision\n").unwrap();
    client.spaces.insert("wiki".into(), wiki);

    App::init(client.clone(), None).unwrap();
    let app = App::open(client.clone()).unwrap();
    app.create(&opts(), "urgent idea ..urgent ::work::alpha")
        .unwrap();
    app.create(&opts(), "see --1 and ///wiki/plans/roadmap.md")
        .unwrap();
    app.create(&opts(), "chores\n??water@2030-05-01 the garden")
        .unwrap();
    App::init(client, Some("work")).unwrap();
    let mut wopts = opts();
    wopts.profile = Some("work".into());
    app.create(&wopts, "work note at --default--1").unwrap();
    app.space_add("wiki", None).unwrap();
    app.observe("wiki").unwrap();
    (tmp, app)
}

fn refs(app: &App, expr: &str) -> Vec<String> {
    let reads = app.reads(&opts()).unwrap();
    let arbor = VaultArbor::build(&reads);
    match quarb::run(expr, &arbor).unwrap() {
        QueryResult::Nodes(ns) => ns.iter().map(|n| arbor.display(*n)).collect(),
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
    }
}

#[test]
fn tree_navigation_and_digit_names() {
    let (_tmp, app) = seeded();
    assert_eq!(
        refs(&app, "/default/*"),
        ["--default--1", "--default--2", "--default--3"]
    );
    // Jot IDs are all-digit hop names.
    assert_eq!(refs(&app, "/default/2"), ["--default--2"]);
    // The space subtree mirrors nested paths; globs work on names.
    assert_eq!(refs(&app, "//space//*.md"), ["///wiki/plans/roadmap.md"]);
    assert_eq!(refs(&app, "/space/wiki/plans/roadmap.md").len(), 1);
}

#[test]
fn traits_and_tags() {
    let (_tmp, app) = seeded();
    // Tags are traits — quarb's own concept, renamed from "tags".
    assert_eq!(refs(&app, "//*<urgent>"), ["--default--1"]);
    // Structural traits. <todo> marks both the jot holding an open
    // todo and the todo item node itself; conjunction disambiguates.
    assert_eq!(
        refs(&app, "//*<todo>"),
        ["--default--3", "--default--3??1"]
    );
    assert_eq!(refs(&app, "//*<jot && todo>"), ["--default--3"]);
    assert_eq!(refs(&app, "//*<todo && open>"), ["--default--3??1"]);
    assert_eq!(refs(&app, "//*<file>"), ["///wiki/plans/roadmap.md"]);
    // File tags participate too.
    assert_eq!(refs(&app, "//*<vision>"), ["///wiki/plans/roadmap.md"]);
    // Trait alternation.
    assert_eq!(refs(&app, "//*<urgent || todo>").len(), 3);
}

#[test]
fn predicates_projections_metadata() {
    let (_tmp, app) = seeded();
    // Category values carry `::`, which is projection syntax when
    // bare — quote hierarchical values.
    assert_eq!(
        refs(&app, "/default/*[::categories *= 'work::alpha']"),
        ["--default--1"]
    );
    assert_eq!(
        refs(&app, "/default/*[::summary *= 'chores']::summary"),
        ["chores"]
    );
    // Bare :: is the content.
    assert_eq!(
        refs(&app, "/default/1::"),
        ["urgent idea ..urgent ::work::alpha\n"]
    );
    // Adapter metadata, comparable as ISO strings and as ns ints.
    assert_eq!(refs(&app, "/default/2:::profile"), ["default"]);
    assert_eq!(
        refs(&app, "//*<jot>[:::created >= \"2020-01-01\"] @| count"),
        ["4"]
    );
    assert_eq!(refs(&app, "/work/1:::jot-id"), ["1"]);
}

#[test]
fn crosslinks_both_directions() {
    let (_tmp, app) = seeded();
    // Outbound: jot 2 references jot 1 (ref) and the file (file).
    assert_eq!(refs(&app, "/default/2->ref"), ["--default--1"]);
    assert_eq!(refs(&app, "/default/2->file"), ["///wiki/plans/roadmap.md"]);
    let mut all = refs(&app, "/default/2->*");
    all.sort();
    assert_eq!(all, ["--default--1", "///wiki/plans/roadmap.md"]);
    // Inbound: backlinks of jot 1 span profiles — and its tag node
    // reaches it over the `tagged` edge.
    let mut back = refs(&app, "/default/1<-*");
    back.sort();
    assert_eq!(back, ["--default--2", "--work--1", "..urgent"]);
    let mut back = refs(&app, "/default/1<-ref");
    back.sort();
    assert_eq!(back, ["--default--2", "--work--1"]);
    assert_eq!(refs(&app, "//roadmap.md<-file"), ["--default--2"]);
}

#[test]
fn attachments_in_the_tree() {
    let (tmp, app) = seeded();
    let f = tmp.path().join("paper.txt");
    std::fs::write(&f, "attached words").unwrap();
    app.attach(&opts(), &[f], &parse_ref("--1").unwrap())
        .unwrap();
    assert_eq!(
        refs(&app, "/default/1/paper.txt"),
        ["--default--1;;paper.txt"]
    );
    // Sizes are typed byte quantities.
    assert_eq!(refs(&app, "//*<attachment>:::size"), ["14_B"]);
}

/// Vault exercising the semantic surface: annotated references,
/// the category taxonomy, todo and bookmark item nodes, and an
/// inter-jot resource.
fn seeded_semantic() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    // 1: the cited source — two-level category, a tag.
    app.create(&opts(), "alpha ::work::alpha ..core").unwrap();
    // 2: annotated references (typed edges), a due todo, bookmarks.
    app.create(
        &opts(),
        "study --1::cites..key and --1::uses::weak\n\
         ??review@2030-05-01 the alpha notes\n\
         !!intro\nfirst part\n!!body\nsecond part\n",
    )
    .unwrap();
    let paper = tmp.path().join("paper.txt");
    std::fs::write(&paper, "attached words").unwrap();
    app.attach(&opts(), &[paper], &parse_ref("--1").unwrap())
        .unwrap();
    // 3: an inter-jot resource — an edge to 1's attachment.
    app.create(&opts(), "see --1;;paper.txt").unwrap();
    (tmp, app)
}

#[test]
fn typed_edges_from_annotations() {
    let (_tmp, app) = seeded_semantic();
    // The category's first segment is the edge label; both edges to
    // the same target survive as distinct typed edges.
    assert_eq!(refs(&app, "/default/2->cites"), ["--default--1"]);
    assert_eq!(refs(&app, "/default/2->uses"), ["--default--1"]);
    assert_eq!(refs(&app, "/default/1<-cites"), ["--default--2"]);
    // Edge annotations read through $-.
    assert_eq!(
        refs(&app, "/default/2->*[$-::tags *= 'key']"),
        ["--default--1"]
    );
    assert_eq!(
        refs(&app, "/default/2->*[$-::category = 'uses::weak']"),
        ["--default--1"]
    );
    // Parallel edges to one target converge to one capsa; the
    // written form is readable on whichever edge the thread took.
    assert_eq!(
        refs(&app, "/default/2->*[$-::as = 'default:1'] @| count"),
        ["1"]
    );
    assert_eq!(refs(&app, "/default/2->*[$- = 'uses']"), ["--default--1"]);
    // The inter-jot resource walks to the attachment node.
    assert_eq!(
        refs(&app, "/default/3->resource"),
        ["--default--1;;paper.txt"]
    );
    assert_eq!(refs(&app, "/default/1/paper.txt<-resource"), ["--default--3"]);
}

#[test]
fn category_taxonomy_and_tag_subtree() {
    let (_tmp, app) = seeded_semantic();
    // The category hierarchy is a tree; jots link ->cat to their
    // category leaves, so <-cat sweeps a level and //* a subtree.
    assert_eq!(refs(&app, "/cats/*"), ["::work"]);
    assert_eq!(refs(&app, "/cats/work/alpha<-cat"), ["--default--1"]);
    assert_eq!(refs(&app, "/cats/work//*<-cat"), ["--default--1"]);
    assert_eq!(refs(&app, "/default/1->cat"), ["::work::alpha"]);
    // The tag subtree enumerates and counts.
    assert_eq!(refs(&app, "/tags/core->tagged"), ["--default--1"]);
    assert_eq!(refs(&app, "/tags/core:::n-jots"), ["1"]);
}

#[test]
fn todo_and_bookmark_nodes() {
    let (_tmp, app) = seeded_semantic();
    // Todo items are child nodes named by their action word.
    assert_eq!(refs(&app, "//*<todo && open>::action"), ["review"]);
    assert_eq!(refs(&app, "/default/2/review::importance"), ["0"]);
    // ::due mints a typed instant (a date-only due displays bare).
    assert_eq!(refs(&app, "/default/2/review::due"), ["2030-05-01"]);
    assert_eq!(refs(&app, "//*<todo>[::due < '2031-01-01']"), ["--default--2??1"]);
    // Bookmarks are child nodes; bare :: is the fragment — the
    // same slice `cuj show --2#intro` prints.
    assert_eq!(refs(&app, "/default/2/intro::"), ["!!intro\nfirst part\n"]);
    let mut marks = refs(&app, "//*<bookmark>");
    marks.sort();
    assert_eq!(marks, ["--default--2#body", "--default--2#intro"]);
}

#[test]
fn typed_metadata_and_orphans() {
    let (_tmp, app) = seeded_semantic();
    // created/modified are typed instants: unquoted temporal
    // predicates work.
    assert_eq!(
        refs(&app, "//*<jot>[:::created > '2020-01-01'] @| count"),
        ["3"]
    );
    // Link counts exclude taxonomy edges.
    assert_eq!(refs(&app, "/default/1:::n-backlinks"), ["2"]);
    assert_eq!(refs(&app, "/default/2:::n-links"), ["2"]);
    // 1 is referenced; 2 and 3 are orphans.
    let mut orphans = refs(&app, "//*<jot && orphan>");
    orphans.sort();
    assert_eq!(orphans, ["--default--2", "--default--3"]);
}

#[test]
fn reparse_restores_annotations_and_is_idempotent() {
    let (_tmp, app) = seeded_semantic();
    // Regress jot 2's index to the pre-annotation shape (what an
    // older cuj wrote): annotations gone from the arbor.
    {
        let reads = app.reads(&opts()).unwrap();
        let entry = reads.resolve_live("default", 2).unwrap().0;
        let mut old = reads.parsed[&entry].clone();
        for r in &mut old.references {
            r.annot = Default::default();
        }
        let fact = cuj::facts::to_fact(cuj::facts::K_JOT_INDEX, entry, &old).unwrap();
        app.commit_pinned(&reads, &[fact]).unwrap();
    }
    assert_eq!(refs(&app, "/default/2->cites").len(), 0);
    assert_eq!(refs(&app, "/default/2->ref"), ["--default--1"]);
    // reparse re-derives from content; a second run is a no-op.
    assert!(app.reparse().unwrap() >= 1);
    assert_eq!(refs(&app, "/default/2->cites"), ["--default--1"]);
    assert_eq!(app.reparse().unwrap(), 0);
}

#[test]
fn reparse_heals_unresolved_references() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.create(&opts(), "see --2 later").unwrap();
    app.create(&opts(), "the target").unwrap();
    // --2 was unresolved when jot 1 was written.
    assert_eq!(refs(&app, "/default/1->ref").len(), 0);
    assert!(app.reparse().unwrap() >= 1);
    assert_eq!(refs(&app, "/default/1->ref"), ["--default--2"]);
}

#[test]
fn parallel_edges_traverse_and_props_pick_first_consistently() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.create(&opts(), "the target").unwrap();
    app.create(&opts(), "see --1::cites::weak and --1::cites::strong..key")
        .unwrap();
    // Both parallel cites edges traverse and both count...
    assert_eq!(refs(&app, "/default/2:::n-links"), ["2"]);
    assert!(!refs(&app, "/default/2->cites").is_empty());
    // ...but the (source, edge label, target) property slot is
    // single-valued: $- answers for the FIRST edge in parse
    // order, consistently (quarb adapter contract).
    let cat = refs(&app, "/default/2->cites[$-::category = 'cites::weak']");
    assert_eq!(cat, ["--default--1"]);
    // The second edge's own annotation is unreachable through the
    // collapsed slot — the documented limit.
    assert!(refs(&app, "/default/2->cites[$-::tags *= 'key']").is_empty());
}

#[test]
fn cats_profile_shadows_taxonomy_root() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    App::init(client.clone(), Some("cats")).unwrap();
    let app = App::open(client).unwrap();
    app.create(&opts(), "a categorized note ::work").unwrap();
    let mut copts = opts();
    copts.profile = Some("cats".into());
    app.create(&copts, "a cat profile note").unwrap();
    // The profile shadows the taxonomy root's NAME (SPEC Appendix
    // B): /cats resolves to one node, and /cats/* is the profile's
    // jots only — no ::work leaking into the sweep.
    assert_eq!(refs(&app, "/cats"), ["cats"]);
    assert_eq!(refs(&app, "/cats/*"), ["--cats--1"]);
    // The taxonomy stays structurally reachable: the jot's cat
    // edge still answers.
    assert_eq!(refs(&app, "/default/1->cat"), ["::work"]);
}

#[test]
fn reparse_keeps_references_to_moved_jots() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    App::init(client.clone(), Some("work")).unwrap();
    let app = App::open(client).unwrap();
    app.create(&opts(), "the target").unwrap();
    app.create(&opts(), "see --1 for details").unwrap();
    // The edge is durable across mv (SPEC §3.4)...
    let (old, new) = app.mv(&opts(), "default", "work", 1).unwrap();
    assert_eq!((old.id, new.profile.as_str()), (1, "work"));
    assert_eq!(refs(&app, "/default/2->ref"), ["--work--1"]);
    // ...and reparse must not sever it: the written form --1 no
    // longer resolves at head, but its historical holder does.
    app.reparse().unwrap();
    assert_eq!(refs(&app, "/default/2->ref"), ["--work--1"]);
    assert_eq!(app.reparse().unwrap(), 0);
}

#[test]
fn union_pipes_and_json_ordering() {
    let (_tmp, app) = seeded();
    let mut u = refs(&app, "//roadmap.md || /default/1");
    u.sort();
    assert_eq!(u, ["--default--1", "///wiki/plans/roadmap.md"]);
    assert_eq!(refs(&app, "/default/* @| count"), ["3"]);
    // Property lists render comma-joined through Value's Display.
    assert_eq!(refs(&app, "/default/1::tags"), ["urgent"]);
}

#[test]
fn edge_specifier_properties_and_typed_file_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = client_for(&tmp);
    let wiki = tmp.path().join("wiki");
    std::fs::create_dir_all(&wiki).unwrap();
    std::fs::write(wiki.join("plan.md"), "the plan\n").unwrap();
    client.spaces.insert("wiki".into(), wiki);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.space_add("wiki", None).unwrap();
    app.observe("wiki").unwrap();
    app.create(&opts(), "target !!intro body !!end tail").unwrap();
    // The specifier precedes annotations in the reference grammar.
    app.create(
        &opts(),
        "notes --1#intro::cites and --1##intro,end::sees \
         plus ///wiki/plan.md::docs..key",
    )
    .unwrap();
    // Specifier annotations answer through $- on the typed edge.
    assert_eq!(
        refs(&app, "/default/2->cites[$-::bookmark = 'intro']"),
        ["--default--1"]
    );
    assert_eq!(
        refs(&app, "/default/2->sees[$-::bookmark-range = 'intro,end']"),
        ["--default--1"]
    );
    // Annotated cross-file references walk as typed file edges
    // with their annotations readable.
    assert_eq!(
        refs(&app, "/default/2->docs[$-::tags *= 'key']"),
        ["///wiki/plan.md"]
    );
}

#[test]
fn todo_items_done_trait_wordless_names_and_file_items() {
    let tmp = tempfile::tempdir().unwrap();
    let mut client = client_for(&tmp);
    let wiki = tmp.path().join("wiki");
    std::fs::create_dir_all(&wiki).unwrap();
    std::fs::write(wiki.join("todo.md"), "??fix the intro\n!!top anchor\n").unwrap();
    client.spaces.insert("wiki".into(), wiki);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.space_add("wiki", None).unwrap();
    app.observe("wiki").unwrap();
    app.create(&opts(), "chores ::work::alpha\n??water the garden")
        .unwrap();

    // Overlay todos carry no action word (the Obsidian import
    // shape); their item nodes are named by 1-based index.
    let reads = app.reads(&opts()).unwrap();
    let (entry, _) = reads.resolve_live("default", 1).unwrap();
    let overlay = cuj::facts::AppliedOverlay {
        todos: vec![cuj::facts::TodoBody {
            raw: "- [ ] wash the car".into(),
            action: None,
            description: "wash the car".into(),
            importance: 0,
            due: None,
            done: false,
            completed: None,
        }],
        ..Default::default()
    };
    let fact = cuj::facts::to_fact(cuj::facts::K_CUJ_APPLIED, entry, &overlay).unwrap();
    app.commit_pinned(&reads, &[fact]).unwrap();
    let raw = refs(&app, "/default/1/2::raw");
    assert_eq!(raw, ["- [ ] wash the car"], "{raw:?}");

    // Completing a todo flips its item node to <done>.
    let r = parse_ref("--1").unwrap();
    app.todo_done(&opts(), &r, "1").unwrap();
    assert_eq!(refs(&app, "/default/1/*<todo && done>").len(), 1);

    // Cat nodes count exact-level jots via :::n-jots.
    assert_eq!(refs(&app, "/cats/work/alpha:::n-jots"), ["1"]);
    assert_eq!(refs(&app, "/cats/work:::n-jots"), ["0"]);

    // File-jots: unreferenced observed files are <orphan>, and
    // their todo/bookmark item nodes exist with content answers.
    let orphans = refs(&app, "/space/wiki/*<orphan>");
    assert_eq!(orphans, ["///wiki/todo.md"], "{orphans:?}");
    assert_eq!(refs(&app, "//todo.md/fix::raw"), ["??fix the intro"]);
    assert!(!refs(&app, "//todo.md/top::").is_empty());
}

#[test]
fn reparse_rederives_observed_files() {
    let (_tmp, app) = seeded();
    let reads = app.reads(&opts()).unwrap();
    let entry = reads.files[&("wiki".to_string(), "plans/roadmap.md".to_string())];
    // Simulate a stale/foreign jot/index on the file entry.
    let empty = cuj::facts::ParsedIndex {
        jots_version: "jots 0.0.0".into(),
        ..Default::default()
    };
    let fact = cuj::facts::to_fact(cuj::facts::K_JOT_INDEX, entry, &empty).unwrap();
    app.commit_pinned(&reads, &[fact]).unwrap();
    let reads = app.reads(&opts()).unwrap();
    assert!(!reads.effective(entry).tags.contains("vision"));
    // reparse re-derives observed text files too (SPEC 10.31).
    assert!(app.reparse().unwrap() >= 1);
    let reads = app.reads(&opts()).unwrap();
    assert!(reads.effective(entry).tags.contains("vision"));
    assert_eq!(app.reparse().unwrap(), 0);
}
