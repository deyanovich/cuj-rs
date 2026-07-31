# Changelog

## Unreleased (0.3.2)

### Changed

- **Protocol versioning renamed: the cuj protocol is
  `1.0-draft.5`** (formerly numbered 0.5 — the draft counter
  continues that line; the quarb spec convention, adopted after
  the crates.io page showed "protocol 0.5" beside the crate's
  own 0.3.x and read as two competing versions). `cuj init`
  writes the new marker; a `0.5` marker is the same revision's
  former spelling — full surface, no gating — and `cuj migrate`
  normalizes it in one committed fact. `0.4` handling is
  unchanged. SPEC §5.13 updated alongside.
- Crate description reworded so the protocol line arrives as a
  trailing parenthetical, not a leading number.

### Fixed

- The libcuj sdist no longer fails on duplicate license files
  (maturin auto-includes workspace-root licenses into member
  sdists; the root pyproject now points at cuj-py's copies).

## 0.3.1 — 2026-07-31

### Added

- **PyPI `cuj`**: the CLI itself, prebuilt — a bin-bindings
  wheel of this crate (the quarb-full mechanism, on the base
  name: cuj is more likely wanted as a command than a
  library). `pip install cuj` puts the complete binary on
  PATH; unix wheels only (the substrate's flock locking).
- **PyPI `libcuj`** (cuj-py workspace member, maturin/PyO3,
  abi3 ≥3.8): the Python library, `import cuj`. First release
  exposes the vault's query surface — `cuj.q(expr, config=,
  profile=, at=, now=)`, the library twin of `cuj q`: node
  results as cuj display strings, value results as typed
  Python values (datetimes, timedeltas, dicts for records).
  The two distributions install side by side: the CLI wheel
  ships no Python module, the library wheel no script. (The
  bindings briefly shipped as PyPI `cuj` 0.3.0 before the
  naming ruling; that upload is superseded and yanked.)
  Both publish from the `py/v*`-tag GitHub workflow (OIDC
  trusted publishing).
- `ClientConfig::load_from(path)`: load a config from an
  explicit path, bypassing `CUJ_CONFIG` and the default
  location — the door for library consumers.

## 0.3.0 — 2026-07-31

### Added

- `--now <instant>`: pin the `now()` instant in `cuj q` for
  reproducible temporal queries (mirrors `qua --now`). Unset,
  `cuj q` now binds one startup clock read — previously the
  invocation instant was left unbound and `now()` silently
  matched nothing.

### Fixed

- `cuj import` now prints the per-note drop report (`Dropped in
  "note.md": property rating, aliases (1)`) alongside the
  unresolved-wikilink line; previously the drops were recorded
  in the `--json` report but silent at the human surface.
- `::summary` and search snippets on `.html` jots read through a
  tag-stripped text shadow instead of echoing the raw
  `<html><head>…` preamble — an imported Evernote note now
  summarizes as its heading.

### Changed

- Track Quarb 0.16 (from 0.10): `cuj q` and `cuj quarb-serve`
  run against the current engine and serve protocol. The
  `AstAdapter` implementation again needed no changes; the
  win is on the query side — `cuj q` now parses the 0.11–0.16
  language surface, notably `::::` (the canonical
  adapter-metadata spelling), the headless both-direction
  crosslink `--label`, resolution spellings `-->` / `<--`,
  fragment splices in predicate position, and the text-level
  koine. Query results over a vault arbor are unchanged.

## 0.2.0 — 2026-07-25

### Changed

- Track Quarb 0.10 (from 0.4): `cuj q` and `cuj quarb-serve`
  run against the current engine and serve protocol. The
  `AstAdapter` implementation needed no changes — the trait
  additions across 0.5–0.10 are all defaulted and the
  implemented method signatures are stable; the query results
  over a vault arbor are unchanged.
- Substrate dependencies are now consumed from crates.io
  (published jots, metatheca, taxopsis/logopsis/semopsis/
  zetetes, quarb, quarb-serve) rather than sibling path
  checkouts, so a cuj-rs checkout builds standalone.

## 0.1.0 — 2026-07-21

### Quarb binding

- `cuj q '<expr>'` runs a Quarb query over the vault as an
  arbor (informative SPEC appendix): tree of profiles → jots →
  attachments plus space/file subtrees, tags as traits,
  effective-index members as properties, reference and file
  edges as `->`/`<-` crosslinks, `--at` for historical states.
  New `arbor` module implements `quarb::AstAdapter` over the
  fact view.
- Arbor v2 — the semantic surface. Jot Syntax edge annotations
  are materialized in `jot/index` (`references` /
  `cross_file_references` / `file_references` / `resources` /
  `inter_jot_resources` carry `category` / `tags` / specifier
  fields; bare file refs and resources still wire as plain
  strings; inter-jot resources gain `space` and a write-time
  resolved `entry`; todos gain `completed`; old facts read
  unchanged) and drive **typed edges**: `--42::cites` walks as
  `->cites` / `<-cites` (`ref` / `file` / `resource` when
  bare), with `$-::category` / `$-::tags` / `$-::as` /
  `$-::bookmark` / `$-::fragment` on the edge. The effective
  index unions edges by (entry, category), so `--42::cites`
  and `--42::uses` are distinct edges.
- New arbor surfaces: `/cats` (the category hierarchy as a
  tree; jots link `->cat`, so `/cats/work//*<-cat` sweeps a
  subtree), `/tags` (`->tagged`, `::;n-jots`), todo item nodes
  (`::action`, `::importance`, typed `::due` / `::completed`,
  `<todo>` + `<open>`/`<done>`), bookmark nodes (bare `::` is
  the fragment `cuj show --N#name` prints), `<orphan>`,
  `::;n-links` / `::;n-backlinks`.
- `cuj quarb-serve` exposes the vault arbor over the Quarb
  serve protocol (stdin/stdout JSON), so `qua` mounts the
  vault beside any other source — a repo, a mailbox, a
  database — without linking cuj:
  `qua '...' serve:'cuj quarb-serve' git:.` is one query over
  jots and commits.
- `cuj reparse` — re-derive `jot/index` from current content
  (idempotent); the migration that back-fills annotations on
  existing vaults and re-resolves once-unresolved references.
- Typed values in the arbor: `::;created` / `::;modified` /
  `::;added` and todo `::due` mint Quarb instants, `::;size`
  a byte quantity. **Breaking for `cuj q` consumers**: the
  `created_ns` / `modified_ns` / `added_ns` metadata keys are
  gone (compare instants directly), `::;size` now displays
  with a unit (`14 B`), `//*<todo>` also matches todo item
  nodes (use `<jot && todo>` / `<todo && open>` to
  disambiguate), and `<-*` backlinks include `<-tagged` from
  tag nodes.

### Protocol 0.5

- File spaces and file-jots: `space add/rm`, `spaces`,
  `observe`, `files`, `show ///space/path`; observed files
  join references (`//path`, `///space/path`, resolved at read
  time), classification, search, grep, and backlinks.
- Capture inbox: `ingest` drains `<vault>/inbox/` — one jot
  per dropped file, mtime as creation time,
  `inbox/<profile>/` routing, per-capture error isolation.
- Templates (`template set/show/rm`, `templates`, `new`) with
  `{{text}}`/`{{date}}`/`{{datetime}}` substitution; saved
  queries (`query save/rm`, `query <name>`, `queries`).
- Hybrid retrieval: vault init creates a zetetes fusion chain;
  `search` serves the fused ranking by default, `--lexical`
  forces BM25; hits carry an `engine` field.
- Transclusion: `show --expand` inlines suffix-carrying
  reference fragments, depth-bounded and cycle-guarded.
- `today` and `agenda` day/todo views; `graph [--dot]` export.
- `import <dir>`: notes-directory binding with path+content
  fingerprints.
- `migrate` bumps a 0.4 vault's protocol marker; 0.4 vaults
  are served as-is, 0.5-only writes answer Unsupported until
  migrated. `space` becomes a reserved profile name.
- Obsidian space dialect (SPEC §8.5):
  `space add <name> --dialect obsidian` declares it; `observe`
  parses YAML frontmatter, inline `#tags` (nested tags become
  `::` categories), `[[wikilinks]]` / `![[embeds]]` with
  in-space resolution, and checkbox tasks into
  `obsidian/index` facts that join the file-jot effective
  index, `find`, `backlinks`, and `graph`. A root `.obsidian/`
  selects the Obsidian binding for `import <dir>`
  (`--dialect obsidian|none` forces): tags/categories land in
  the overlay, wikilinks become entry-resolved overlay
  references, non-note embeds become attachments, checkboxes
  become overlay todos.

### Fixed

- `--profile all` widens `templates` and `queries` listings
  (profile-qualified in text, `profile` field in JSON) instead
  of silently narrowing to the active profile; `query <name>`
  and `template show <name>` under `all` run a unique holder
  and refuse ambiguous names.
- `export` refuses filesystem names from vault data that could
  escape the export dir (separators, dot-dots in a filename,
  profile, or extension — reachable only from a vault written
  by another implementation).
- A content blob referenced by the vault's own facts going
  missing surfaces as substrate corruption (exit 1), not as a
  user-facing NotFound (metatheca get_bytes now answers
  Integrity for it).
- `fsck` sweeps every live path against the projections and
  flags bindings no read can reach (e.g. a jot path whose entry
  has no `cuj/id`) — previously such dark data passed silently.
- JSON jot objects emit one `references` element per effective
  edge (SPEC 5.5): typed edges to one target appear once each,
  carrying an optional `category` field and their own written
  form (`as`) — no more first-wins fold hiding edges or serving
  a stale written form. `link` is a no-op only when a BARE edge
  to the target already exists; typed edges no longer suppress
  it.
- `cuj todo` with no leading ref refuses ref-shaped tokens after
  the construct as ambiguous (a misplaced target and a reference
  in the todo text read identically), pointing at both correct
  spellings; with a leading ref, embedded references pass.
- Import and observe memory is bounded by one file, not the
  tree: the walk collects paths only, text files read one at a
  time, and binaries (observed files, Obsidian embeds) stream
  into the vault through metatheca's new streaming blob deposit;
  `export` streams attachments back out. Observe change
  detection compares blob hashes instead of reading old content.
- `find` and `grep` now include file-jots per SPEC §8.6:
  `find` matches their tags and categories in any profile
  scope (scoping filters post-query, `o:<n>` offset cursors),
  and `grep` scans observed textual files. Grep JSON: jot hits
  are unchanged; file hits carry `space`/`path` instead of
  `profile`/`id`.
- Construct rewriting no longer corrupts prefixes of longer
  constructs: `demote ::work` leaves `::work::alpha` alone,
  and `todo done` with a raw match that prefixes another
  construct rewrites the parsed todo span, not the first
  textual hit.
- `reparse` no longer severs durable references to jots that
  were moved with `mv`.
- Write and maintenance commands are rejected under `--at`
  (`space add/rm`, `observe`, `migrate`, `reparse`, `init`,
  `ext set`, `config set --vault`, `template set/rm`,
  `query save/rm`).
- `mv` honors a profile-qualified ref; `rm`/`edit`/`mv`
  reject `#bookmark`/`:lines` suffixes.
- `edit` with an unchanged buffer commits nothing, so it no
  longer reverts concurrent writes; editor and extraction
  temp files live in private random tempdirs.
- `show --expand` no longer panics on malformed fragment
  specifiers in content.
- `find ::cat` matches categories on segment boundaries —
  `::work` no longer matches `::workshop`.
- `today`, `agenda`, and `find --since/--until` interpret
  date-only bounds in local time; `agenda --until` keeps
  undated todos; template `{{date}}`/`{{datetime}}` are
  local.
- `show` on deleted/moved jots exits non-zero with the
  standard error envelope; `references --json` emits all four
  partitions (JSON shape change: top-level object, not
  array).
- `similar --N` excludes the query jot; `search foo
  --lexical` parses the mode flag anywhere; `promote ::cat`
  refuses an existing target profile; `graph` and `observe`
  degrade instead of panicking on integrity violations;
  `quarb-serve` answers out-of-range node ids instead of
  panicking.
- Evernote import: ENML active content (`script`, `on*`
  attributes, `javascript:` URLs) is sanitized; the dedup
  fingerprint includes tags and resources (legacy
  fingerprints still dedup); Obsidian embed filenames are
  sanitized (NFC) and deduped per note.

### Protocol 0.4

Initial implementation of cuj protocol 0.4 on the jots parser
and the metatheca vault with the taxopsis / logopsis / semopsis
retrieval extensions.

- Full protocol 0.4 command surface: init, create, show (with
  bookmark/line-range fragments), list, find, grep, search,
  similar, label/apply, link, todo, todo done, edit, rm, mv,
  promote/demote, backlinks, references, attach/detach/
  attachments, history, log, profile management, ext, config,
  reindex, export, fsck — plus time-travel reads (`--at`) and
  `--json` on reads.
- Pure-substrate reads: no cuj-owned database; fact-shaped
  reads are metatheca fact queries, retrieval goes through the
  extension chains, staleness contract per SPEC §9.6.
- Writes are parent-pinned head-CAS commits with bounded
  reread-and-retry (SPEC §3.2), so concurrent writers cannot
  collide on jot IDs or overwrite each other's overlays.
- Evernote `.enex` import: chronological IDs across files,
  one commit per note, fingerprint dedup (resumable), overlay
  tags/categories, live or frozen todos, attachments with
  recognition-text extractions, `--dry-run`, JSON report.
- Library layer for other frontends: `queries` (data-returning
  reads with pagination/cursors), `App::update_content`
  (non-interactive content writes), public `Reads` fact view.
