from typing import Any, Optional

__version__: str

def q(
    expr: str,
    *,
    config: Optional[str] = None,
    profile: Optional[str] = None,
    at: Optional[str] = None,
    now: Optional[str] = None,
) -> list[Any]: ...
