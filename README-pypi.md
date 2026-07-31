# cuj

*n. Clever Use of Jots* — a plain-text PKM engine for your
terminal, shipped prebuilt: `pip install cuj` puts the complete
`cuj` command on your PATH, no Rust toolchain and no compile.

Your notes are plain text. Now they have an engine:

```sh
cuj -- "throttle the sync loop before it melts the NAS ..homelab ::infra"
cuj import export.enex          # Evernote, checkboxes become todos
cuj import my-obsidian-vault    # wikilinks become references
cuj q '//*<todo && open> | rec(::description, ::due)'
cuj q '/default/3<-contradicts::summary'
```

One-line captures with inline [Jot Syntax](https://jotsyntax.org)
annotations — flat tags (`..tag`) *and* hierarchical categories
(`::work::alpha`), labeled references (`--3::cites`), todos with
due dates (`??charter@2026-05-30 …`), bookmarks; a
content-addressed fact store underneath (full history, `--at`
time travel); [Quarb](https://quarb.org) queries over the whole
vault via `cuj q`.

This distribution is the CLI. The Python library — `import cuj`,
`cuj.q(...)` returning typed values — is its sibling:
`pip install libcuj`. The two install side by side without
collision. Building from source instead: `cargo install cuj`
([crates.io](https://crates.io/crates/cuj)).

Linux (x86_64/aarch64) and macOS (x86_64/arm64); the storage
substrate's file locking is unix-only, so no Windows wheels for
now. Docs and worked examples: [cuj.net](https://cuj.net) and
the article
[*Ask your notes a real question*](https://quarb.org/articles/ask-your-notes.html).

License: MIT OR Apache-2.0.
