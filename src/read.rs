//! CLI rendering of the read operations (SPEC §10, §11.2): thin
//! stdout renderers over the data-returning `queries` module.

use std::path::Path;

use metatheca::{Uuid, format_iso8601_secs};
use serde_json::{Value, json};

use crate::app::{App, Opts, RefArg};
use crate::error::{Error, Result};
use crate::queries::{self, JotView, ShowOutcome};
use crate::reads::Reads;

pub use crate::queries::{FindArgs, SimilarTarget};

// ── rendering helpers ─────────────────────────────────────────────

/// The SPEC §11.2 jot object as a JSON value.
pub fn jot_json(reads: &Reads<'_>, entry: Uuid) -> Value {
    serde_json::to_value(queries::jot_view(reads, entry)).unwrap_or(Value::Null)
}

fn json_out<T: serde::Serialize>(v: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::to_value(v).unwrap_or(Value::Null)).unwrap()
    );
}

fn short_ref_view(jot: &JotView, scope: Option<&str>) -> String {
    match scope {
        Some(p) if p == jot.profile => format!("--{}", jot.id),
        _ => format!("--{}--{}", jot.profile, jot.id),
    }
}

// ── show (SPEC §10.4) ─────────────────────────────────────────────

pub fn show(app: &App, opts: &Opts, r: &RefArg, expand: bool) -> Result<()> {
    match queries::show(app, opts, r, expand)? {
        // Deleted and moved-away identities are error classes
        // (SPEC §13, §5.3): non-zero exit, the standard --json
        // error envelope, courtesy detail in the message.
        ShowOutcome::Deleted { profile, id } => Err(Error::Deleted(format!(
            "--{profile}--{id} (no live path)"
        ))),
        ShowOutcome::Moved {
            profile,
            id,
            current,
        } => Err(Error::NotFound(format!(
            "--{profile}--{id} (moved to --{}--{})",
            current.profile, current.id
        ))),
        ShowOutcome::Shown { jot, content } => {
            if opts.json {
                let mut v = serde_json::to_value(&jot).unwrap_or(Value::Null);
                v["content"] = Value::String(content);
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
                return Ok(());
            }
            println!("--{}--{} ({})", jot.profile, jot.id, jot.ext);
            if let Some(c) = jot.created_at_ns {
                println!("created  {}", format_iso8601_secs(c));
            }
            if let Some(m) = jot.modified_at_ns {
                println!("modified {}", format_iso8601_secs(m));
            }
            let mut labels: Vec<String> = jot.tags.iter().map(|t| format!("..{t}")).collect();
            labels.extend(jot.categories.iter().map(|c| format!("::{c}")));
            if !labels.is_empty() {
                println!("labels   {}", labels.join(" "));
            }
            if !jot.references.is_empty() {
                let refs: Vec<String> = jot
                    .references
                    .iter()
                    .map(|rv| match (&rv.profile, rv.id) {
                        // Render the target's *current* identity.
                        (Some(p), Some(id)) if *p == jot.profile => format!("--{id}"),
                        (Some(p), Some(id)) => format!("--{p}--{id}"),
                        _ => format!("--{}?", rv.written_as),
                    })
                    .collect();
                println!("refs     {}", refs.join(" "));
            }
            if !jot.attachments.is_empty() {
                println!("attach   {}", jot.attachments.join(" "));
            }
            if !jot.todos.is_empty() {
                println!("todos:");
                for (i, t) in jot.todos.iter().enumerate() {
                    println!(
                        "  {}. [{}] {}",
                        i + 1,
                        if t.done { "x" } else { " " },
                        t.raw
                    );
                }
            }
            println!();
            print!("{content}");
            if !content.ends_with('\n') {
                println!();
            }
            Ok(())
        }
    }
}

// ── list (SPEC §10.5) ─────────────────────────────────────────────

pub fn list(app: &App, opts: &Opts) -> Result<()> {
    let jots = queries::list(app, opts, 0, opts.k)?;
    if opts.json {
        json_out(&jots);
        return Ok(());
    }
    let scope = opts.read_scope(&app.client);
    for jot in &jots {
        let date = jot
            .created_at_ns
            .map(|ns| format_iso8601_secs(ns)[..10].to_string())
            .unwrap_or_default();
        println!(
            "{:>12}  {}  {}",
            short_ref_view(jot, scope),
            date,
            jot.summary
        );
    }
    Ok(())
}

// ── find (SPEC §10.6) ─────────────────────────────────────────────

pub fn find(app: &App, opts: &Opts, args: &FindArgs) -> Result<()> {
    print_page(app, opts, queries::find(app, opts, args, opts.k, None)?)
}

pub fn saved_query(app: &App, opts: &Opts, name: &str) -> Result<()> {
    print_page(
        app,
        opts,
        queries::run_saved_query(app, opts, name, opts.k, None)?,
    )
}

fn print_page(app: &App, opts: &Opts, page: queries::FindPage) -> Result<()> {
    if opts.json {
        json_out(&page.hits);
        return Ok(());
    }
    let scope = opts.read_scope(&app.client);
    for hit in &page.hits {
        println!("{:>12}  {}", hit.display_ref(scope), hit.summary());
    }
    Ok(())
}

// ── grep (SPEC §10.9) ─────────────────────────────────────────────

pub fn grep(app: &App, opts: &Opts, pattern: &str, filters: &FindArgs) -> Result<()> {
    let hits = queries::grep(app, opts, pattern, filters)?;
    if opts.json {
        json_out(&hits);
        return Ok(());
    }
    let scope = opts.read_scope(&app.client);
    for h in &hits {
        let r = match &h.source {
            queries::GrepSource::Jot { profile, id } => match scope {
                Some(p) if p == profile => format!("--{id}"),
                _ => format!("--{profile}--{id}"),
            },
            queries::GrepSource::File { space, path } => format!("///{space}/{path}"),
        };
        let label = h
            .attachment
            .as_deref()
            .map(|a| format!(";;{a}"))
            .unwrap_or_default();
        println!("{r}{label}:{}: {}", h.line, h.text);
    }
    Ok(())
}

// ── search / similar (SPEC §10.10, 10.11) ─────────────────────────

pub fn search(app: &App, opts: &Opts, query: &str, lexical: bool) -> Result<()> {
    let hits = queries::search(app, opts, query, opts.k.unwrap_or(10), 0, lexical)?;
    print_hits(app, opts, &hits)
}

pub fn similar(app: &App, opts: &Opts, target: SimilarTarget<'_>) -> Result<()> {
    let hits = queries::similar(app, opts, target, opts.k.unwrap_or(10), 0)?;
    print_hits(app, opts, &hits)
}

fn print_hits(app: &App, opts: &Opts, hits: &[queries::SearchHit]) -> Result<()> {
    if opts.json {
        json_out(&hits);
        return Ok(());
    }
    let scope = opts.read_scope(&app.client);
    for h in hits {
        println!(
            "{:>12}  {:8.3}  {}",
            h.target.display_ref(scope),
            h.score,
            h.snippet
        );
    }
    Ok(())
}

// ── backlinks / references (SPEC §10.20) ──────────────────────────

pub fn backlinks(app: &App, opts: &Opts, r: &RefArg) -> Result<()> {
    print_page(app, opts, queries::backlinks(app, opts, r, None, None)?)
}

pub fn backlinks_file(app: &App, opts: &Opts, space: Option<&str>, path: &str) -> Result<()> {
    print_page(
        app,
        opts,
        queries::backlinks_file(app, opts, space, path, None, None)?,
    )
}

pub fn references(app: &App, opts: &Opts, r: &RefArg) -> Result<()> {
    let listing = queries::references(app, opts, r)?;
    if opts.json {
        // The full partitioning (SPEC §10.20): resolved, deleted,
        // unknown, unresolved — the text renderer shows all four,
        // so must the JSON one.
        json_out(&listing);
        return Ok(());
    }
    let scope = opts.read_scope(&app.client);
    for jot in &listing.resolved {
        println!("{:>12}  {}", short_ref_view(jot, scope), jot.summary);
    }
    for ident in &listing.deleted {
        let r = match scope {
            Some(p) if *p == ident.profile => format!("--{}", ident.id),
            _ => format!("--{}--{}", ident.profile, ident.id),
        };
        println!("{r:>12}  (deleted)");
    }
    for written in &listing.unknown {
        println!("{:>12}  (unknown entry)", format!("--{written}"));
    }
    for raw in &listing.unresolved {
        println!("{:>12}  (unresolved)", format!("--{raw}"));
    }
    Ok(())
}

// ── attachments (SPEC §10.22) ─────────────────────────────────────

pub fn attachments(app: &App, opts: &Opts, r: &RefArg) -> Result<()> {
    let atts = queries::attachments(app, opts, r)?;
    if opts.json {
        json_out(&atts);
        return Ok(());
    }
    for a in &atts {
        println!(
            "{}  {} bytes  {}  {}",
            a.filename,
            a.size,
            a.mime.as_deref().unwrap_or("-"),
            format_iso8601_secs(a.added_at_ns)
        );
    }
    Ok(())
}

// ── history / log (SPEC §10.26) ───────────────────────────────────

pub fn history(app: &App, opts: &Opts, r: &RefArg) -> Result<()> {
    let events = queries::history(app, opts, r)?;
    if opts.json {
        json_out(&events);
        return Ok(());
    }
    for e in &events {
        println!("{}  {}", format_iso8601_secs(e.at_ns), e.description);
    }
    Ok(())
}

pub fn vault_log(
    app: &App,
    opts: &Opts,
    since: Option<&str>,
    until: Option<&str>,
    n: Option<usize>,
) -> Result<()> {
    if opts.json {
        json_out(&queries::vault_log(app, since, until, n)?);
        return Ok(());
    }
    for e in queries::vault_log(app, since, until, n)? {
        println!(
            "{}  {}  {} fact(s)",
            &e.state[..12],
            format_iso8601_secs(e.at_ns),
            e.facts
        );
    }
    Ok(())
}

// ── profiles / config (SPEC §10.27, 10.29) ────────────────────────

pub fn profiles(app: &App, opts: &Opts) -> Result<()> {
    let views = queries::profiles(app, opts)?;
    if opts.json {
        json_out(&views);
        return Ok(());
    }
    for p in &views {
        let marker = if p.active { "*" } else { " " };
        println!("{marker} {}  ({} jots, .{})", p.name, p.jots, p.default_ext);
    }
    Ok(())
}

pub fn profile_show(app: &App, opts: &Opts, name: &str) -> Result<()> {
    let reads = app.reads(opts)?;
    let p = reads.profile(name)?;
    println!("{}", serde_json::to_string_pretty(&json!(p)).unwrap());
    Ok(())
}

pub fn config_show(app: &App, opts: &Opts) -> Result<()> {
    let reads = app.reads(opts)?;
    let profile_name = opts
        .read_scope(&app.client)
        .unwrap_or(&app.client.active_profile)
        .to_string();
    println!("# client ({})", app.client.path.display());
    println!(
        "vault          = {:?}",
        app.client.vault.display().to_string()
    );
    println!("active_profile = {:?}", app.client.active_profile);
    if let Some(e) = &app.client.editor {
        println!("editor         = {e:?}");
    }
    for (k, v) in &app.client.extract {
        println!("extract.{k}    = {v:?}");
    }
    for (k, v) in &app.client.spaces {
        println!("space.{k}      = {:?}", v.display().to_string());
    }
    if let Ok(p) = reads.profile(&profile_name) {
        println!("\n# vault profile {:?}", p.name);
        println!("{}", serde_json::to_string_pretty(&json!(p)).unwrap());
    }
    Ok(())
}

// ── export (SPEC §10.34) ──────────────────────────────────────────

pub fn export(app: &App, opts: &Opts, dir: &Path) -> Result<()> {
    let count = queries::export(app, opts, dir)?;
    println!("exported {count} file(s) to {}", dir.display());
    Ok(())
}

// ── 0.5 read views ────────────────────────────────────────────────

pub fn show_file(app: &App, opts: &Opts, space: Option<&str>, path: &str) -> Result<()> {
    let (view, content) = queries::show_file(app, opts, space, path)?;
    if opts.json {
        let mut v = serde_json::to_value(&view).unwrap_or(Value::Null);
        v["content"] = Value::String(content);
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(());
    }
    println!("///{}/{}", view.space, view.path);
    if let Some(m) = view.modified_at_ns {
        println!("modified {}", format_iso8601_secs(m));
    }
    let mut labels: Vec<String> = view.tags.iter().map(|t| format!("..{t}")).collect();
    labels.extend(view.categories.iter().map(|c| format!("::{c}")));
    if !labels.is_empty() {
        println!("labels   {}", labels.join(" "));
    }
    println!();
    print!("{content}");
    if !content.ends_with('\n') {
        println!();
    }
    Ok(())
}

pub fn spaces(app: &App, opts: &Opts) -> Result<()> {
    let views = queries::spaces(app, opts)?;
    if opts.json {
        json_out(&views);
        return Ok(());
    }
    for s in &views {
        let dialect = match &s.dialect {
            Some(d) => format!(", dialect {d}"),
            None => String::new(),
        };
        println!(
            "{}  ({} file(s), {}{dialect})",
            s.name,
            s.files,
            s.dir.as_deref().unwrap_or("no local directory")
        );
    }
    Ok(())
}

pub fn files(app: &App, opts: &Opts, space: Option<&str>) -> Result<()> {
    let views = queries::files(app, opts, space)?;
    if opts.json {
        json_out(&views);
        return Ok(());
    }
    for f in &views {
        let m = f
            .modified_at_ns
            .map(format_iso8601_secs)
            .unwrap_or_default();
        println!("///{}/{}  {}", f.space, f.path, m);
    }
    Ok(())
}

pub fn templates(app: &App, opts: &Opts) -> Result<()> {
    let views = queries::templates(app, opts)?;
    if opts.json {
        json_out(&views);
        return Ok(());
    }
    let widened = opts.read_scope(&app.client).is_none();
    for t in &views {
        if widened {
            println!("{}:{}  (.{})", t.profile, t.name, t.ext);
        } else {
            println!("{}  (.{})", t.name, t.ext);
        }
    }
    Ok(())
}

pub fn template_show(app: &App, opts: &Opts, name: &str) -> Result<()> {
    let views = queries::templates(app, opts)?;
    let hits: Vec<_> = views.iter().filter(|t| t.name == name).collect();
    if hits.len() > 1 {
        return Err(Error::Usage(format!(
            "template {name:?} is ambiguous under --profile all ({})",
            hits.iter()
                .map(|t| t.profile.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let t = hits
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotFound(format!("template {name:?}")))?;
    print!("{}", t.content);
    if !t.content.ends_with('\n') {
        println!();
    }
    Ok(())
}

pub fn saved_queries(app: &App, opts: &Opts) -> Result<()> {
    let views = queries::saved_queries(app, opts)?;
    if opts.json {
        json_out(&views);
        return Ok(());
    }
    let widened = opts.read_scope(&app.client).is_none();
    for q in &views {
        if widened {
            println!("{}:{}  {}", q.profile, q.name, q.args.join(" "));
        } else {
            println!("{}  {}", q.name, q.args.join(" "));
        }
    }
    Ok(())
}

pub fn today(app: &App, opts: &Opts, date: Option<&str>) -> Result<()> {
    let view = queries::today(app, opts, date)?;
    if opts.json {
        json_out(&view);
        return Ok(());
    }
    let scope = opts.read_scope(&app.client);
    println!("# {}", view.date);
    for jot in &view.jots {
        let r = match scope {
            Some(p) if *p == jot.profile => format!("--{}", jot.id),
            _ => format!("--{}--{}", jot.profile, jot.id),
        };
        println!("{r:>12}  {}", jot.summary);
    }
    if !view.due.is_empty() {
        println!("due:");
        print_agenda_items(&view.due, scope);
    }
    Ok(())
}

pub fn agenda(app: &App, opts: &Opts, until: Option<&str>) -> Result<()> {
    let items = queries::agenda(app, opts, until)?;
    if opts.json {
        json_out(&items);
        return Ok(());
    }
    print_agenda_items(&items, opts.read_scope(&app.client));
    Ok(())
}

fn print_agenda_items(items: &[queries::AgendaItem], scope: Option<&str>) {
    for i in items {
        let r = match scope {
            Some(p) if *p == i.profile => format!("--{}", i.id),
            _ => format!("--{}--{}", i.profile, i.id),
        };
        let due = i.todo.due.as_deref().unwrap_or("");
        println!("{r:>12}  {due:<20}  {}", i.todo.raw);
    }
}

pub fn graph(app: &App, opts: &Opts, dot: bool) -> Result<()> {
    let g = queries::graph(app, opts)?;
    if dot {
        println!("digraph cuj {{");
        for n in &g.nodes {
            println!(
                "  {:?} [label={:?}];",
                n.reference,
                format!("{}\n{}", n.reference, n.label)
            );
        }
        for e in &g.edges {
            println!("  {:?} -> {:?};", e.from, e.to);
        }
        println!("}}");
    } else {
        json_out(&g);
    }
    Ok(())
}

// ── q: quarb queries (SPEC appendix) ──────────────────────────────

/// `cuj quarb-serve`: expose the vault arbor over the Quarb
/// serve protocol (stdin/stdout), so `qua` mounts the vault
/// beside any other source without linking cuj.
pub fn quarb_serve(app: &App, opts: &Opts) -> Result<()> {
    let reads = app.reads(opts)?;
    let arbor = crate::arbor::VaultArbor::build(&reads);
    quarb_serve::serve(&arbor, |n| arbor.display(n), "cuj")
        .map_err(|e| Error::Usage(format!("quarb-serve: {e}")))
}

pub fn quarb_query(app: &App, opts: &Opts, expr: &str) -> Result<()> {
    let reads = app.reads(opts)?;
    let arbor = crate::arbor::VaultArbor::build(&reads);
    // The invocation instant is always bound — from --now or one
    // startup clock read; unbound, now() silently matches nothing.
    let (secs, nanos) = match &opts.now {
        Some(s) => quarb::temporal::parse_iso(s)
            .map(|(secs, nanos, _)| (secs, nanos))
            .ok_or_else(|| Error::Usage(format!("bad --now instant {s:?}")))?,
        None => {
            let ns = crate::facts::now_ns();
            (
                ns.div_euclid(1_000_000_000),
                ns.rem_euclid(1_000_000_000) as u32,
            )
        }
    };
    let nowed = quarb::WithNow {
        inner: &arbor,
        secs,
        nanos,
    };
    let result = quarb::run(expr, &nowed).map_err(|e| Error::Usage(format!("quarb: {e}")))?;
    match result {
        quarb::QueryResult::Nodes(nodes) => {
            if opts.json {
                let refs: Vec<String> = nodes.iter().map(|n| arbor.display(*n)).collect();
                json_out(&refs);
            } else {
                for n in nodes {
                    println!("{}", arbor.display(n));
                }
            }
        }
        quarb::QueryResult::Values(values) => {
            if opts.json {
                let vals: Vec<serde_json::Value> = values
                    .iter()
                    .map(|v| match v {
                        quarb::Value::Null => Value::Null,
                        quarb::Value::Bool(b) => json!(b),
                        quarb::Value::Int(n) => json!(n),
                        quarb::Value::Float(x) => json!(x),
                        quarb::Value::Str(s) => json!(s),
                        quarb::Value::List(_)
                        | quarb::Value::Record(_)
                        | quarb::Value::Instant { .. }
                        | quarb::Value::Duration { .. }
                        | quarb::Value::Quantity { .. } => json!(v.to_string()),
                    })
                    .collect();
                json_out(&vals);
            } else {
                for v in values {
                    println!("{v}");
                }
            }
        }
    }
    Ok(())
}

// ── fsck (SPEC §10.36) ────────────────────────────────────────────

pub fn fsck(app: &App, opts: &Opts) -> Result<()> {
    let outcome = queries::fsck(app)?;
    if opts.json {
        // Exactly one JSON value (SPEC §11.2): the outcome object
        // on a clean vault; a broken one surfaces through the
        // standard error envelope, problems joined in the message.
        if outcome.substrate_errors() == 0 && outcome.problems.is_empty() {
            json_out(&outcome);
            return Ok(());
        }
        let mut parts: Vec<String> = outcome
            .layers
            .iter()
            .flat_map(|l| l.errors.iter().cloned())
            .collect();
        parts.extend(outcome.problems.iter().cloned());
        return Err(Error::Substrate(format!("fsck: {}", parts.join("; "))));
    }
    for layer in &outcome.layers {
        println!(
            "{}: {}, {} error(s)",
            layer.name,
            layer.summary,
            layer.errors.len()
        );
        for e in &layer.errors {
            println!("{}: ERROR: {e}", layer.name);
        }
    }
    let substrate_errors = outcome.substrate_errors();
    if substrate_errors > 0 {
        return Err(Error::Substrate(format!(
            "{substrate_errors} substrate fsck error(s)"
        )));
    }
    if outcome.problems.is_empty() {
        println!(
            "cuj: ok ({} jots, {} profiles)",
            outcome.jots, outcome.profiles
        );
        Ok(())
    } else {
        for p in &outcome.problems {
            println!("cuj: PROBLEM: {p}");
        }
        Err(Error::Substrate(format!(
            "{} cuj-level problem(s)",
            outcome.problems.len()
        )))
    }
}
