# cuj · Python bindings

[cuj](https://cuj.net) — *n. Clever Use of Jots* — is a
plain-text PKM engine: one-line captures, inline
[Jot Syntax](https://jotsyntax.org) annotations (tags,
hierarchical categories, labeled references, todos with due
dates), a content-addressed fact store underneath, and
importers for Evernote (`.enex`) and Obsidian vaults. Your
notes stay plain text; the engine makes them answer questions.

This package binds cuj into Python. The first release exposes
the vault's query surface — the library twin of the `cuj q`
command, running [Quarb](https://quarb.org) over the vault
in-process:

```python
import cuj

# The open todo list, due dates as data.
cuj.q("//*<todo && open> | rec(::description, ::due)")
# [{'description': 'buy primus spares', 'due': None},
#  {'description': 'renew NZ transit visas',
#   'due': datetime.datetime(2026, 6, 15, ...)}, ...]

# Who cites note 3 - and who contradicts it.
cuj.q("/default/3<-cites::summary")
cuj.q("/default/3<-contradicts::summary")

# Notes sharing a tag with note 14 but not linked to it.
cuj.q("/default/14 . <-tagged->tagged"
      "[::::id != (1)::::id][!--*[::::id = (1)::::id]]::summary")
```

Node results return as cuj's own display strings
(`--default--12`, `///wiki/note.md`, `..tag`); value results as
typed Python values — `int`, `float`, tz-aware `datetime`,
`timedelta`, dicts for records, quantities as their written
face (`"204.808 kB"`).

Keyword arguments mirror the CLI's flags:

```python
cuj.q(expr,
      config="~/vaults/field/config.toml",  # else CUJ_CONFIG / ~/.cuj
      profile="all",                        # widen across profiles
      at="~8",                              # a historical state
      now="2026-07-31T12:00:00Z")           # pin now() - reproducible
```

Errors raise `ValueError` with cuj's or the engine's message.

The vault itself is made with the
[`cuj` CLI](https://crates.io/crates/cuj) (`cargo install cuj`)
— capture, import, label, promote; reading is the engine's job,
actions are the tool's. The data model and protocol are
documented from [cuj.net](https://cuj.net); the query language
in the [Quarb spec](https://quarb.org/spec/latest).

License: MIT OR Apache-2.0.
