from __future__ import annotations

import html
import json
import re
import shutil
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from helpers.config import PRODUCTION_BASE_URL

E2E_ROOT = Path(__file__).resolve().parents[1]
ARTIFACT_ROOT = E2E_ROOT / ".artifacts"
RESULTS_INDEX = ARTIFACT_ROOT / "results.json"
FAILURES_DIR = ARTIFACT_ROOT / "failures"
PLAYWRIGHT_REPORT_DIR = ARTIFACT_ROOT / "playwright-report"
TEST_RESULTS_DIR = ARTIFACT_ROOT / "test-results"

SUITE_NAME = "web-smoke"
RUNNER = "pytest-playwright"
LANGUAGE = "python"
DEPENDENCY_MANAGER = "uv"
SCHEMA_VERSION = 2
MAX_ERROR_CHARS = 20_000
MAX_TRACE_SUMMARY_STEPS = 20
MAX_DIAGNOSTICS = 20
ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;]*m")


@dataclass
class TestArtifacts:
    __test__ = False

    nodeid: str
    test_id: str
    base_url: str
    current_url: str | None = None
    runtime_data: dict[str, Any] = field(default_factory=dict)
    failure_step: str | None = None
    expected_behavior: str | None = None
    observed_behavior: str | None = None
    error_message: str | None = None

    @property
    def failure_dir(self) -> Path:
        return FAILURES_DIR / self.test_id

    @property
    def trace_path(self) -> Path:
        return self.failure_dir / "trace.zip"

    @property
    def trace_summary_path(self) -> Path:
        return self.failure_dir / "trace-summary.md"

    @property
    def screenshot_path(self) -> Path:
        return self.failure_dir / "screenshot.png"

    @property
    def failure_json_path(self) -> Path:
        return self.failure_dir / "failure.json"

    @property
    def error_text_path(self) -> Path:
        return self.failure_dir / "error.txt"

    def set_current_url(self, url: str | None) -> None:
        self.current_url = url

    def add_runtime_data(self, key: str, value: Any) -> None:
        self.runtime_data[key] = value

    def set_expected_behavior(self, value: str) -> None:
        self.expected_behavior = value

    def set_observed_behavior(self, value: str) -> None:
        self.observed_behavior = value

    def write_result(self, *, status: str, error_text: str | None = None) -> None:
        if status not in {"passed", "failed", "skipped"}:
            raise ValueError(f"unsupported test result status {status!r}")

        ARTIFACT_ROOT.mkdir(parents=True, exist_ok=True)
        clean_error = _clean_error(error_text) if error_text else None
        error_message = _error_message(clean_error) if clean_error else None
        failure_step = self._failure_step() if status == "failed" else None

        if status == "failed":
            self.failure_dir.mkdir(parents=True, exist_ok=True)
            if clean_error:
                self.error_text_path.write_text(clean_error, encoding="utf-8")
            self.trace_summary_path.write_text(
                _trace_summary(
                    test_case=self._test_case_name(),
                    nodeid=self.nodeid,
                    status=status,
                    base_url=self.base_url,
                    current_url=self.current_url,
                    failure_step=failure_step,
                    runtime_data=self.runtime_data,
                    error_text=clean_error,
                ),
                encoding="utf-8",
            )

        payload = self._payload(
            status=status,
            failure_step=failure_step,
            error_message=error_message,
        )

        if status == "failed":
            self.failure_json_path.write_text(
                json.dumps(payload, indent=2, sort_keys=True),
                encoding="utf-8",
            )

        _append_test_case(payload)

    def _payload(
        self,
        *,
        status: str,
        failure_step: str | None,
        error_message: str | None,
    ) -> dict[str, Any]:
        artifacts = {
            "trace": _maybe_artifact_path(self.trace_path),
            "trace_summary": _maybe_artifact_path(self.trace_summary_path),
            "screenshot": _maybe_artifact_path(self.screenshot_path),
            "html_report": _artifact_path(PLAYWRIGHT_REPORT_DIR / "index.html"),
            "failure_packet": (
                _artifact_path(self.failure_dir) if status == "failed" else None
            ),
            "failure_json": (
                _artifact_path(self.failure_json_path) if status == "failed" else None
            ),
            "error_text": _maybe_artifact_path(self.error_text_path),
        }

        observed_behavior = self.observed_behavior
        if status == "failed" and observed_behavior is None:
            observed_behavior = error_message

        return {
            "name": self._test_case_name(),
            "test_id": self.test_id,
            "nodeid": self.nodeid,
            "base_url": self.base_url,
            "status": status,
            "description": self.runtime_data.get("description"),
            "failure_step": failure_step,
            "error_message": error_message,
            "current_url": self.current_url,
            "observed_behavior": observed_behavior,
            "expected_behavior": self.expected_behavior,
            "runtime_data": _json_safe(self.runtime_data),
            "artifacts": artifacts,
        }

    def _test_case_name(self) -> str:
        scenario = self.runtime_data.get("scenario")
        return str(scenario) if scenario else self.test_id

    def _failure_step(self) -> str | None:
        if self.failure_step:
            return self.failure_step

        current_step = self.runtime_data.get("current_step")
        if isinstance(current_step, str) and current_step:
            return current_step

        steps = self.runtime_data.get("steps")
        if isinstance(steps, list) and steps:
            last = steps[-1]
            if isinstance(last, dict):
                name = last.get("name")
                if isinstance(name, str) and name:
                    return name

        return None


def initialize_artifact_run(*, base_url: str) -> None:
    _clear_artifact_paths()
    ARTIFACT_ROOT.mkdir(parents=True, exist_ok=True)
    PLAYWRIGHT_REPORT_DIR.mkdir(parents=True, exist_ok=True)
    TEST_RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    _write_suite(_new_suite(base_url=base_url))


def finalize_artifact_run(*, exitstatus: int = 0) -> None:
    suite = _read_suite()
    if suite is None:
        suite = _new_suite(base_url=PRODUCTION_BASE_URL)

    suite["run_finished_at"] = _now()
    cases = suite.get("test_cases", [])
    has_failed_case = any(
        isinstance(case, dict) and case.get("status") == "failed" for case in cases
    )
    suite["status"] = "failed" if exitstatus != 0 or has_failed_case else "passed"

    _write_suite(suite)
    _write_html_report(suite)


def create_test_artifacts(nodeid: str, *, base_url: str) -> TestArtifacts:
    return TestArtifacts(
        nodeid=nodeid,
        test_id=_safe_test_id(nodeid),
        base_url=base_url,
    )


def _new_suite(*, base_url: str) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "suite": SUITE_NAME,
        "runner": RUNNER,
        "language": LANGUAGE,
        "dependency_manager": DEPENDENCY_MANAGER,
        "production_url": PRODUCTION_BASE_URL,
        "base_url": base_url,
        "run_started_at": _now(),
        "run_finished_at": None,
        "status": "running",
        "test_cases": [],
    }


def _clear_artifact_paths() -> None:
    for path in (FAILURES_DIR, PLAYWRIGHT_REPORT_DIR, TEST_RESULTS_DIR):
        if path.exists():
            shutil.rmtree(path)

    if RESULTS_INDEX.exists():
        RESULTS_INDEX.unlink()


def _append_test_case(payload: dict[str, Any]) -> None:
    suite = _read_suite()
    if suite is None:
        suite = _new_suite(base_url=str(payload.get("base_url") or PRODUCTION_BASE_URL))

    cases = suite.setdefault("test_cases", [])
    if not isinstance(cases, list):
        cases = []
        suite["test_cases"] = cases

    cases.append(payload)
    _write_suite(suite)


def _read_suite() -> dict[str, Any] | None:
    if not RESULTS_INDEX.exists():
        return None

    loaded = json.loads(RESULTS_INDEX.read_text(encoding="utf-8"))
    return loaded if isinstance(loaded, dict) else None


def _write_suite(payload: dict[str, Any]) -> None:
    ARTIFACT_ROOT.mkdir(parents=True, exist_ok=True)
    RESULTS_INDEX.write_text(
        json.dumps(payload, indent=2, sort_keys=True),
        encoding="utf-8",
    )


def _write_html_report(suite: dict[str, Any]) -> None:
    PLAYWRIGHT_REPORT_DIR.mkdir(parents=True, exist_ok=True)
    rows = []
    for case in suite.get("test_cases", []):
        if not isinstance(case, dict):
            continue
        artifacts = (
            case.get("artifacts") if isinstance(case.get("artifacts"), dict) else {}
        )
        links = []
        for label, key in (
            ("packet", "failure_packet"),
            ("trace", "trace"),
            ("summary", "trace_summary"),
            ("screenshot", "screenshot"),
            ("error", "error_text"),
        ):
            value = artifacts.get(key) if artifacts else None
            if value:
                links.append(f'<a href="{_html_rel(value)}">{html.escape(label)}</a>')

        rows.append(
            "<tr>"
            f"<td>{html.escape(str(case.get('name') or 'unknown'))}</td>"
            f'<td><span class="status {html.escape(str(case.get("status")))}">'
            f"{html.escape(str(case.get('status')))}</span></td>"
            f"<td>{html.escape(str(case.get('failure_step') or ''))}</td>"
            f"<td>{html.escape(str(case.get('current_url') or ''))}</td>"
            f"<td>{' '.join(links)}</td>"
            "</tr>"
        )

    document = f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>KrabArena Web Smoke Report</title>
  <style>
    body {{ font-family: sans-serif; margin: 2rem; color: #202124; }}
    table {{ border-collapse: collapse; width: 100%; }}
    th, td {{ border: 1px solid #d8d8d8; padding: 0.5rem; text-align: left; }}
    th {{ background: #f4f4f4; }}
    .status {{ font-weight: 700; }}
    .passed {{ color: #147a32; }}
    .failed {{ color: #b3261e; }}
    .skipped {{ color: #8a5a00; }}
  </style>
</head>
<body>
  <h1>KrabArena Web Smoke Report</h1>
  <p><strong>Status:</strong> {html.escape(str(suite.get("status")))}</p>
  <p><strong>Base URL:</strong> {html.escape(str(suite.get("base_url")))}</p>
  <p><strong>Started:</strong> {html.escape(str(suite.get("run_started_at")))}</p>
  <p><strong>Finished:</strong> {html.escape(str(suite.get("run_finished_at")))}</p>
  <table>
    <thead>
      <tr>
        <th>Test case</th>
        <th>Status</th>
        <th>Failure step</th>
        <th>Current URL</th>
        <th>Artifacts</th>
      </tr>
    </thead>
    <tbody>
      {"".join(rows)}
    </tbody>
  </table>
</body>
</html>
"""
    (PLAYWRIGHT_REPORT_DIR / "index.html").write_text(document, encoding="utf-8")


def _trace_summary(
    *,
    test_case: str,
    nodeid: str,
    status: str,
    base_url: str,
    current_url: str | None,
    failure_step: str | None,
    runtime_data: dict[str, Any],
    error_text: str | None,
) -> str:
    lines = [
        f"# Failure summary: {test_case}",
        "",
        f"- status: {status}",
        f"- nodeid: `{nodeid}`",
        f"- base_url: `{base_url}`",
        f"- final_url: `{current_url or ''}`",
        f"- failure_step: `{failure_step or ''}`",
        "",
        "## Last user actions / navigations",
        "",
    ]

    steps = runtime_data.get("steps")
    if isinstance(steps, list) and steps:
        for step in steps[-MAX_TRACE_SUMMARY_STEPS:]:
            lines.append(f"- {_format_step(step)}")
    else:
        lines.append("- No recorded user actions.")

    lines.extend(["", "## Browser diagnostics", ""])
    diagnostics_found = False
    for key in ("console_errors", "page_errors", "failed_requests", "http_errors"):
        items = runtime_data.get(key)
        if not isinstance(items, list) or not items:
            continue
        diagnostics_found = True
        lines.append(f"### {key}")
        lines.append("")
        for item in items[-MAX_DIAGNOSTICS:]:
            lines.append(
                f"- `{_single_line(json.dumps(_json_safe(item), sort_keys=True))}`"
            )
        lines.append("")

    if not diagnostics_found:
        lines.append(
            "- No browser console, page, request, or HTTP error signals recorded."
        )

    lines.extend(["", "## Assertion / error excerpt", ""])
    if error_text:
        lines.append("```text")
        lines.append(_error_excerpt(error_text))
        lines.append("```")
    else:
        lines.append("No assertion output was captured.")

    return "\n".join(lines) + "\n"


def _format_step(step: Any) -> str:
    if not isinstance(step, dict):
        return _single_line(str(step))

    name = step.get("name", "unknown-step")
    details = {
        key: value
        for key, value in step.items()
        if key not in {"name"} and value is not None
    }
    if not details:
        return str(name)
    return f"{name} — `{_single_line(json.dumps(_json_safe(details), sort_keys=True))}`"


def _maybe_artifact_path(path: Path) -> str | None:
    return _artifact_path(path) if path.exists() else None


def _artifact_path(path: Path) -> str:
    try:
        return str(path.relative_to(E2E_ROOT))
    except ValueError:
        return str(path)


def _html_rel(path: str) -> str:
    artifact_path = Path(path)
    if artifact_path.parts and artifact_path.parts[0] == ".artifacts":
        artifact_path = Path(*artifact_path.parts[1:])
    if artifact_path.parts and artifact_path.parts[0] == "playwright-report":
        artifact_path = Path("index.html")
    else:
        artifact_path = Path("..") / artifact_path
    return html.escape(artifact_path.as_posix(), quote=True)


def _clean_error(error_text: str) -> str:
    cleaned = ANSI_ESCAPE_RE.sub("", error_text)
    return cleaned[-MAX_ERROR_CHARS:]


def _error_message(error_text: str) -> str:
    lines = [line.strip() for line in error_text.splitlines() if line.strip()]
    for line in reversed(lines):
        if line.startswith("E       "):
            return line.removeprefix("E       ")[:1_000]
    for line in reversed(lines):
        if "AssertionError" in line or "Error:" in line:
            return line[:1_000]
    return lines[-1][:1_000] if lines else ""


def _error_excerpt(error_text: str, *, max_lines: int = 200) -> str:
    lines = error_text.splitlines()
    return "\n".join(lines[-max_lines:])


def _json_safe(value: Any) -> Any:
    try:
        json.dumps(value)
        return value
    except TypeError:
        if isinstance(value, dict):
            return {str(k): _json_safe(v) for k, v in value.items()}
        if isinstance(value, (list, tuple)):
            return [_json_safe(item) for item in value]
        return str(value)


def _single_line(value: str) -> str:
    return " ".join(value.split())


def _now() -> str:
    return datetime.now(UTC).isoformat()


def _safe_test_id(nodeid: str) -> str:
    lowered = nodeid.lower()
    replaced = re.sub(r"[^a-z0-9._-]+", "-", lowered)
    return replaced.strip("-") or "unknown-test"
