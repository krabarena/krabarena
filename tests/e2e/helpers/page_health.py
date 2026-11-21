from __future__ import annotations

from playwright.sync_api import Page, Response, expect

GENERIC_SERVER_ERROR_TEXT = (
    "Internal Server Error",
    "Service Unavailable",
    "Bad Gateway",
    "Gateway Timeout",
)

CLIENT_CRASH_TEXT = (
    "Application error",
    "Hydration failed",
    "Unhandled Runtime Error",
    "Something went wrong",
)


def assert_successful_response(response: Response | None) -> None:
    assert response is not None, "expected page navigation to return a response"
    assert response.status < 500, f"expected non-5xx response, got {response.status}"


def assert_page_has_visible_body(page: Page) -> None:
    body = page.locator("body")
    expect(body).to_be_visible()
    assert body.inner_text().strip(), "expected visible body content"


def assert_no_generic_server_error(page: Page) -> None:
    body_text = page.locator("body").inner_text()
    for marker in GENERIC_SERVER_ERROR_TEXT:
        assert marker not in body_text, f"page shows generic server error: {marker}"


def assert_no_client_side_crash(page: Page) -> None:
    body_text = page.locator("body").inner_text()
    for marker in CLIENT_CRASH_TEXT:
        assert marker not in body_text, f"page shows client-side crash marker: {marker}"


def assert_page_is_healthy(page: Page, response: Response | None) -> None:
    assert_successful_response(response)
    assert_page_has_visible_body(page)
    assert_no_generic_server_error(page)
    assert_no_client_side_crash(page)
