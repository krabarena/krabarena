from __future__ import annotations

from collections.abc import Iterator
from urllib.parse import parse_qsl, urlencode, urlparse, urlunparse

import pytest
from playwright.sync_api import Browser, BrowserContext, Page
from playwright.sync_api import Error as PlaywrightError

from helpers.config import resolve_base_url
from helpers.results import (
    TestArtifacts,
    create_test_artifacts,
    finalize_artifact_run,
    initialize_artifact_run,
)

MAX_DIAGNOSTIC_ITEMS = 20
SENSITIVE_QUERY_KEYS = {
    "access_token",
    "auth",
    "code",
    "id_token",
    "jwt",
    "refresh_token",
    "session",
    "state",
    "token",
}


def pytest_sessionstart(session: pytest.Session) -> None:
    option_value = session.config.getoption("--base-url", default=None)
    initialize_artifact_run(base_url=resolve_base_url(option_value))


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    finalize_artifact_run(exitstatus=exitstatus)


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(
    item: pytest.Item,
    call: pytest.CallInfo[object],
) -> Iterator[None]:
    outcome = yield
    report = outcome.get_result()
    setattr(item, f"rep_{report.when}", report)


@pytest.fixture(scope="session")
def base_url(pytestconfig: pytest.Config) -> str:
    option_value = pytestconfig.getoption("--base-url", default=None)
    return resolve_base_url(option_value)


@pytest.fixture
def e2e_artifacts(
    request: pytest.FixtureRequest,
    base_url: str,
) -> Iterator[TestArtifacts]:
    artifacts = create_test_artifacts(request.node.nodeid, base_url=base_url)
    yield artifacts

    report = getattr(request.node, "rep_call", None)
    if report is None:
        return

    status = "failed" if report.failed else "skipped" if report.skipped else "passed"
    error_text = report.longreprtext if report.failed else None
    artifacts.write_result(status=status, error_text=error_text)


@pytest.fixture
def context(
    browser: Browser,
    request: pytest.FixtureRequest,
    e2e_artifacts: TestArtifacts,
) -> Iterator[BrowserContext]:
    context = browser.new_context()
    trace_started = False

    try:
        context.tracing.start(
            screenshots=True,
            snapshots=True,
            sources=True,
            title=e2e_artifacts.test_id,
        )
        trace_started = True
        yield context
    finally:
        failed = _test_failed(request.node)

        if trace_started:
            try:
                if failed:
                    e2e_artifacts.failure_dir.mkdir(parents=True, exist_ok=True)
                    context.tracing.stop(path=str(e2e_artifacts.trace_path))
                else:
                    context.tracing.stop()
            except PlaywrightError as exc:
                e2e_artifacts.add_runtime_data("trace_error", str(exc))

        try:
            context.close()
        except PlaywrightError as exc:
            e2e_artifacts.add_runtime_data("context_close_error", str(exc))


@pytest.fixture
def page(
    context: BrowserContext,
    request: pytest.FixtureRequest,
    e2e_artifacts: TestArtifacts,
) -> Iterator[Page]:
    page = context.new_page()
    _attach_page_diagnostics(page, e2e_artifacts)
    e2e_artifacts.set_current_url(page.url)

    try:
        yield page
    finally:
        e2e_artifacts.set_current_url(page.url)

        if _test_failed(request.node):
            try:
                e2e_artifacts.failure_dir.mkdir(parents=True, exist_ok=True)
                page.screenshot(path=str(e2e_artifacts.screenshot_path), full_page=True)
            except PlaywrightError as exc:
                e2e_artifacts.add_runtime_data("screenshot_error", str(exc))

        try:
            page.close()
        except PlaywrightError as exc:
            e2e_artifacts.add_runtime_data("page_close_error", str(exc))


def _test_failed(node: pytest.Node) -> bool:
    report = getattr(node, "rep_call", None)
    return bool(report and report.failed)


def _attach_page_diagnostics(page: Page, artifacts: TestArtifacts) -> None:
    page.on("console", lambda message: _record_console_error(artifacts, message))
    page.on(
        "pageerror",
        lambda error: _append_diagnostic(
            artifacts,
            "page_errors",
            {"message": str(error)},
        ),
    )
    page.on(
        "requestfailed",
        lambda request: _append_diagnostic(
            artifacts,
            "failed_requests",
            {
                "method": request.method,
                "url": _safe_url(request.url),
                "failure": str(request.failure or ""),
            },
        ),
    )
    page.on("response", lambda response: _record_http_error(artifacts, response))


def _record_console_error(artifacts: TestArtifacts, message) -> None:
    if message.type != "error":
        return

    _append_diagnostic(
        artifacts,
        "console_errors",
        {
            "text": message.text,
            "location": message.location,
        },
    )


def _record_http_error(artifacts: TestArtifacts, response) -> None:
    if response.status < 400:
        return

    _append_diagnostic(
        artifacts,
        "http_errors",
        {
            "status": response.status,
            "url": _safe_url(response.url),
        },
    )


def _append_diagnostic(
    artifacts: TestArtifacts,
    key: str,
    value: dict[str, object],
) -> None:
    items = artifacts.runtime_data.setdefault(key, [])
    if not isinstance(items, list):
        items = []
        artifacts.runtime_data[key] = items

    items.append(value)
    del items[:-MAX_DIAGNOSTIC_ITEMS]


def _safe_url(url: str) -> str:
    parsed = urlparse(url)
    if not parsed.query:
        return url

    safe_query = [
        (key, "[redacted]" if key.lower() in SENSITIVE_QUERY_KEYS else value)
        for key, value in parse_qsl(parsed.query, keep_blank_values=True)
    ]
    return urlunparse(parsed._replace(query=urlencode(safe_query)))
