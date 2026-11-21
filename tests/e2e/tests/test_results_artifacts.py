from __future__ import annotations

import json
from pathlib import Path

import pytest

from helpers import results


@pytest.mark.foundation
def test_suite_result_schema_and_statuses(tmp_path, monkeypatch) -> None:
    artifact_root = _use_temp_artifacts(tmp_path, monkeypatch)
    base_url = "http://127.0.0.1:3001"

    results.initialize_artifact_run(base_url=base_url)

    passed = results.create_test_artifacts(
        "tests/test_home_smoke.py::test_home_smoke[chromium]",
        base_url=base_url,
    )
    passed.add_runtime_data("scenario", "home-smoke")
    passed.write_result(status="passed")

    skipped = results.create_test_artifacts(
        "tests/test_auth_smoke.py::test_google_oauth_smoke[chromium]",
        base_url=base_url,
    )
    skipped.add_runtime_data("scenario", "google-oauth-smoke")
    skipped.write_result(status="skipped")

    failed = results.create_test_artifacts(
        "tests/test_search_smoke.py::test_search_smoke[chromium]",
        base_url=base_url,
    )
    failed.add_runtime_data("scenario", "search-smoke")
    failed.add_runtime_data("current_step", "submit-search")
    failed.add_runtime_data(
        "steps",
        [{"name": "open-home"}, {"name": "submit-search"}],
    )
    failed.failure_dir.mkdir(parents=True)
    failed.trace_path.write_bytes(b"trace")
    failed.screenshot_path.write_bytes(b"png")
    failed.write_result(
        status="failed",
        error_text="\x1b[31mE       AssertionError: search failed\x1b[0m",
    )

    results.finalize_artifact_run(exitstatus=1)

    suite = _read_json(artifact_root / "results.json")
    assert suite["schema_version"] == 2
    assert suite["suite"] == "web-smoke"
    assert suite["runner"] == "pytest-playwright"
    assert suite["language"] == "python"
    assert suite["dependency_manager"] == "uv"
    assert suite["production_url"] == "https://krabarena.com"
    assert suite["base_url"] == base_url
    assert suite["run_started_at"]
    assert suite["run_finished_at"]
    assert suite["status"] == "failed"

    statuses = {case["name"]: case["status"] for case in suite["test_cases"]}
    assert statuses == {
        "home-smoke": "passed",
        "google-oauth-smoke": "skipped",
        "search-smoke": "failed",
    }

    failed_case = _case(suite, "search-smoke")
    assert failed_case["failure_step"] == "submit-search"
    assert failed_case["error_message"] == "AssertionError: search failed"
    assert failed_case["artifacts"]["trace"]
    assert failed_case["artifacts"]["screenshot"]
    assert failed_case["artifacts"]["failure_json"]
    assert failed_case["artifacts"]["trace_summary"]
    assert failed_case["artifacts"]["error_text"]
    assert (artifact_root / "playwright-report" / "index.html").exists()


@pytest.mark.foundation
def test_failed_result_writes_llm_failure_packet(tmp_path, monkeypatch) -> None:
    artifact_root = _use_temp_artifacts(tmp_path, monkeypatch)
    base_url = "http://127.0.0.1:3001"

    results.initialize_artifact_run(base_url=base_url)
    artifacts = results.create_test_artifacts(
        "tests/test_claim_smoke.py::test_claim_detail_smoke[chromium]",
        base_url=base_url,
    )
    artifacts.set_current_url("http://127.0.0.1:3001/claims/example")
    artifacts.add_runtime_data("scenario", "claim-detail-smoke")
    artifacts.add_runtime_data("current_step", "open-claim-detail")
    artifacts.add_runtime_data(
        "console_errors",
        [{"text": "boom", "location": {"url": "/claims/example"}}],
    )
    artifacts.failure_dir.mkdir(parents=True)
    artifacts.trace_path.write_bytes(b"trace")
    artifacts.screenshot_path.write_bytes(b"png")
    artifacts.write_result(
        status="failed",
        error_text="E       AssertionError: missing claim body",
    )

    failure_dir = artifact_root / "failures" / artifacts.test_id
    failure_json = _read_json(failure_dir / "failure.json")
    trace_summary = (failure_dir / "trace-summary.md").read_text(encoding="utf-8")

    assert failure_json["name"] == "claim-detail-smoke"
    assert failure_json["status"] == "failed"
    assert failure_json["current_url"] == "http://127.0.0.1:3001/claims/example"
    assert failure_json["failure_step"] == "open-claim-detail"
    assert "missing claim body" in trace_summary
    assert "console_errors" in trace_summary
    assert (failure_dir / "error.txt").exists()
    assert (failure_dir / "screenshot.png").exists()


@pytest.mark.foundation
def test_fresh_run_does_not_reference_stale_failure_artifacts(
    tmp_path,
    monkeypatch,
) -> None:
    artifact_root = _use_temp_artifacts(tmp_path, monkeypatch)
    base_url = "http://127.0.0.1:3001"
    nodeid = "tests/test_search_smoke.py::test_search_smoke[chromium]"

    results.initialize_artifact_run(base_url=base_url)
    failed = results.create_test_artifacts(nodeid, base_url=base_url)
    failed.add_runtime_data("scenario", "search-smoke")
    failed.failure_dir.mkdir(parents=True)
    failed.trace_path.write_bytes(b"trace")
    failed.screenshot_path.write_bytes(b"png")
    failed.write_result(status="failed", error_text="E       AssertionError: old")
    assert (artifact_root / "failures").exists()

    results.initialize_artifact_run(base_url=base_url)
    passed = results.create_test_artifacts(nodeid, base_url=base_url)
    passed.add_runtime_data("scenario", "search-smoke")
    passed.write_result(status="passed")
    results.finalize_artifact_run(exitstatus=0)

    suite = _read_json(artifact_root / "results.json")
    assert suite["status"] == "passed"
    assert not (artifact_root / "failures").exists()

    case = _case(suite, "search-smoke")
    assert case["artifacts"]["trace"] is None
    assert case["artifacts"]["screenshot"] is None
    assert case["artifacts"]["failure_packet"] is None


@pytest.mark.foundation
def test_error_cleaning_strips_ansi_and_truncates() -> None:
    long_error = "\x1b[31m" + ("x" * (results.MAX_ERROR_CHARS + 100)) + "\x1b[0m"

    cleaned = results._clean_error(long_error)

    assert "\x1b" not in cleaned
    assert len(cleaned) == results.MAX_ERROR_CHARS


def _use_temp_artifacts(tmp_path: Path, monkeypatch) -> Path:
    e2e_root = tmp_path / "e2e"
    artifact_root = e2e_root / ".artifacts"
    monkeypatch.setattr(results, "E2E_ROOT", e2e_root)
    monkeypatch.setattr(results, "ARTIFACT_ROOT", artifact_root)
    monkeypatch.setattr(results, "RESULTS_INDEX", artifact_root / "results.json")
    monkeypatch.setattr(results, "FAILURES_DIR", artifact_root / "failures")
    monkeypatch.setattr(
        results,
        "PLAYWRIGHT_REPORT_DIR",
        artifact_root / "playwright-report",
    )
    monkeypatch.setattr(
        results,
        "TEST_RESULTS_DIR",
        artifact_root / "test-results",
    )
    return artifact_root


def _read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def _case(suite: dict, name: str) -> dict:
    return next(case for case in suite["test_cases"] if case["name"] == name)
