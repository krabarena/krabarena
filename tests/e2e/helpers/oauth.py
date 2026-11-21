from __future__ import annotations

from urllib.parse import urlparse

from playwright.sync_api import Error as PlaywrightError
from playwright.sync_api import Page

from helpers.results import TestArtifacts

EXPECTED_PROVIDER_HOSTS = {
    "google": ("accounts.google.com",),
    "github": ("github.com",),
}

CONFIG_ERROR_MARKERS = (
    "invalid_client",
    "redirect_uri_mismatch",
    "unauthorized_client",
    "disabled",
    "incorrect callback",
    "callback url",
    "bad verification code",
    "oauth app",
)


def assert_provider_redirect(
    page: Page,
    *,
    provider: str,
    artifacts: TestArtifacts,
) -> None:
    current_url = page.url
    parsed = urlparse(current_url)
    host = parsed.netloc.lower()
    expected_hosts = EXPECTED_PROVIDER_HOSTS[provider]

    artifacts.add_runtime_data("provider_url", current_url)
    artifacts.add_runtime_data("provider_host", host)

    assert any(_host_matches(host, expected) for expected in expected_hosts), (
        f"expected {provider} OAuth provider host, got {host!r} at {current_url!r}"
    )

    body_text = _visible_body_text(page)
    lower_url = current_url.lower()
    lower_body = body_text.lower()

    for marker in CONFIG_ERROR_MARKERS:
        if marker in lower_url or marker in lower_body:
            artifacts.add_runtime_data("provider_error_marker", marker)
            artifacts.add_runtime_data("provider_error_text", body_text[:1_000])
            raise AssertionError(
                f"{provider} OAuth page shows configuration error marker {marker!r}"
            )

    if "client_id" in lower_body and _looks_like_client_id_error(lower_body):
        artifacts.add_runtime_data("provider_error_marker", "client_id")
        artifacts.add_runtime_data("provider_error_text", body_text[:1_000])
        raise AssertionError(
            f"{provider} OAuth page shows client_id configuration error"
        )


def _host_matches(host: str, expected: str) -> bool:
    return host == expected or host.endswith(f".{expected}")


def _visible_body_text(page: Page) -> str:
    try:
        return page.locator("body").inner_text(timeout=5_000)
    except PlaywrightError:
        return ""


def _looks_like_client_id_error(body: str) -> bool:
    return any(marker in body for marker in ("error", "invalid", "missing"))
