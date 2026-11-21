from __future__ import annotations

from urllib.parse import urlparse, urlunparse

from playwright.sync_api import Page, Response

from helpers.config import target_url


def open_target_page(
    page: Page,
    path: str = "/",
    *,
    base_url: str | None = None,
    wait_until: str = "domcontentloaded",
    timeout_ms: int = 30_000,
) -> Response | None:
    return page.goto(
        target_url(path, base_url=base_url),
        wait_until=wait_until,
        timeout=timeout_ms,
    )


def normalize_url(url: str) -> str:
    parsed = urlparse(url)
    return urlunparse(
        (
            parsed.scheme.lower(),
            parsed.netloc.lower(),
            parsed.path or "/",
            "",
            parsed.query,
            "",
        )
    )
