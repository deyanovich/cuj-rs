//! The cuj CLI (SPEC §11): global-flag extraction, dispatch, exit
//! codes. No argument-parsing dependency, matching the family style.

use std::path::PathBuf;
use std::process::ExitCode;

use cuj::app::{App, Opts, RefArg, either, parse_ref};
use cuj::client::ClientConfig;
use cuj::error::{Error, Result};
use cuj::facts;
use cuj::read::{self, SimilarTarget};

const USAGE: &str = "\
cuj — note-jotting on Jot Syntax + metatheca (protocol 0.5)

usage:
  cuj init [<profile>]                 create the vault / a profile
  cuj [--md|--txt|--tex|--html] -- <text>...   create a jot
  cuj new <template> [<text>...]       create from a template
  cuj --<id>  |  cuj show --<id>[#bm|:N-M] [--expand]   view a jot
  cuj show //<path> | ///<space>/<path>        view a file-jot
  cuj list                             list live jots
  cuj find [..tag] [::cat] [--since D] [--until D] [--todo]
           [--filter EXPR]                     typed filtering
  cuj query <name> | query save <name> <find args...> | query rm <name>
  cuj queries                          list saved queries
  cuj q '<expr>'                       Quarb query over the vault arbor
  cuj quarb-serve                      serve the vault arbor to qua (mounting)
  cuj today [<date>] | agenda [--until D]      day / todo views
  cuj grep [filters --] <pattern>      unindexed regex scan
  cuj search [--lexical|--semantic] <query>    ranked search (-k N)
  cuj similar --<id>                   semantic neighbours
  cuj label --<id>... <..tag|::cat>... apply metadata (alias: apply)
  cuj link --<src> --<tgt>             add a reference
  cuj todo [--<id>...] ??<action> [description]
  cuj todo done --<id> <match>         complete a todo
  cuj edit --<id>                      edit content in $EDITOR
  cuj rm --<id>                        delete (tombstone)
  cuj mv --from <p> --to <p> --<id>    move between profiles
  cuj promote ..tag | ::category       taxonomy mobility
  cuj demote ::category
  cuj backlinks --<id>|//<path>|///<space>/<path> | references --<id>
  cuj graph [--dot]                    reference graph export
  cuj attach <file>... --<id> | detach <name>... --<id>
  cuj attachments --<id>
  cuj space add <name> [--dialect <d>] | space rm <name> | spaces
                                       file-space registry
  cuj observe [<space>]                snapshot observed files
  cuj files [<space>]                  list observed file-jots
  cuj ingest                           drain the capture inbox
  cuj template set <name> [--md|...] -- <text> | show|rm <name>
  cuj templates                        list templates
  cuj history --<id> | log [-n N]
  cuj profile [show <name>] | profiles | switch <name>
  cuj ext [set --md|--txt|--tex|--html]
  cuj config [get <key> | set [--vault] <key> <value>]
  cuj reindex                          rebuild the derived indexes
  cuj reparse                          re-parse content, refresh jot/index facts
  cuj migrate                          move a 0.4 vault to 0.5
  cuj import [--category ::cat]... [--dry-run] [--freeze-todos]
             [--dialect obsidian|none]
             <file.enex>... | <dir>
  cuj export <dir>                     materialize a working tree
  cuj fsck                             integrity checks

global flags: --profile <name|all>, --json, --at <stateref>,
              --now <instant> (q), --force, --stale, -k <n>
";

fn main() -> ExitCode {
    // Die quietly when the read end of a pipe closes (`cuj list |
    // head`) instead of panicking on the first failed println.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Mirror split_flags: --json is a global flag only
            // before `--`; a literal --json in message text must
            // not switch the error rendering.
            let json_mode = std::env::args()
                .take_while(|a| a != "--")
                .any(|a| a == "--json");
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({"error": {"code": e.code(), "message": e.to_string()}})
                );
            } else {
                eprintln!("cuj: {e}");
            }
            ExitCode::from(e.exit_code().clamp(0, 255) as u8)
        }
    }
}

struct Cli {
    opts: Opts,
    /// Positional tokens with global flags removed.
    args: Vec<String>,
}

fn split_flags(argv: Vec<String>) -> Result<Cli> {
    let mut opts = Opts {
        stale_notes: true,
        ..Opts::default()
    };
    let mut args = Vec::new();
    let mut it = argv.into_iter();
    let mut after_dashdash = false;
    while let Some(a) = it.next() {
        if after_dashdash {
            args.push(a);
            continue;
        }
        match a.as_str() {
            "--" => {
                args.push(a);
                after_dashdash = true;
            }
            "--json" => opts.json = true,
            "--force" => opts.force = true,
            "--stale" => opts.stale = true,
            "--profile" => {
                opts.profile = Some(
                    it.next()
                        .ok_or_else(|| Error::Usage("--profile needs a name".into()))?,
                )
            }
            "--at" => {
                opts.at = Some(
                    it.next()
                        .ok_or_else(|| Error::Usage("--at needs a stateref".into()))?,
                )
            }
            "--now" => {
                opts.now = Some(
                    it.next()
                        .ok_or_else(|| Error::Usage("--now needs an ISO instant".into()))?,
                )
            }
            "-k" => {
                let n = it
                    .next()
                    .ok_or_else(|| Error::Usage("-k needs a number".into()))?;
                opts.k = Some(
                    n.parse()
                        .map_err(|_| Error::Usage(format!("bad -k value {n:?}")))?,
                );
            }
            "--md" | "--txt" | "--tex" | "--html" => {
                opts.ext_override = Some(a[2..].to_string());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => args.push(a),
        }
    }
    Ok(Cli { opts, args })
}

/// Split leading `--N` reference args from the remaining tokens.
fn take_refs(args: &[String]) -> (Vec<RefArg>, Vec<String>) {
    let mut refs = Vec::new();
    let mut rest = Vec::new();
    for a in args {
        if rest.is_empty() {
            if let Some(r) = parse_ref(a) {
                refs.push(r);
                continue;
            }
        }
        rest.push(a.clone());
    }
    (refs, rest)
}

/// Write commands MUST reject --json (SPEC §11.1); import is the
/// one exception, whose report is a documented JSON value.
fn reject_json(opts: &Opts, what: &str) -> Result<()> {
    if opts.json {
        return Err(Error::Usage(format!("{what} does not support --json")));
    }
    Ok(())
}

/// Write commands MUST be rejected under --at (SPEC §10.35). The
/// App methods that take an Opts enforce this themselves; the
/// opts-less maintenance ops are guarded here at dispatch.
fn reject_at(opts: &Opts, what: &str) -> Result<()> {
    if opts.at.is_some() {
        return Err(Error::Usage(format!("{what} cannot run under --at")));
    }
    Ok(())
}

fn one_ref(args: &[String], what: &str) -> Result<RefArg> {
    let (refs, rest) = take_refs(args);
    if refs.len() != 1 || !rest.is_empty() {
        return Err(Error::Usage(format!("{what} takes exactly one --<id>")));
    }
    Ok(refs.into_iter().next().unwrap())
}

/// Write commands take a bare ref: a #bookmark / :lines suffix
/// names part of a jot, and silently applying the op to the whole
/// jot would do more than the user asked.
fn one_bare_ref(args: &[String], what: &str) -> Result<RefArg> {
    let r = one_ref(args, what)?;
    if r.suffix.is_some() {
        return Err(Error::Usage(format!(
            "{what} takes a bare --<id> (no #bookmark or :lines suffix)"
        )));
    }
    Ok(r)
}

use cuj::queries::parse_find_args;

/// `//path` / `///space/path` file-reference arguments (SPEC
/// §file references).
fn parse_file_ref(arg: &str) -> Option<(Option<String>, String)> {
    if let Some(rest) = arg.strip_prefix("///") {
        let (space, path) = rest.split_once('/')?;
        (!space.is_empty() && !path.is_empty()).then(|| (Some(space.to_string()), path.to_string()))
    } else if let Some(rest) = arg.strip_prefix("//") {
        (!rest.is_empty() && !rest.starts_with('/')).then(|| (None, rest.to_string()))
    } else {
        None
    }
}

fn run() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        print!("{USAGE}");
        return Ok(());
    }
    let Cli { opts, args } = split_flags(argv)?;
    let client = ClientConfig::load()?;

    // `cuj -- <text>` and `cuj --<id>` come before subcommand match.
    if let Some(first) = args.first() {
        if first == "--" {
            reject_json(&opts, "create")?;
            let text = args[1..].join(" ");
            if text.trim().is_empty() {
                return Err(Error::Usage("create: empty jot text".into()));
            }
            let app = App::open(client)?;
            let ident = app.create(&opts, &text)?;
            println!("--{}--{}", ident.profile, ident.id);
            return Ok(());
        }
        if args.len() == 1 {
            if let Some(r) = parse_ref(first) {
                let app = App::open(client)?;
                return read::show(&app, &opts, &r, false);
            }
            if let Some((space, path)) = parse_file_ref(first) {
                let app = App::open(client)?;
                return read::show_file(&app, &opts, space.as_deref(), &path);
            }
        }
    }

    let cmd = args[0].clone();
    let rest = &args[1..];

    match cmd.as_str() {
        "init" => {
            reject_json(&opts, "init")?;
            reject_at(&opts, "init")?;
            App::init(client, rest.first().map(String::as_str))
        }
        "show" => {
            let app = App::open(client)?;
            let mut expand = false;
            let mut target: Option<&str> = None;
            for a in rest {
                match a.as_str() {
                    "--expand" => expand = true,
                    other if target.is_none() => target = Some(other),
                    other => return Err(Error::Usage(format!("show: unexpected {other:?}"))),
                }
            }
            let target = target
                .ok_or_else(|| Error::Usage("show --<id> | //<path> | ///<space>/<path>".into()))?;
            if let Some((space, path)) = parse_file_ref(target) {
                read::show_file(&app, &opts, space.as_deref(), &path)
            } else if let Some(r) = parse_ref(target) {
                read::show(&app, &opts, &r, expand)
            } else {
                Err(Error::Usage(format!("show: bad reference {target:?}")))
            }
        }
        "new" => {
            reject_json(&opts, "new")?;
            let app = App::open(client)?;
            let name = rest
                .first()
                .ok_or_else(|| Error::Usage("new <template> [<text>...]".into()))?;
            let text = rest[1..].join(" ");
            let ident = app.new_from_template(&opts, name, &text)?;
            println!("--{}--{}", ident.profile, ident.id);
            Ok(())
        }
        "list" => read::list(&App::open(client)?, &opts),
        "find" => {
            let app = App::open(client)?;
            let f = parse_find_args(rest)?;
            read::find(&app, &opts, &f)
        }
        "grep" => {
            let app = App::open(client)?;
            // `cuj grep [filters --] <pattern>`
            let (filters, pattern) = match rest.iter().position(|a| a == "--") {
                Some(i) => (parse_find_args(&rest[..i])?, rest[i + 1..].join(" ")),
                None => (parse_find_args(&[])?, rest.join(" ")),
            };
            if pattern.trim().is_empty() {
                return Err(Error::Usage("grep needs a pattern".into()));
            }
            read::grep(&app, &opts, &pattern, &filters)
        }
        "search" => {
            let app = App::open(client)?;
            // Mode flags are recognized anywhere, not only as the
            // first token — `search foo --lexical` must not search
            // for the literal string "foo --lexical".
            let mut mode = "default";
            let mut terms: Vec<String> = Vec::new();
            for a in rest {
                match a.as_str() {
                    "--semantic" | "--lexical" if mode != "default" => {
                        return Err(Error::Usage("search takes one mode flag".into()));
                    }
                    "--semantic" => mode = "semantic",
                    "--lexical" => mode = "lexical",
                    _ => terms.push(a.clone()),
                }
            }
            let query = terms.join(" ");
            if query.trim().is_empty() {
                return Err(Error::Usage("search needs a query".into()));
            }
            match mode {
                "semantic" => read::similar(&app, &opts, SimilarTarget::Text(&query)),
                m => read::search(&app, &opts, &query, m == "lexical"),
            }
        }
        "similar" => {
            let app = App::open(client)?;
            let r = one_ref(rest, "similar")?;
            read::similar(&app, &opts, SimilarTarget::Jot(&r))
        }
        "label" | "apply" => {
            reject_json(&opts, "label")?;
            let app = App::open(client)?;
            let (refs, tokens) = take_refs(rest);
            if refs.is_empty() || tokens.is_empty() {
                return Err(Error::Usage("label --<id>... <..tag|::category>...".into()));
            }
            for line in app.label(&opts, &refs, &tokens)? {
                println!("{line}");
            }
            Ok(())
        }
        "link" => {
            reject_json(&opts, "link")?;
            let app = App::open(client)?;
            let (refs, tokens) = take_refs(rest);
            if refs.len() != 2 || !tokens.is_empty() {
                return Err(Error::Usage("link --<src> --<tgt>".into()));
            }
            let changed = app.link(&opts, &refs[0], &refs[1])?;
            println!("{}", if changed { "linked" } else { "already linked" });
            Ok(())
        }
        "todo" => {
            reject_json(&opts, "todo")?;
            let app = App::open(client)?;
            if rest.first().map(String::as_str) == Some("done") {
                let (refs, tokens) = take_refs(&rest[1..]);
                if refs.len() != 1 || tokens.is_empty() {
                    return Err(Error::Usage("todo done --<id> <match>".into()));
                }
                println!("{}", app.todo_done(&opts, &refs[0], &tokens.join(" "))?);
                return Ok(());
            }
            let (refs, tokens) = take_refs(rest);
            let construct = tokens.join(" ");
            if !construct.starts_with("??") {
                return Err(Error::Usage(
                    "todo [--<id>...] ??<action> [description]".into(),
                ));
            }
            // A ref-shaped token after the construct in a ref-less
            // invocation is ambiguous — a misplaced target and a
            // reference in the todo text read identically. Refuse
            // and show both spellings rather than guess. Leading
            // refs signal the convention is known; embedded refs
            // are then intentional.
            if refs.is_empty() {
                if let Some(t) = tokens.iter().skip(1).find(|t| parse_ref(t).is_some()) {
                    return Err(Error::Usage(format!(
                        "todo: {t:?} after the construct is ambiguous — refs lead \
                         (todo {t} ??...); to reference a jot in the todo text, \
                         use: cuj -- {construct:?}"
                    )));
                }
            }
            match app.todo(&opts, &refs, &construct)? {
                either::TodoOutcome::Created(ident) => {
                    println!("--{}--{}", ident.profile, ident.id)
                }
                either::TodoOutcome::Applied(idents) => {
                    for i in idents {
                        println!("--{}--{} todo added", i.profile, i.id);
                    }
                }
            }
            Ok(())
        }
        "edit" => {
            reject_json(&opts, "edit")?;
            let app = App::open(client)?;
            let r = one_bare_ref(rest, "edit")?;
            let changed = app.edit(&opts, &r)?;
            println!("{}", if changed { "updated" } else { "unchanged" });
            Ok(())
        }
        "rm" => {
            reject_json(&opts, "rm")?;
            let app = App::open(client)?;
            let r = one_bare_ref(rest, "rm")?;
            let ident = app.rm(&opts, &r)?;
            println!("--{}--{} deleted", ident.profile, ident.id);
            Ok(())
        }
        "mv" => {
            reject_json(&opts, "mv")?;
            let app = App::open(client)?;
            let mut from = None;
            let mut to = None;
            let mut refs = Vec::new();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--from" => from = it.next().cloned(),
                    "--to" => to = it.next().cloned(),
                    other => {
                        if let Some(r) = parse_ref(other) {
                            refs.push(r);
                        } else {
                            return Err(Error::Usage(format!("mv: unexpected {other:?}")));
                        }
                    }
                }
            }
            let to = to.ok_or_else(|| Error::Usage("mv needs --to <profile>".into()))?;
            if refs.len() != 1 {
                return Err(Error::Usage("mv --from <p> --to <p> --<id>".into()));
            }
            if refs[0].suffix.is_some() {
                return Err(Error::Usage(
                    "mv takes a bare --<id> (no #bookmark or :lines suffix)".into(),
                ));
            }
            // A qualified ref names its own source profile; a bare
            // ref falls back to --from, then the active profile.
            let from = match (&refs[0].profile, from) {
                (Some(p), Some(f)) if *p != f => {
                    return Err(Error::Usage(format!(
                        "mv: ref names profile {p:?} but --from says {f:?}"
                    )));
                }
                (Some(p), _) => p.clone(),
                (None, Some(f)) => f,
                (None, None) => app.client.active_profile.clone(),
            };
            let (old, new) = app.mv(&opts, &from, &to, refs[0].id)?;
            println!(
                "--{}--{} -> --{}--{}",
                old.profile, old.id, new.profile, new.id
            );
            Ok(())
        }
        "promote" => {
            reject_json(&opts, "promote")?;
            let app = App::open(client)?;
            let arg = rest
                .first()
                .ok_or_else(|| Error::Usage("promote ..tag | ::category".into()))?;
            let lines = if let Some(tag) = arg.strip_prefix("..") {
                app.promote_tag(&opts, tag)?
            } else if let Some(cat) = arg.strip_prefix("::") {
                app.promote_category(&opts, cat)?
            } else {
                return Err(Error::Usage("promote ..tag | ::category".into()));
            };
            for l in lines {
                println!("{l}");
            }
            Ok(())
        }
        "demote" => {
            reject_json(&opts, "demote")?;
            let app = App::open(client)?;
            let arg = rest
                .first()
                .and_then(|a| a.strip_prefix("::"))
                .ok_or_else(|| Error::Usage("demote ::category".into()))?;
            for l in app.demote_category(&opts, arg)? {
                println!("{l}");
            }
            Ok(())
        }
        "backlinks" => {
            let app = App::open(client)?;
            if let Some(first) = rest.first() {
                if let Some((space, path)) = parse_file_ref(first) {
                    if rest.len() != 1 {
                        return Err(Error::Usage("backlinks takes one reference".into()));
                    }
                    return read::backlinks_file(&app, &opts, space.as_deref(), &path);
                }
            }
            let r = one_ref(rest, "backlinks")?;
            read::backlinks(&app, &opts, &r)
        }
        "references" => {
            let app = App::open(client)?;
            let r = one_ref(rest, "references")?;
            read::references(&app, &opts, &r)
        }
        "attach" => {
            reject_json(&opts, "attach")?;
            let app = App::open(client)?;
            let (refs, files) = split_trailing_ref(rest)?;
            let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            if files.is_empty() {
                return Err(Error::Usage("attach <file>... --<id>".into()));
            }
            for line in app.attach(&opts, &files, &refs)? {
                println!("{line}");
            }
            Ok(())
        }
        "detach" => {
            reject_json(&opts, "detach")?;
            let app = App::open(client)?;
            let (r, names) = split_trailing_ref(rest)?;
            if names.is_empty() {
                return Err(Error::Usage("detach <filename>... --<id>".into()));
            }
            for line in app.detach(&opts, &names, &r)? {
                println!("{line}");
            }
            Ok(())
        }
        "attachments" => {
            let app = App::open(client)?;
            let r = one_ref(rest, "attachments")?;
            read::attachments(&app, &opts, &r)
        }
        "history" => {
            let app = App::open(client)?;
            let r = one_ref(rest, "history")?;
            read::history(&app, &opts, &r)
        }
        "log" => {
            let app = App::open(client)?;
            let mut since = None;
            let mut until = None;
            let mut n = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--since" => since = it.next().cloned(),
                    "--until" => until = it.next().cloned(),
                    "-n" => n = it.next().and_then(|x| x.parse().ok()),
                    other => return Err(Error::Usage(format!("log: unexpected {other:?}"))),
                }
            }
            read::vault_log(&app, &opts, since.as_deref(), until.as_deref(), n)
        }
        "profile" => {
            let app = App::open(client)?;
            match rest.first().map(String::as_str) {
                None => {
                    println!("{}", app.client.active_profile);
                    Ok(())
                }
                Some("show") => {
                    let name = rest
                        .get(1)
                        .ok_or_else(|| Error::Usage("profile show <name>".into()))?;
                    read::profile_show(&app, &opts, name)
                }
                Some(other) => Err(Error::Usage(format!("profile: unexpected {other:?}"))),
            }
        }
        "profiles" => read::profiles(&App::open(client)?, &opts),
        "switch" => {
            reject_json(&opts, "switch")?;
            let name = rest
                .first()
                .ok_or_else(|| Error::Usage("switch <profile>".into()))?;
            let app = App::open(client)?;
            let reads = app.reads(&opts)?;
            reads.profile(name)?;
            let mut c = app.client.clone();
            c.active_profile = name.clone();
            c.save()?;
            println!("switched to {name}");
            Ok(())
        }
        "ext" => {
            let app = App::open(client)?;
            let profile = opts.write_profile(&app.client)?.to_string();
            match rest.first().map(String::as_str) {
                None => {
                    let reads = app.reads(&opts)?;
                    println!("{}", reads.profile(&profile)?.default_ext);
                    Ok(())
                }
                Some("set") => {
                    reject_json(&opts, "ext set")?;
                    reject_at(&opts, "ext set")?;
                    let ext = opts
                        .ext_override
                        .clone()
                        .ok_or_else(|| Error::Usage("ext set --md|--txt|--tex|--html".into()))?;
                    facts::validate_ext(&ext)?;
                    app.update_profile(&profile, |p| p.default_ext = ext.clone())?;
                    println!("default_ext = {ext}");
                    Ok(())
                }
                Some(other) => Err(Error::Usage(format!("ext: unexpected {other:?}"))),
            }
        }
        "config" => config_cmd(client, &opts, rest),
        "quarb-serve" => {
            let app = App::open(client)?;
            read::quarb_serve(&app, &opts)
        }
        "q" => {
            let app = App::open(client)?;
            let expr = rest.join(" ");
            if expr.trim().is_empty() {
                return Err(Error::Usage("q <quarb expression>".into()));
            }
            read::quarb_query(&app, &opts, &expr)
        }
        "query" => match rest.first().map(String::as_str) {
            Some("save") => {
                reject_json(&opts, "query save")?;
                let app = App::open(client)?;
                let name = rest
                    .get(1)
                    .ok_or_else(|| Error::Usage("query save <name> <find args...>".into()))?;
                let args = &rest[2..];
                parse_find_args(args)?;
                app.query_save(&opts, name, args)?;
                println!("saved query {name}");
                Ok(())
            }
            Some("rm") => {
                reject_json(&opts, "query rm")?;
                let app = App::open(client)?;
                let name = rest
                    .get(1)
                    .ok_or_else(|| Error::Usage("query rm <name>".into()))?;
                app.query_rm(&opts, name)?;
                println!("removed query {name}");
                Ok(())
            }
            Some(name) => read::saved_query(&App::open(client)?, &opts, name),
            None => Err(Error::Usage(
                "query <name> | query save <name> ... | query rm <name>".into(),
            )),
        },
        "queries" => read::saved_queries(&App::open(client)?, &opts),
        "today" => {
            let app = App::open(client)?;
            read::today(&app, &opts, rest.first().map(String::as_str))
        }
        "agenda" => {
            let app = App::open(client)?;
            let until = match rest.first().map(String::as_str) {
                Some("--until") => Some(
                    rest.get(1)
                        .ok_or_else(|| Error::Usage("--until needs a date".into()))?
                        .as_str(),
                ),
                Some(other) => return Err(Error::Usage(format!("agenda: unexpected {other:?}"))),
                None => None,
            };
            read::agenda(&app, &opts, until)
        }
        "graph" => {
            let app = App::open(client)?;
            let dot = match rest.first().map(String::as_str) {
                Some("--dot") => true,
                Some(other) => return Err(Error::Usage(format!("graph: unexpected {other:?}"))),
                None => false,
            };
            read::graph(&app, &opts, dot)
        }
        "space" => {
            reject_json(&opts, "space")?;
            reject_at(&opts, "space")?;
            let app = App::open(client)?;
            match (rest.first().map(String::as_str), rest.get(1)) {
                (Some("add"), Some(name)) => {
                    let dialect = match rest.get(2).map(String::as_str) {
                        Some("--dialect") => Some(
                            rest.get(3)
                                .ok_or_else(|| {
                                    Error::Usage("--dialect takes a dialect name".into())
                                })?
                                .as_str(),
                        ),
                        Some(other) => {
                            return Err(Error::Usage(format!("space add: unexpected {other:?}")));
                        }
                        None => None,
                    };
                    match app.space_add(name, dialect)? {
                        "registered" => println!(
                            "added space {name} (bind it: cuj config set space.{name} <dir>)"
                        ),
                        outcome => println!("space {name}: {outcome}"),
                    }
                    Ok(())
                }
                (Some("rm"), Some(name)) => {
                    let files = app.space_rm(name)?;
                    println!("removed space {name} ({files} file path(s) retracted)");
                    Ok(())
                }
                _ => Err(Error::Usage(
                    "space add <name> [--dialect <dialect>] | space rm <name>".into(),
                )),
            }
        }
        "spaces" => read::spaces(&App::open(client)?, &opts),
        "observe" => {
            reject_json(&opts, "observe")?;
            reject_at(&opts, "observe")?;
            let app = App::open(client)?;
            let names: Vec<String> = match rest.first() {
                Some(n) => vec![n.clone()],
                None => app.client.spaces.keys().cloned().collect(),
            };
            if names.is_empty() {
                return Err(Error::Usage(
                    "no spaces bound on this machine (config set space.<name> <dir>)".into(),
                ));
            }
            for name in names {
                let report = app.observe(&name)?;
                if report.is_empty() {
                    println!("{name}: unchanged");
                } else {
                    for line in report {
                        println!("{line}");
                    }
                }
            }
            Ok(())
        }
        "files" => {
            let app = App::open(client)?;
            read::files(&app, &opts, rest.first().map(String::as_str))
        }
        "ingest" => {
            reject_json(&opts, "ingest")?;
            let app = App::open(client)?;
            let report = app.ingest(&opts)?;
            if report.is_empty() {
                println!("inbox empty");
            } else {
                for line in report {
                    println!("{line}");
                }
            }
            Ok(())
        }
        "template" => {
            let app = App::open(client)?;
            match rest.first().map(String::as_str) {
                Some("set") => {
                    reject_json(&opts, "template set")?;
                    let name = rest.get(1).ok_or_else(|| {
                        Error::Usage("template set <name> [--md|...] -- <text>".into())
                    })?;
                    let dashdash = rest.iter().position(|a| a == "--").ok_or_else(|| {
                        Error::Usage("template set <name> [--md|...] -- <text>".into())
                    })?;
                    let text = rest[dashdash + 1..].join(" ");
                    if text.trim().is_empty() {
                        return Err(Error::Usage("template set: empty template text".into()));
                    }
                    app.template_set(&opts, name, &text)?;
                    println!("set template {name}");
                    Ok(())
                }
                Some("show") => {
                    let name = rest
                        .get(1)
                        .ok_or_else(|| Error::Usage("template show <name>".into()))?;
                    read::template_show(&app, &opts, name)
                }
                Some("rm") => {
                    reject_json(&opts, "template rm")?;
                    let name = rest
                        .get(1)
                        .ok_or_else(|| Error::Usage("template rm <name>".into()))?;
                    app.template_rm(&opts, name)?;
                    println!("removed template {name}");
                    Ok(())
                }
                _ => Err(Error::Usage(
                    "template set <name> -- <text> | show <name> | rm <name>".into(),
                )),
            }
        }
        "templates" => read::templates(&App::open(client)?, &opts),
        "migrate" => {
            reject_json(&opts, "migrate")?;
            reject_at(&opts, "migrate")?;
            let app = App::open(client)?;
            println!("{}", app.migrate()?);
            Ok(())
        }
        "reindex" => {
            reject_json(&opts, "reindex")?;
            let app = App::open(client)?;
            app.reindex()?;
            println!("reindexed");
            Ok(())
        }
        "reparse" => {
            reject_json(&opts, "reparse")?;
            reject_at(&opts, "reparse")?;
            let app = App::open(client)?;
            match app.reparse()? {
                0 => println!("up to date"),
                n => println!("reparsed {n} jot/index fact(s)"),
            }
            Ok(())
        }
        "import" => {
            let app = App::open(client)?;
            let mut args = cuj::import::ImportArgs::default();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--category" => {
                        let c = it
                            .next()
                            .ok_or_else(|| Error::Usage("--category needs a ::category".into()))?;
                        args.categories.push(cuj::import::parse_category(c)?);
                    }
                    "--dry-run" => args.dry_run = true,
                    "--freeze-todos" => args.freeze_todos = true,
                    "--dialect" => {
                        let d = it.next().ok_or_else(|| {
                            Error::Usage("--dialect takes obsidian or none".into())
                        })?;
                        args.dialect = Some(d.clone());
                    }
                    other if other.starts_with("--") => {
                        return Err(Error::Usage(format!("import: unexpected {other:?}")));
                    }
                    path => args.files.push(PathBuf::from(path)),
                }
            }
            let report = cuj::import::run(&app, &opts, &args)?;
            cuj::import::print_report(&report, &opts);
            Ok(())
        }
        "export" => {
            let app = App::open(client)?;
            let dir = rest
                .first()
                .ok_or_else(|| Error::Usage("export <dir>".into()))?;
            read::export(&app, &opts, &PathBuf::from(dir))
        }
        "fsck" => read::fsck(&App::open(client)?, &opts),
        other => Err(Error::Usage(format!(
            "unknown command {other:?} (try `cuj --help`)"
        ))),
    }
}

/// `attach a.pdf b.png --42` — the single ref may sit anywhere.
fn split_trailing_ref(args: &[String]) -> Result<(RefArg, Vec<String>)> {
    let mut r = None;
    let mut rest = Vec::new();
    for a in args {
        if let Some(parsed) = parse_ref(a) {
            if r.is_some() {
                return Err(Error::Usage("exactly one --<id> expected".into()));
            }
            r = Some(parsed);
        } else {
            rest.push(a.clone());
        }
    }
    Ok((
        r.ok_or_else(|| Error::Usage("missing --<id>".into()))?,
        rest,
    ))
}

fn config_cmd(client: ClientConfig, opts: &Opts, rest: &[String]) -> Result<()> {
    match rest.first().map(String::as_str) {
        None => {
            let app = App::open(client)?;
            read::config_show(&app, opts)
        }
        Some("get") => {
            let key = rest
                .get(1)
                .ok_or_else(|| Error::Usage("config get <key>".into()))?;
            match key.as_str() {
                "vault" => println!("{}", client.vault.display()),
                "active_profile" => println!("{}", client.active_profile),
                "editor" => println!("{}", client.editor_command()),
                k if k.starts_with("extract.") => {
                    let ext = &k["extract.".len()..];
                    match client.extract.get(ext) {
                        Some(cmd) => println!("{cmd}"),
                        None => return Err(Error::NotFound(format!("client key {key:?}"))),
                    }
                }
                k if k.starts_with("space.") => {
                    let name = &k["space.".len()..];
                    match client.spaces.get(name) {
                        Some(dir) => println!("{}", dir.display()),
                        None => return Err(Error::NotFound(format!("client key {key:?}"))),
                    }
                }
                other => return Err(Error::NotFound(format!("client key {other:?}"))),
            }
            Ok(())
        }
        Some("set") => {
            reject_json(opts, "config set")?;
            let (vault_scope, kv) = match rest.get(1).map(String::as_str) {
                Some("--vault") => (true, &rest[2..]),
                _ => (false, &rest[1..]),
            };
            let key = kv
                .first()
                .ok_or_else(|| Error::Usage("config set [--vault] <key> <value>".into()))?;
            let value = kv
                .get(1)
                .ok_or_else(|| Error::Usage("config set [--vault] <key> <value>".into()))?;
            if vault_scope {
                reject_at(opts, "config set --vault")?;
                let app = App::open(client)?;
                let profile = opts.write_profile(&app.client)?.to_string();
                // Validate key and value up front: unknown keys are
                // errors, and the profile fact is only rewritten with
                // a value that parses (SPEC §10.29).
                match key.as_str() {
                    "default_ext" => facts::validate_ext(value)?,
                    "default_space" => {
                        if !value.is_empty() {
                            facts::validate_short_name(value, "space name")?;
                        }
                    }
                    "jots_frozen" => {
                        if value != "true" && value != "false" {
                            return Err(Error::Usage(format!(
                                "jots_frozen takes true|false, got {value:?}"
                            )));
                        }
                    }
                    "exclusive" | "required" => {
                        for pat in value.split(',').filter(|s| !s.is_empty()) {
                            facts::ConstraintPattern::parse(pat)?;
                        }
                    }
                    k if k.strip_prefix("parse.").is_some_and(|f| !f.is_empty()) => {
                        if value != "true" && value != "false" {
                            return Err(Error::Usage(format!(
                                "{key} takes true|false, got {value:?}"
                            )));
                        }
                    }
                    other => return Err(Error::NotFound(format!("vault profile key {other:?}"))),
                }
                let key2 = key.clone();
                let value2 = value.clone();
                app.update_profile(&profile, move |p| {
                    if let Some(flag) = key2.strip_prefix("parse.") {
                        p.parse
                            .insert(flag.to_string(), serde_json::Value::Bool(value2 == "true"));
                        return;
                    }
                    match key2.as_str() {
                        "default_ext" => p.default_ext = value2.clone(),
                        "default_space" => {
                            p.default_space = if value2.is_empty() {
                                None
                            } else {
                                Some(value2.clone())
                            }
                        }
                        "jots_frozen" => p.jots_frozen = value2 == "true",
                        "exclusive" => {
                            p.exclusive = value2
                                .split(',')
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect()
                        }
                        "required" => {
                            p.required = value2
                                .split(',')
                                .filter(|s| !s.is_empty())
                                .map(String::from)
                                .collect()
                        }
                        // Unreachable: keys are validated above.
                        _ => {}
                    }
                })?;
                println!("set {key} (profile {profile})");
                Ok(())
            } else {
                let mut c = client;
                match key.as_str() {
                    "vault" => c.vault = PathBuf::from(value),
                    "active_profile" => c.active_profile = value.clone(),
                    "editor" => c.editor = Some(value.clone()),
                    k if k.strip_prefix("space.").is_some_and(|s| !s.is_empty()) => {
                        let name = k["space.".len()..].to_lowercase();
                        // An empty value removes the binding.
                        if value.is_empty() {
                            c.spaces.remove(&name);
                        } else {
                            c.spaces.insert(name, PathBuf::from(value));
                        }
                    }
                    k if k.strip_prefix("extract.").is_some_and(|e| !e.is_empty()) => {
                        let ext = k["extract.".len()..].to_lowercase();
                        // An empty value removes the mapping.
                        if value.is_empty() {
                            c.extract.remove(&ext);
                        } else {
                            c.extract.insert(ext, value.clone());
                        }
                    }
                    other => return Err(Error::NotFound(format!("client key {other:?}"))),
                }
                c.save()?;
                println!("set {key}");
                Ok(())
            }
        }
        Some(other) => Err(Error::Usage(format!("config: unexpected {other:?}"))),
    }
}
