"""cuj, the plain-text PKM, from Python.

cuj (https://cuj.net) is a note-jotting engine: plain-text jots
with inline Jot Syntax annotations - tags, hierarchical
categories, labeled references, todos with due dates - on a
content-addressed fact store, with Evernote and Obsidian
imports. This package binds it into Python.

This first release exposes the vault's query surface, the
library twin of the ``cuj q`` command::

    >>> import cuj
    >>> cuj.q("//*<todo && open> | rec(::description, ::due)")
    [{'description': 'buy primus spares', 'due': None}, ...]

Node results come back as cuj display strings, value results as
typed Python values (datetimes, timedeltas, dicts for records).
``config=`` points at a config.toml; ``at=`` reads a historical
state; ``now=`` pins the clock for reproducible temporal
queries.
"""

from cuj._cuj import __version__, q

__all__ = ["q", "__version__"]
