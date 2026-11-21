from __future__ import annotations

import os
from urllib.parse import urljoin, urlparse

DEFAULT_BASE_URL = "https://krabarena.com"
PRODUCTION_BASE_URL = DEFAULT_BASE_URL
BASE_URL_ENV_VAR = "KRAB_E2E_BASE_URL"


def resolve_base_url(override: str | None = None) -> str:
    candidate = override or os.environ.get(BASE_URL_ENV_VAR) or DEFAULT_BASE_URL
    candidate = candidate.strip().rstrip("/")
    parsed = urlparse(candidate)

    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError(
            f"invalid E2E base URL {candidate!r}; expected absolute http(s) URL"
        )

    return candidate


def target_url(path: str = "/", base_url: str | None = None) -> str:
    if not path:
        path = "/"

    if not path.startswith("/"):
        path = f"/{path}"

    return urljoin(f"{resolve_base_url(base_url)}/", path.lstrip("/"))


def production_url(path: str = "/") -> str:
    return target_url(path, base_url=PRODUCTION_BASE_URL)
