//! End-to-end library tests: vault lifecycle, jot CRUD, overlay
//! semantics, entry-resolved references, counter allocation, and
//! the retrieval bindings.

use std::collections::BTreeMap;

use cuj::app::{App, Opts, parse_ref};
use cuj::client::ClientConfig;
use cuj::error::Error;
use cuj::ext;
use cuj::reads::{Reads, Resolution};

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

#[test]
fn lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client.clone()).unwrap();

    // create
    let a = app
        .create(&opts(), "capture the ..urgent idea ::work::alpha")
        .unwrap();
    assert_eq!((a.profile.as_str(), a.id), ("default", 1));
    let b = app
        .create(&opts(), "quieter thought about gardens --1")
        .unwrap();
    assert_eq!(b.id, 2);

    let reads = app.reads(&opts()).unwrap();
    let (e1, _) = reads.resolve_live("default", 1).unwrap();
    let (e2, _) = reads.resolve_live("default", 2).unwrap();
    let eff1 = reads.effective(e1);
    assert!(eff1.tags.contains("urgent"));
    assert!(eff1.categories.contains("work::alpha"));
    // --1 in jot 2's content resolved to entry 1 at write time.
    let eff2 = reads.effective(e2);
    assert!(eff2.edges.contains_key(&e1.hyphenated().to_string()));

    // label (overlay, content untouched)
    let r2 = parse_ref("--2").unwrap();
    app.label(&opts(), std::slice::from_ref(&r2), &["..garden".into()])
        .unwrap();
    let reads = app.reads(&opts()).unwrap();
    assert!(reads.effective(e2).tags.contains("garden"));
    let content = reads.bytes(e2).unwrap();
    assert!(!String::from_utf8_lossy(&content).contains("..garden"));

    // link + idempotence
    let r1 = parse_ref("--1").unwrap();
    assert!(app.link(&opts(), &r1, &r2).unwrap());
    assert!(!app.link(&opts(), &r1, &r2).unwrap());
    // A content-parsed edge counts too (SPEC §10.13): jot 2's
    // content already references --1, so linking commits nothing.
    assert!(!app.link(&opts(), &r2, &r1).unwrap());

    // taxopsis find via the generic document extractor
    let chain = ext::taxopsis_fresh(app.root(), &app.vault, false, false).unwrap();
    let expr =
        taxopsis::parse_filter(r#"jot/index/tags = "urgent" OR cuj/applied/tags = "urgent""#)
            .unwrap();
    let sref = chain.current_hash().unwrap().to_hex();
    let page = chain.query(&expr, &sref, None, None, None).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].entry, e1);

    // backlinks are a taxopsis query on references/entry
    let expr = taxopsis::parse_filter(&format!(
        r#"jot/index/references/entry = "{0}" OR cuj/applied/references/entry = "{0}""#,
        e1.hyphenated()
    ))
    .unwrap();
    let page = chain.query(&expr, &sref, None, None, None).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].entry, e2);

    // logopsis search
    let lchain = ext::logopsis_fresh(app.root(), &app.vault, false, false).unwrap();
    let q = logopsis::parse_query("gardens").unwrap();
    let hits = lchain.query(&q, 10, "current").unwrap();
    assert!(hits.iter().any(|h| h.entry == e2));

    // semopsis (feathash embedder — deterministic)
    let schain = ext::semopsis_fresh(app.root(), &app.vault, false, false).unwrap();
    let shits = schain
        .query(b"gardens and quiet", 2, None, "current")
        .unwrap();
    assert!(!shits.is_empty());

    // rm: pure path retraction; id retired via the counter
    app.rm(&opts(), &r1).unwrap();
    let reads = app.reads(&opts()).unwrap();
    assert!(matches!(
        reads.resolve("default", 1).unwrap(),
        Resolution::Deleted(_)
    ));
    assert_eq!(reads.next_id("default").unwrap(), 3);
    // deleted jots drop out of the retrieval index by construction
    let chain = ext::taxopsis_fresh(app.root(), &app.vault, false, false).unwrap();
    let expr = taxopsis::parse_filter(r#"jot/index/tags = "urgent""#).unwrap();
    let sref = chain.current_hash().unwrap().to_hex();
    assert!(
        chain
            .query(&expr, &sref, None, None, None)
            .unwrap()
            .rows
            .is_empty()
    );

    // second profile + mv: entry edge survives, courtesy hint on old id
    App::init(client.clone(), Some("work")).unwrap();
    let (old, new) = app.mv(&opts(), "default", "work", 2).unwrap();
    assert_eq!((old.profile.as_str(), old.id), ("default", 2));
    assert_eq!((new.profile.as_str(), new.id), ("work", 1));
    let reads = app.reads(&opts()).unwrap();
    match reads.resolve("default", 2).unwrap() {
        Resolution::Moved(e, cur) => {
            assert_eq!(e, e2);
            assert_eq!((cur.profile.as_str(), cur.id), ("work", 1));
        }
        _ => panic!("expected Moved courtesy answer"),
    }
    // moved jot keeps its overlay tag and its entry-resolved edge
    let eff = reads.effective(e2);
    assert!(eff.tags.contains("garden"));
    assert!(eff.edges.contains_key(&e1.hyphenated().to_string()));
}

#[test]
fn todo_nested_constructs_and_bookmark_ranges() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.create(&opts(), "the target !!a middle !!b end").unwrap();
    app.create(&opts(), "second target").unwrap();
    app.create(&opts(), "digest --1##a,b\n??follow up ..urgent --2")
        .unwrap();

    let reads = app.reads(&opts()).unwrap();
    let (e1, _) = reads.resolve_live("default", 1).unwrap();
    let (e2, _) = reads.resolve_live("default", 2).unwrap();
    let (e3, _) = reads.resolve_live("default", 3).unwrap();
    let eff = reads.effective(e3);
    // Constructs nested in a todo contribute to the jot-level
    // arrays (SPEC §5.1): the tag is findable, the edge resolved.
    assert!(eff.tags.contains("urgent"));
    assert!(eff.edges.contains_key(&e2.hyphenated().to_string()));
    // A ##start,end specifier survives into the edge annotation.
    let infos = &eff.edges[&e1.hyphenated().to_string()];
    assert!(
        infos.iter().any(|i| i
            .annot
            .bookmark_range
            .as_ref()
            .is_some_and(|r| r.start.as_deref() == Some("a")
                && r.end.as_deref() == Some("b"))),
        "{infos:?}"
    );
}

#[test]
fn todos_and_frozen() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client.clone()).unwrap();

    match app.todo(&opts(), &[], "??review the draft").unwrap() {
        cuj::app::either::TodoOutcome::Created(i) => assert_eq!(i.id, 1),
        _ => panic!("expected creation"),
    }
    let reads = app.reads(&opts()).unwrap();
    let (e1, _) = reads.resolve_live("default", 1).unwrap();
    let eff = reads.effective(e1);
    assert_eq!(eff.todos.len(), 1);
    assert!(!eff.todos[0].done);

    let r = parse_ref("--1").unwrap();
    let msg = app.todo_done(&opts(), &r, "1").unwrap();
    assert!(msg.contains("content"), "{msg}");
    let reads = app.reads(&opts()).unwrap();
    assert!(reads.effective(e1).todos.iter().all(|t| t.done));

    // freeze the profile: edits refuse, labels still work
    app.update_profile("default", |p| p.jots_frozen = true)
        .unwrap();
    app.create(&opts(), "x").unwrap();
    let r2 = parse_ref("--2").unwrap();
    let err = app.edit(&opts(), &r2).unwrap_err();
    assert!(matches!(err, Error::Frozen(_)), "{err}");
    app.label(&opts(), std::slice::from_ref(&r2), &["..frozenok".into()])
        .unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e2, _) = reads.resolve_live("default", 2).unwrap();
    assert!(reads.effective(e2).tags.contains("frozenok"));
}

#[test]
fn constraints() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client.clone()).unwrap();
    app.update_profile("default", |p| {
        p.exclusive = vec!["::".into()];
        p.required = vec!["..".into()];
    })
    .unwrap();

    let err = app.create(&opts(), "no tags here").unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)), "{err}");
    let mut forced = opts();
    forced.force = true;
    app.create(&forced, "no tags here").unwrap();

    let err = app
        .create(&forced, "..t ::one ::two double categories")
        .unwrap_err();
    assert!(matches!(err, Error::ConstraintViolation(_)), "{err}");

    app.create(&opts(), "..tagged ::one fine").unwrap();
}

#[test]
fn export_reads_and_extractor_pinning() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client.clone()).unwrap();
    app.create(&opts(), "first ..a").unwrap();
    app.create(&opts(), "second ..b").unwrap();

    let out = tmp.path().join("export");
    cuj::read::export(&app, &opts(), &out).unwrap();
    assert!(out.join("default/1.md").is_file());
    assert_eq!(
        std::fs::read_to_string(out.join("default/1.md")).unwrap(),
        "first ..a\n"
    );

    app.reindex().unwrap();
    let chain = ext::open_taxopsis(app.root()).unwrap();
    assert_eq!(
        chain.current_state().unwrap().metatheca,
        app.vault.current_state_hash().unwrap()
    );
    // The pinned extractor is the generic document family.
    assert_eq!(
        chain.current_state().unwrap().extractor.to_string(),
        "metatheca-docs:1"
    );

    // Historical reads see the pre-second-create state.
    let s1 = app.vault.resolve("~1").unwrap();
    let hist = Reads::load(&app.vault, Some(&s1)).unwrap();
    assert_eq!(hist.list(None).len(), 1);
}

// ── 0.4 conformance additions ─────────────────────────────────────

/// SPEC §3.2: a batch built against a stale view must surface
/// Conflict from the pinned commit, and the op-level retry (a fresh
/// App call) must then succeed with a distinct ID.
#[test]
fn cas_pin_and_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app1 = App::open(client.clone()).unwrap();
    let app2 = App::open(client.clone()).unwrap();

    // Stale pin surfaces Conflict: load a view, let a second writer
    // land a commit, then try to commit against the old pin.
    let reads = app1.reads(&opts()).unwrap();
    app2.create(&opts(), "concurrent writer").unwrap();
    let err = app1.commit_pinned(&reads, &[]).unwrap_err();
    assert!(matches!(err, Error::Conflict));

    // The retry driver rereads: the same logical op through the
    // public surface allocates the next free ID, no collision.
    let b = app1.create(&opts(), "after the race").unwrap();
    assert_eq!(b.id, 2);
    let reads = app1.reads(&opts()).unwrap();
    assert_eq!(reads.idents.len(), reads.id_map.len());
}

/// The retry driver retries Conflict up to the bound, then surfaces
/// it; non-conflict errors pass through untouched.
#[test]
fn cas_retry_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    let attempts = std::cell::Cell::new(0usize);
    let out: Result<(), Error> = app.with_cas_retry(&opts(), |_reads| {
        attempts.set(attempts.get() + 1);
        Err(Error::Conflict)
    });
    assert!(matches!(out, Err(Error::Conflict)));
    assert_eq!(attempts.get(), cuj::app::CAS_RETRIES);

    let attempts = std::cell::Cell::new(0usize);
    let out = app.with_cas_retry(&opts(), |_reads| {
        attempts.set(attempts.get() + 1);
        if attempts.get() < 3 {
            Err(Error::Conflict)
        } else {
            Ok(attempts.get())
        }
    });
    assert_eq!(out.unwrap(), 3);
}

/// SPEC §10.15: todo done's ordinal is the one show displays — over
/// all todos, done ones included. On a frozen profile the overlay
/// marks a todo done IN PLACE, so open-only indexing would drift
/// from show's numbering.
#[test]
fn todo_done_ordinal_matches_show() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    app.create(&opts(), "plan:\n??one first\n??two second\n??three third")
        .unwrap();
    app.update_profile("default", |p| p.jots_frozen = true)
        .unwrap();
    let r = parse_ref("--1").unwrap();
    // Complete the first; frozen, so the overlay records it done in
    // place and show still lists one=1, two=2, three=3.
    app.todo_done(&opts(), &r, "1").unwrap();
    // Ordinal 2 must mean the SECOND LISTED todo ("two"), not the
    // second open one ("three").
    app.todo_done(&opts(), &r, "2").unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", 1).unwrap();
    let eff = reads.effective(e);
    let done: Vec<&str> = eff
        .todos
        .iter()
        .filter(|t| t.done)
        .map(|t| t.action.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(done, ["one", "two"]);

    // Completing an already-done ordinal is a no-op with a message.
    let msg = app.todo_done(&opts(), &r, "1").unwrap();
    assert!(msg.contains("already done"), "{msg}");
}

/// SPEC §11.2 / §10.4: jots carry a modification time distinct from
/// creation once edited.
#[test]
fn modified_at_ns_tracks_content() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    app.create(&opts(), "original ??touch it").unwrap();
    let reads = app.reads(&opts()).unwrap();
    let (e, _) = reads.resolve_live("default", 1).unwrap();
    let m0 = *reads.modified.get(&e).unwrap();

    let json = cuj::read::jot_json(&reads, e);
    assert_eq!(json["modified_at_ns"].as_i64(), Some(m0));

    // A content rewrite (todo done) advances the modification time.
    let r = parse_ref("--1").unwrap();
    app.todo_done(&opts(), &r, "1").unwrap();
    let reads = app.reads(&opts()).unwrap();
    let m1 = *reads.modified.get(&e).unwrap();
    assert!(m1 > m0);
}

/// The library surface a web backend builds on: data-returning
/// queries (no stdout), pagination, and non-interactive content
/// updates.
#[test]
fn library_surface_for_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();

    for i in 1..=5 {
        app.create(&opts(), &format!("note number {i} ..bulk"))
            .unwrap();
    }

    // list pagination.
    let page = cuj::queries::list(&app, &opts(), 2, Some(2)).unwrap();
    assert_eq!(
        page.iter().map(|j| j.id).collect::<Vec<_>>(),
        [3, 4],
        "offset 2, limit 2"
    );
    assert!(page[0].summary.contains("note number 3"));

    // find with a native limit + resumption cursor.
    let f = cuj::queries::FindArgs {
        tags: vec!["bulk".into()],
        categories: vec![],
        since: None,
        until: None,
        todo: false,
        filter: None,
    };
    let ids = |page: &cuj::queries::FindPage| -> Vec<i64> {
        page.hits
            .iter()
            .map(|h| match h {
                cuj::queries::HitTarget::Jot(j) => j.id,
                cuj::queries::HitTarget::File(_) => panic!("no files here"),
            })
            .collect()
    };
    let p1 = cuj::queries::find(&app, &opts(), &f, Some(3), None).unwrap();
    assert_eq!(ids(&p1), [1, 2, 3]);
    let cursor = p1.cursor.expect("more results remain");
    let p2 = cuj::queries::find(&app, &opts(), &f, Some(3), Some(&cursor)).unwrap();
    assert_eq!(ids(&p2), [4, 5]);
    assert!(p2.cursor.is_none());

    // search returns structured hits.
    let hits = cuj::queries::search(&app, &opts(), "number", 2, 0, false).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits[0].snippet.contains("number"));

    // Non-interactive content update: constraint-checked, retried,
    // and a no-op on identical bytes.
    let r = parse_ref("--1").unwrap();
    let changed = app
        .update_content(&opts(), &r, "rewritten from the backend ..bulk\n")
        .unwrap();
    assert!(changed);
    let changed = app
        .update_content(&opts(), &r, "rewritten from the backend ..bulk\n")
        .unwrap();
    assert!(!changed, "identical content is a no-op");
    match cuj::queries::show(&app, &opts(), &r, false).unwrap() {
        cuj::queries::ShowOutcome::Shown { jot, content } => {
            assert!(content.starts_with("rewritten"));
            assert!(jot.modified_at_ns.unwrap() > jot.created_at_ns.unwrap());
        }
        _ => panic!("expected Shown"),
    }

    // update_content respects frozen profiles.
    app.update_profile("default", |p| p.jots_frozen = true)
        .unwrap();
    let err = app
        .update_content(&opts(), &r, "should not land\n")
        .unwrap_err();
    assert!(matches!(err, Error::Frozen(_)));
}

#[test]
fn fsck_detects_identity_less_live_path() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.create(&opts(), "healthy jot").unwrap();
    assert!(cuj::queries::fsck(&app).unwrap().clean());

    // A live jot path whose entry has no cuj/id identity — the
    // invariant graph degrades around and fsck exists to report.
    let reads = app.reads(&opts()).unwrap();
    let ghost = app.vault.new_entry_id();
    app.commit_pinned(
        &reads,
        &[metatheca::Fact::path(ghost, "default/jot/99.md", true)],
    )
    .unwrap();
    let outcome = cuj::queries::fsck(&app).unwrap();
    assert!(!outcome.clean(), "fsck must flag the ghost path");
    assert!(!outcome.problems.is_empty(), "{outcome:?}");
}

#[test]
fn missing_content_blob_is_substrate_not_notfound() {
    let tmp = tempfile::tempdir().unwrap();
    let client = client_for(&tmp);
    App::init(client.clone(), None).unwrap();
    let app = App::open(client.clone()).unwrap();
    app.create(&opts(), "soon to be corrupted").unwrap();

    let reads = app.reads(&opts()).unwrap();
    let (e1, _) = reads.resolve_live("default", 1).unwrap();
    let hex = reads.blobs[&e1].to_hex();
    // Corrupt the vault: remove the content blob's depositum.
    std::fs::remove_dir_all(client.vault.join("cella/deposita").join(&hex)).unwrap();
    // A blob referenced by the vault's own facts going missing is
    // corruption — Substrate (exit 1), never a user-facing
    // NotFound (exit 3) that reads as "no such jot".
    let err = reads.bytes(e1).unwrap_err();
    assert!(matches!(err, Error::Substrate(_)), "{err}");
    assert!(err.to_string().contains("missing from the cella"), "{err}");
}
