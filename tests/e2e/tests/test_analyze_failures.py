from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

import analyze_failures


@pytest.mark.foundation
def test_no_failed_cases_writes_no_failure_summary_and_removes_stale_handoff(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    artifacts_dir = _artifact_tree(tmp_path, monkeypatch)
    (artifacts_dir / "smoke-agent-input.json").write_text("{}", encoding="utf-8")
    (artifacts_dir / "smoke-agent-instructions.md").write_text("old", encoding="utf-8")
    _write_results(
        artifacts_dir,
        [
            {
                "name": "home-smoke",
                "status": "passed",
                "artifacts": {},
            }
        ],
    )

    summary = analyze_failures.analyze_failures(artifacts_dir=artifacts_dir)

    assert summary["no_failures"] is True
    assert summary["failed_cases"] == []
    assert summary["llm_used"] is False
    assert (artifacts_dir / "smoke-failure-summary.md").exists()
    assert (artifacts_dir / "smoke-failure-summary.json").exists()
    assert not (artifacts_dir / "smoke-agent-input.json").exists()
    assert not (artifacts_dir / "smoke-agent-instructions.md").exists()


@pytest.mark.foundation
def test_failed_case_writes_agent_handoff_and_fallback_summary(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    artifacts_dir = _artifact_tree(tmp_path, monkeypatch)
    _write_failed_packet(
        artifacts_dir,
        name="search-smoke",
        failure_step="submit-search",
        error_message="Expected at least one search result, found zero.",
        screenshot=True,
    )
    _write_results(
        artifacts_dir,
        [
            {
                "name": "search-smoke",
                "status": "failed",
                "artifacts": _artifact_refs("search-smoke"),
            }
        ],
    )

    summary = analyze_failures.analyze_failures(artifacts_dir=artifacts_dir)
    summary_json = _read_json(artifacts_dir / "smoke-failure-summary.json")
    summary_md = (artifacts_dir / "smoke-failure-summary.md").read_text(
        encoding="utf-8"
    )
    agent_input = _read_json(artifacts_dir / "smoke-agent-input.json")
    agent_instructions = (artifacts_dir / "smoke-agent-instructions.md").read_text(
        encoding="utf-8"
    )

    assert summary["analysis_mode"] == "github-agentic-workflow-handoff"
    assert summary["llm_used"] is False
    assert summary["fingerprint"] == (
        "web-smoke:https://krabarena.com:search-smoke:search-failure"
    )
    assert summary["agent_handoff"] == {
        "input": ".artifacts/smoke-agent-input.json",
        "instructions": ".artifacts/smoke-agent-instructions.md",
    }
    assert summary_json["failed_cases"][0]["category"] == "search-failure"
    assert summary_json["failed_cases"][0]["evidence"]["trace_zip"]
    assert "## Failed Scenarios" in summary_md
    assert "search-smoke" in summary_md
    assert agent_input["mode"] == "github-agentic-workflow"
    assert agent_input["failed_cases"][0]["name"] == "search-smoke"
    assert agent_input["failed_cases"][0]["screenshot_available"] is True
    assert agent_input["issue_policy"]["fingerprint"] == summary["fingerprint"]
    assert agent_input["issue_policy"]["fingerprint_marker"] == (
        "<!-- krabarena-web-smoke:"
        "web-smoke:https://krabarena.com:search-smoke:search-failure -->"
    )
    assert agent_input["issue_policy"]["labels"] == [
        "web-smoke",
        "production",
        "automated",
    ]
    assert agent_input["issue_policy"]["title_prefix"] == "[web-smoke] "
    assert agent_input["issue_policy"]["create_issue_title"] == (
        "search-smoke failed: search failure"
    )
    assert "Do not rerun Playwright or pytest." in agent_input["constraints"]
    assert "safe outputs" in agent_instructions
    assert summary["fingerprint"] in agent_instructions
    assert agent_input["issue_policy"]["fingerprint_marker"] in agent_instructions


@pytest.mark.foundation
def test_missing_screenshot_is_explicit_without_failing_analysis(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    artifacts_dir = _artifact_tree(tmp_path, monkeypatch)
    _write_failed_packet(
        artifacts_dir,
        name="github-oauth-smoke",
        failure_step="start-github-oauth",
        error_message="redirect_uri_mismatch",
        screenshot=False,
    )
    _write_results(
        artifacts_dir,
        [
            {
                "name": "github-oauth-smoke",
                "status": "failed",
                "artifacts": _artifact_refs("github-oauth-smoke", screenshot=False),
            }
        ],
    )

    summary = analyze_failures.analyze_failures(artifacts_dir=artifacts_dir)
    agent_input = _read_json(artifacts_dir / "smoke-agent-input.json")

    assert summary["failed_cases"][0]["category"] == "oauth-configuration-error"
    assert summary["failed_cases"][0]["evidence"]["screenshot"] is None
    assert agent_input["failed_cases"][0]["screenshot_available"] is False
    assert agent_input["failed_cases"][0]["evidence"]["screenshot"] is None


@pytest.mark.foundation
@pytest.mark.parametrize(
    ("source", "category"),
    [
        ({"name": "search-smoke", "failure_step": "submit-search"}, "search-failure"),
        (
            {"name": "google-oauth-smoke", "error_message": "redirect_uri_mismatch"},
            "oauth-configuration-error",
        ),
        ({"error_message": "Internal Server Error 500"}, "server-error"),
        ({"error_message": "Hydration failed"}, "client-side-crash"),
        ({"error_message": "expected visible body content"}, "blank-page"),
    ],
)
def test_category_fallback_maps_known_failures(
    source: dict[str, Any],
    category: str,
) -> None:
    assert analyze_failures._fallback_category(source) == category


@pytest.mark.foundation
def test_agent_input_excludes_raw_trace_contents_and_passed_cases(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    artifacts_dir = _artifact_tree(tmp_path, monkeypatch)
    failure_dir = _write_failed_packet(
        artifacts_dir,
        name="search-smoke",
        failure_step="submit-search",
        error_message="access_token=secret-token",
        screenshot=True,
    )
    (failure_dir / "trace.zip").write_bytes(b"RAW_TRACE_SECRET")
    _write_results(
        artifacts_dir,
        [
            {
                "name": "home-smoke",
                "status": "passed",
                "runtime_data": {"selected_battle_title": "passed data"},
                "artifacts": {},
            },
            {
                "name": "search-smoke",
                "status": "failed",
                "artifacts": _artifact_refs("search-smoke"),
            },
        ],
    )
    suite = _read_json(artifacts_dir / "results.json")
    packets = analyze_failures._failed_cases_from_suite(
        suite,
        artifacts_dir=artifacts_dir,
    )
    fallback = analyze_failures._fallback_summary(
        suite,
        packets,
        artifacts_dir / "smoke-failure-summary.md",
    )

    payload = analyze_failures._agent_input_payload(suite, packets, fallback)
    rendered = json.dumps(payload, sort_keys=True)

    assert "RAW_TRACE_SECRET" not in rendered
    assert "passed data" not in rendered
    assert "secret-token" not in rendered
    assert "[redacted]" in rendered


@pytest.mark.foundation
def test_agent_instructions_include_safe_outputs_and_fingerprint(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    artifacts_dir = _artifact_tree(tmp_path, monkeypatch)
    _write_failed_packet(
        artifacts_dir,
        name="claim-detail-smoke",
        failure_step="open-claim-detail",
        error_message="missing claim body",
        screenshot=True,
    )
    _write_results(
        artifacts_dir,
        [
            {
                "name": "claim-detail-smoke",
                "status": "failed",
                "artifacts": _artifact_refs("claim-detail-smoke"),
            }
        ],
    )

    summary = analyze_failures.analyze_failures(artifacts_dir=artifacts_dir)
    instructions = (artifacts_dir / "smoke-agent-instructions.md").read_text(
        encoding="utf-8"
    )

    assert "safe outputs" in instructions
    assert "create or update exactly one GitHub Issue" in instructions
    assert summary["fingerprint"] in instructions


@pytest.mark.foundation
def test_issue_policy_includes_github_run_url_when_available(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GITHUB_SERVER_URL", "https://github.com")
    monkeypatch.setenv("GITHUB_REPOSITORY", "krabarena/krabarena")
    monkeypatch.setenv("GITHUB_RUN_ID", "123456")
    monkeypatch.setenv("GITHUB_RUN_ATTEMPT", "2")
    monkeypatch.setenv("GITHUB_WORKFLOW", "Web Smoke")
    artifacts_dir = _artifact_tree(tmp_path, monkeypatch)
    _write_failed_packet(
        artifacts_dir,
        name="search-smoke",
        failure_step="submit-search",
        error_message="Expected at least one search result, found zero.",
        screenshot=True,
    )
    _write_results(
        artifacts_dir,
        [
            {
                "name": "search-smoke",
                "status": "failed",
                "artifacts": _artifact_refs("search-smoke"),
            }
        ],
    )

    summary = analyze_failures.analyze_failures(artifacts_dir=artifacts_dir)
    agent_input = _read_json(artifacts_dir / "smoke-agent-input.json")

    assert summary["issue_policy"]["github_run"] == {
        "repository": "krabarena/krabarena",
        "run_id": "123456",
        "run_attempt": "2",
        "workflow": "Web Smoke",
        "url": "https://github.com/krabarena/krabarena/actions/runs/123456",
    }
    assert agent_input["issue_policy"] == summary["issue_policy"]


def _artifact_tree(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    e2e_root = tmp_path / "e2e"
    artifacts_dir = e2e_root / ".artifacts"
    artifacts_dir.mkdir(parents=True)
    monkeypatch.setattr(analyze_failures, "E2E_ROOT", e2e_root)
    monkeypatch.setattr(analyze_failures, "ARTIFACT_ROOT", artifacts_dir)
    monkeypatch.setattr(
        analyze_failures,
        "RESULTS_PATH",
        artifacts_dir / "results.json",
    )
    monkeypatch.setattr(
        analyze_failures,
        "SUMMARY_MD_PATH",
        artifacts_dir / "smoke-failure-summary.md",
    )
    monkeypatch.setattr(
        analyze_failures,
        "SUMMARY_JSON_PATH",
        artifacts_dir / "smoke-failure-summary.json",
    )
    monkeypatch.setattr(
        analyze_failures,
        "AGENT_INPUT_PATH",
        artifacts_dir / "smoke-agent-input.json",
    )
    monkeypatch.setattr(
        analyze_failures,
        "AGENT_INSTRUCTIONS_PATH",
        artifacts_dir / "smoke-agent-instructions.md",
    )
    return artifacts_dir


def _write_results(artifacts_dir: Path, test_cases: list[dict[str, Any]]) -> None:
    payload = {
        "schema_version": 2,
        "suite": "web-smoke",
        "production_url": "https://krabarena.com",
        "status": (
            "failed"
            if any(case["status"] == "failed" for case in test_cases)
            else "passed"
        ),
        "test_cases": test_cases,
    }
    (artifacts_dir / "results.json").write_text(
        json.dumps(payload, indent=2),
        encoding="utf-8",
    )


def _write_failed_packet(
    artifacts_dir: Path,
    *,
    name: str,
    failure_step: str,
    error_message: str,
    screenshot: bool,
) -> Path:
    failure_dir = artifacts_dir / "failures" / name
    failure_dir.mkdir(parents=True)
    packet = {
        "name": name,
        "status": "failed",
        "failure_step": failure_step,
        "current_url": "https://krabarena.com/example",
        "expected_behavior": f"{name} should complete.",
        "observed_behavior": error_message,
        "error_message": error_message,
        "runtime_data": {
            "search_query": "access_token=secret-token",
            "steps": [{"name": failure_step}],
        },
        "artifacts": _artifact_refs(name, screenshot=screenshot),
    }
    (failure_dir / "failure.json").write_text(
        json.dumps(packet, indent=2),
        encoding="utf-8",
    )
    (failure_dir / "trace-summary.md").write_text(
        f"# Trace Summary\n\nFailure: {error_message}\n",
        encoding="utf-8",
    )
    (failure_dir / "error.txt").write_text(
        f"E       AssertionError: {error_message}\n",
        encoding="utf-8",
    )
    (failure_dir / "trace.zip").write_bytes(b"trace")
    if screenshot:
        (failure_dir / "screenshot.png").write_bytes(b"png")
    return failure_dir


def _artifact_refs(name: str, *, screenshot: bool = True) -> dict[str, str | None]:
    base = f".artifacts/failures/{name}"
    return {
        "failure_json": f"{base}/failure.json",
        "trace_summary": f"{base}/trace-summary.md",
        "trace": f"{base}/trace.zip",
        "screenshot": f"{base}/screenshot.png" if screenshot else None,
        "error_text": f"{base}/error.txt",
    }


def _read_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))
