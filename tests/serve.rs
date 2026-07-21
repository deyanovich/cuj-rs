//! Serve-protocol round-trip (SPEC appendix): `cuj quarb-serve`
//! spoken to by the quarb-serve client must answer exactly like
//! the in-process arbor — typed values and edge properties
//! included.

use std::collections::BTreeMap;

use cuj::app::{App, Opts, parse_ref};
use cuj::arbor::VaultArbor;
use cuj::client::ClientConfig;
use quarb::QueryResult;
use quarb_serve::ServeAdapter;

/// Values of a query against an adapter (nodes are out of scope
/// here — locator rendering is the CLI's, not the protocol's).
fn values(adapter: &impl quarb::AstAdapter, expr: &str) -> Vec<String> {
    match quarb::run(expr, adapter).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("#{}", n.0)).collect(),
    }
}

#[test]
fn served_vault_equals_in_process() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.toml");
    let vault = tmp.path().join("vault");
    std::fs::write(&config, format!("vault = \"{}\"\n", vault.display())).unwrap();
    let client = ClientConfig {
        vault,
        active_profile: "default".into(),
        editor: None,
        extract: BTreeMap::new(),
        spaces: BTreeMap::new(),
        path: config.clone(),
    };
    App::init(client.clone(), None).unwrap();
    let app = App::open(client).unwrap();
    app.create(&Opts::default(), "alpha ::work::alpha ..core")
        .unwrap();
    app.create(
        &Opts::default(),
        "study --1::cites..key\n??review@2030-05-01 the notes\n",
    )
    .unwrap();
    let paper = tmp.path().join("paper.txt");
    std::fs::write(&paper, "attached words").unwrap();
    app.attach(&Opts::default(), &[paper], &parse_ref("--1").unwrap())
        .unwrap();

    let served = ServeAdapter::spawn(&format!(
        "CUJ_CONFIG='{}' '{}' quarb-serve",
        config.display(),
        env!("CARGO_BIN_EXE_cuj")
    ))
    .unwrap();
    assert_eq!(served.name, "cuj");

    let reads = app.reads(&Opts::default()).unwrap();
    let arbor = VaultArbor::build(&reads);
    for expr in [
        // Typed instants survive the wire: the temporal predicate
        // filters server-side values client-side, and the display
        // is the instant's, not a stringly echo.
        "//*<jot>[::;created > '2020-01-01'] @| count",
        "/default/2/review::due",
        // Byte quantities.
        "//*<attachment>::;size",
        // Edge properties over the link_property op.
        "/default/2->*[$-::tags *= 'key']::;id",
        "/default/2->*[$- = 'cites']::;id",
        // Plain navigation parity.
        "/default/* @| count",
        "/default/1::tags",
    ] {
        assert_eq!(
            values(&served, expr),
            values(&arbor, expr),
            "served != in-process for {expr:?}"
        );
    }
    // And the typed values are what they should be, not just equal.
    assert_eq!(values(&served, "/default/2/review::due"), ["2030-05-01"]);
    assert_eq!(values(&served, "//*<attachment>::;size"), ["14 B"]);
}
