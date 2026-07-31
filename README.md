# cuj-rs

Rust implementation of the **cuj** protocol (1.0-draft.5
line): a
note-jotting and journaling CLI whose annotation language is
[Jot Syntax](https://jotsyntax.org) and whose storage is a
[metatheca](https://pantheca.org) vault with the taxopsis /
logopsis / semopsis retrieval extensions. Crate and binary are
both named `cuj`.

The normative protocol lives in the sibling `spec` repo; this
implementation targets it directly:

- jots are metatheca entries; integer jot IDs are alias facts
  (`cuj/jot` + latest-wins `cuj/id`), allocated by a
  `cuj/counter` fact on the profile entry, serialized by head
  CAS
- references resolve to entries at write time, so the reference
  graph survives moves; deletion is a path retraction (no
  tombstones, no redirects — history lives behind `--at`)
- parsing via the `jots` crate; parsed snapshots land as
  `jot/index` facts, the `label`/`link`/`todo` overlay as
  `cuj/applied` (content never rewritten by metadata ops)
- retrieval is all substrate: `find` and `backlinks` compile to
  taxopsis filters over the generic `metatheca-docs:1` document
  extractor; `search` is logopsis BM25; `similar` /
  `search --semantic` is semopsis (deterministic `feathash`
  embedder by default); `grep` stays an unindexed scan
- **cuj keeps no index of its own**: fact-shaped reads are
  metatheca fact queries (metatheca SPEC §5.7); there is no
  cuj-owned database file at all
- every read supports `--json`; most support `--at <stateref>`
  time travel; `export` materializes a working tree at any
  state

## Status

0.1.0-dev — full command surface implemented against protocol
0.5 draft, on metatheca 0.1.3 and taxopsis 0.1.1.

Measured at 10,000 jots (release build): warm `show` / `find` /
`backlinks` / `search` 110–140 ms, `create` ~130 ms, `list`
(10k summary lines) ~350 ms. After a write, the first
index-backed query pays that extension's reindex: taxopsis
~0.7 s, logopsis ~2 s, semopsis ~4 s (Phase 1 reindexes are
O(corpus); `--stale` skips the wait and says so).

## Build and test

```
cargo build
cargo test
```

Dependencies on `jots`, `metatheca`, `taxopsis`, `logopsis`,
and `semopsis` are path dependencies on sibling checkouts until
those crates publish.

## Quick start

```
cuj init
cuj -- "capture the ..urgent idea ::work::alpha"
cuj list
cuj find ..urgent
cuj search "idea"
cuj --1
```

## Command surface

Creating and editing:

- `cuj init [<profile>]` — create the vault, or add a profile
  to an existing one.
- `cuj [--md|--txt|--tex|--html] -- <text>...` — create a jot
  from the argument text (extension override optional).
- `cuj edit --<id>` — round-trip the content through `$EDITOR`.
- `cuj rm --<id>` — delete (a path retraction; history keeps
  everything, `--at` still sees it).
- `cuj mv [--from <p>] --to <p> --<id>` — move a jot between
  profiles; it gets the target's next ID, references survive.

Reading:

- `cuj --<id>` / `cuj show --<id>[#bm|:N-M]` — print a jot: id,
  created/modified times, labels, refs, attachments, todos,
  content. Suffixes extract a bookmark fragment or line range;
  `--expand` transcludes suffix-carrying references inline.
- `cuj list` — live jots of the active profile (`-k` limits).
- `cuj find [..tag] [::cat] [--since D] [--until D] [--todo]
  [--filter EXPR]` — typed filtering via taxopsis; `::cat` is a
  prefix match, `--filter` takes a raw taxopsis expression.
- `cuj query <name>` / `cuj query save <name> <find args...>` /
  `cuj query rm <name>` / `cuj queries` — saved find queries.
- `cuj today [<date>]` — the day's jots and due todos;
  `cuj agenda [--until D]` — open todos, dated first.
- `cuj grep [filters --] <pattern>` — index-free regex scan of
  content and attachment extractions.
- `cuj search <query>` — ranked search with match-derived
  snippets: hybrid (BM25 + semantic, fused by zetetes) when the
  vault carries the fusion chain, `--lexical` forces BM25.
- `cuj similar --<id>` / `cuj search --semantic <text>` —
  embedding nearest-neighbour search.
- `cuj backlinks --<id>|//<path>|///<space>/<path>` /
  `cuj references --<id>` — inbound / outbound references.
- `cuj graph [--dot]` — the reference graph (jots + files) as
  JSON or Graphviz DOT.
- `cuj q '<expr>'` — a [Quarb](https://quarb.org) query over
  the vault as an arbor: profiles/jots (with todo and bookmark
  item nodes, and attachments) and space/file subtrees as the
  hierarchy, plus `/cats` (the category hierarchy as a tree)
  and `/tags`; reference edges as `->` / `<-` crosslinks,
  **typed by their Jot Syntax annotation** — `--42::cites`
  walks as `->cites` / `<-cites`, edge tags and the full
  category read through `$-::tags` / `$-::category`; tags are
  traits (`//*<urgent>`), the effective index projects as
  properties (`[::todos *= budget]::summary`), bare `::` is
  the content (a bookmark's is its fragment), timestamps are
  typed instants (`[::;modified > '2026-01-01']`, todo
  `::due`), sizes byte quantities, `<orphan>` /
  `::;n-backlinks` for link hygiene. Honors `--at`. Like
  `grep`, it reads the fact view directly and is never stale.
- `cuj history --<id>` — the jot's fact history;
  `cuj log [-n N] [--since D] [--until D]` — vault states.

File spaces (observed note trees) and capture:

- `cuj space add <name>` + `cuj config set space.<name> <dir>` —
  register a space and bind it to a local directory.
- `cuj observe [<space>]` — snapshot the tree into the vault:
  observed files become referenceable (`//path`,
  `///space/path`), searchable, and classifiable file-jots,
  while remaining ordinary files you edit with ordinary tools.
- `cuj files [<space>]`, `cuj spaces`, `cuj space rm <name>`,
  `cuj show ///<space>/<path>`.
- `cuj ingest` — drain `<vault>/inbox/`: any transport that can
  drop a file (phone sync, e-mail hook, shell) queues a
  capture; one jot per file, mtime as creation time,
  `inbox/<profile>/` routes to that profile.

Templates:

- `cuj template set <name> [--md|...] -- <text>` — store a
  template (`{{text}}`, `{{date}}`, `{{datetime}}`
  placeholders); `cuj new <template> [<text>...]` instantiates
  it through the normal create path. `cuj templates`,
  `cuj template show|rm <name>`.

Metadata (overlay; content is never rewritten):

- `cuj label --<id>... <..tag|::cat>...` (alias `apply`).
- `cuj link --<src> --<tgt>` — add a reference edge.
- `cuj todo [--<id>...] ??<action> [description]` — add a todo
  (with no target: create a todo-jot).
- `cuj todo done --<id> <match>` — complete a todo by ordinal
  (as `show` numbers them), raw construct, or description.

Taxonomy mobility:

- `cuj promote ..tag` — rewrite a tag to a category across the
  profile; `cuj promote ::category` — move matching jots into a
  new profile named after the category's last segment.
- `cuj demote ::category` — category back to tag.

Profiles and configuration:

- `cuj profile [show <name>]`, `cuj profiles`,
  `cuj switch <name>`.
- `cuj ext [set --md|--txt|--tex|--html]` — the profile's
  default extension.
- `cuj config` / `cuj config get <key>` /
  `cuj config set [--vault] <key> <value>` — client keys
  (`vault`, `active_profile`, `editor`, `extract.<ext>`) or
  vault-profile keys (`default_ext`, `jots_frozen`,
  `exclusive`, `required`, `default_space`, `parse.<flag>`).
  Unknown keys and malformed values are errors. Client keys
  also include `space.<name>` (local space directories).

Attachments:

- `cuj attach <file>... --<id>` — attach files (re-attaching a
  filename replaces it); text extraction runs per the client's
  `extract.<ext>` tool map, making attachment text searchable.
- `cuj detach <name>... --<id>`, `cuj attachments --<id>`.

Maintenance:

- `cuj reindex` — rebuild the derived indexes.
- `cuj reparse` — re-derive `jot/index` from current content
  for every live jot and observed file; the migration step
  after a cuj upgrade extends the parsed snapshot (edge
  annotations), and it re-resolves references that were
  unresolved when written. Idempotent.
- `cuj export <dir>` — materialize the jot/attachment tree
  (combine with `--at` for a historical tree).
- `cuj fsck` — substrate checks plus cuj-level invariants.
- `cuj migrate` — move a 0.4 vault to the current protocol (one
  committed fact; 0.4 vaults are otherwise served as-is).
- `cuj import ... <file.enex>... | <dir>` — see below.

Global flags: `--profile <name|all>` (`all` is read-only),
`--json` (reads only; errors become
`{"error":{"code","message"}}`), `--at <stateref>` (historical
reads), `--force` (bypass `required` constraints), `--stale`
(serve possibly-stale index results), `-k <n>`.

## Exit codes

Every failure maps to a stable exit code (documented here as
SPEC §11.3 requires; the names are the `code` values of the
`--json` error envelope):

- `0` — success
- `2` — `Usage` (command-line usage error)
- `3` — `NotFound` (jot reference, profile, attachment
  filename, bookmark, or todo match did not resolve)
- `4` — `Deleted` (the reference resolves to a deleted jot)
- `5` — `ConstraintViolation` (an `exclusive`/`required`
  constraint check failed)
- `6` — `Frozen` (content rewrite on a frozen profile)
- `7` — `Stale` (an index-backed query could not be served at
  the requested state)
- `8` — `Conflict` (concurrent writer, CAS retries exhausted)
- `9` — `Unsupported` (the vault's protocol is not supported)
- `1` — `Substrate` / `Io` (anything below cuj: metatheca,
  the extensions, jots, or the filesystem)

## Library use

The binary is a thin renderer over the `cuj` library crate,
which is the intended substrate for other frontends (the `cui`
TUI browser; a web backend):

- `cuj::queries` — every read as a data-returning function:
  `JotView` (the SPEC §11.2 jot object as a struct), paginated
  `list`/`search`/`similar` (offset + limit) and
  `find`/`backlinks` (limit + resumption cursor), structured
  `grep`/`history`/`fsck` results.
- `cuj::App` — the write surface: `create`, `label`, `link`,
  `todo`, `todo_done`, `rm`, `mv`, `promote_*`, `attach`,
  `detach`, `update_profile`, and `update_content` (the
  non-interactive content write behind `edit`, for consumers
  that cannot shell out to `$EDITOR`). All writes are pinned
  head-CAS commits with bounded reread-and-retry.
- `cuj::reads::Reads` — the raw fact view when a consumer wants
  to build its own read model.

## Import

`cuj import <dir>` ingests a notes directory: each `.md` /
`.txt` / `.tex` / `.html` file becomes one jot (content
verbatim, extension preserved, mtime as creation time), with
`sha256(relpath \0 content)` fingerprints making re-imports
no-ops. To *cohabit* with a live notes tree instead of
migrating it, register it as a space and `observe` it.

`cuj import` also ingests Evernote `.enex` export files (File →
Export from the native apps):

```
cuj import --dry-run *.enex
cuj import --category ::evernote *.enex
```

- Notes land as `.html` jots — ENML is an XHTML dialect, so the
  conversion is lossless: only `<en-note>` / `<en-todo>` /
  `<en-media>` and the XML prolog are rewritten, everything else
  passes through byte-for-byte. The title becomes an `<h1>`, a
  web-clipper source URL becomes a `Source:` link under it.
- IDs are assigned in ascending `<created>` order across all
  files given in one invocation, so jot IDs are chronological.
  `<created>` becomes the jot's birth timestamp.
- Evernote tags are sanitized to the jots tag alphabet
  (`Reading List` → `reading_list`) and applied as overlay
  labels, never written into content; `--category ::cat`
  (repeatable) adds overlay categories the same way.
- Attachments become attachment entries (filenames sanitized to
  the jots `//ref` grammar); Evernote's image-recognition text
  becomes a `cuj/extraction`, so `cuj search` sees OCR words.
- Checkboxes become live todos (`??todo …` / `??.todo …`)
  manageable via `cuj todo`; `--freeze-todos` renders them as
  inert `☐` / `☑` instead.
- Every imported jot carries a `cuj/import` fact (source file +
  content fingerprint); re-importing the same notes is a no-op,
  which also makes interrupted bulk imports safely resumable.
  A note's import is one vault commit — there are no partial
  notes.
