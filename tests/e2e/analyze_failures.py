from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

E2E_ROOT = Path(__file__).resolve().parent
ARTIFACT_ROOT = E2E_ROOT / ".artifacts"
RESULTS_PATH = ARTIFACT_ROOT / "results.json"
SUMMARY_MD_PATH = ARTIFACT_ROOT / "smoke-failure-summary.md"
SUMMARY_JSON_PATH = ARTIFACT_ROOT / "smoke-failure-summary.json"
AGENT_INPUT_PATH = ARTIFACT_ROOT / "smoke-agent-input.json"
AGENT_INSTRUCTIONS_PATH = ARTIFACT_ROOT / "smoke-agent-instructions.md"

MAX_TRACE_SUMMARY_CHARS = 12_000
MAX_ERROR_CHARS = 8_000
ISSUE_LABELS = ("web-smoke", "production", "automated")
ISSUE_TITLE_PREFIX = "[web-smoke] "
ISSUE_MARKER_PREFIX = "krabarena-web-smoke"

APPROVED_CATEGORIES = (
    "site-unavailable",
    "blank-page",
    "server-error",
    "client-side-crash",
    "broken-navigation",
    "missing-critical-content",
    "claim-rendering-failure",
    "search-failure",
    "oauth-configuration-error",
    "result-page-broken",
    "test-flake-or-unclear",
    "unknown-needs-human-triage",
)

SECRET_VALUE_RE = re.compile(
    r"(?i)\b(access_token|id_token|refresh_token|client_secret|authorization|"
    r"cookie|token|secret)=([^\s&]+)"
)
OAUTH_PARAM_RE = re.compile(r"(?i)([?&](?:code|state)=)([^&\s]+)")


@dataclass(frozen=True)
class FailedCasePacket:
    name: str
    failure_step: str | None
    category: str
    current_url: str | None
    expected_behavior: str | None
    observed_behavior: str | None
    error_message: str | None
    runtime_data: dict[str, Any]
    evidence: dict[str, str | None]
    trace_summary: str
    error_output: str
    screenshot_available: bool


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Prepare GitHub Agentic Workflow handoff for web-smoke failures."
    )
    parser.add_argument(
        "--artifacts-dir",
        default=str(ARTIFACT_ROOT),
        help="Path to tests/e2e/.artifacts.",
    )
    args = parser.parse_args(argv)

    artifacts_dir = Path(args.artifacts_dir)
    result = analyze_failures(artifacts_dir=artifacts_dir)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


def analyze_failures(*, artifacts_dir: Path = ARTIFACT_ROOT) -> dict[str, Any]:
    results_path = artifacts_dir / "results.json"
    summary_md_path = artifacts_dir / "smoke-failure-summary.md"
    summary_json_path = artifacts_dir / "smoke-failure-summary.json"
    agent_input_path = artifacts_dir / "smoke-agent-input.json"
    agent_instructions_path = artifacts_dir / "smoke-agent-instructions.md"

    suite = _read_json(results_path)
    failed_cases = _failed_cases_from_suite(suite, artifacts_dir=artifacts_dir)

    if not failed_cases:
        _remove_if_exists(agent_input_path)
        _remove_if_exists(agent_instructions_path)
        summary = _no_failures_summary(summary_md_path)
        _write_outputs(summary, summary_md_path, summary_json_path)
        return summary

    summary = _fallback_summary(suite, failed_cases, summary_md_path)
    summary["issue_policy"] = _issue_policy(summary, failed_cases[0])
    agent_payload = _agent_input_payload(suite, failed_cases, summary)
    agent_instructions = _agent_instructions(agent_payload)

    agent_input_path.write_text(
        json.dumps(agent_payload, indent=2, sort_keys=True),
        encoding="utf-8",
    )
    agent_instructions_path.write_text(agent_instructions, encoding="utf-8")

    summary["analysis_mode"] = "github-agentic-workflow-handoff"
    summary["llm_used"] = False
    summary["agent_handoff"] = {
        "input": _artifact_output_path(agent_input_path),
        "instructions": _artifact_output_path(agent_instructions_path),
    }
    _write_outputs(summary, summary_md_path, summary_json_path)
    return summary


def _failed_cases_from_suite(
    suite: dict[str, Any],
    *,
    artifacts_dir: Path,
) -> list[FailedCasePacket]:
    cases = suite.get("test_cases", [])
    if not isinstance(cases, list):
        return []

    packets = []
    for case in cases:
        if not isinstance(case, dict) or case.get("status") != "failed":
            continue
        packets.append(_failed_case_packet(case, artifacts_dir=artifacts_dir))
    return packets


def _failed_case_packet(
    case: dict[str, Any],
    *,
    artifacts_dir: Path,
) -> FailedCasePacket:
    artifacts = case.get("artifacts") if isinstance(case.get("artifacts"), dict) else {}
    evidence = {
        "failure_json": artifacts.get("failure_json"),
        "trace_summary": artifacts.get("trace_summary"),
        "trace_zip": artifacts.get("trace"),
        "screenshot": artifacts.get("screenshot"),
        "error_output": artifacts.get("error_text"),
    }

    failure_json = _read_optional_json(
        _artifact_path(evidence["failure_json"], artifacts_dir=artifacts_dir)
    )
    source = failure_json if failure_json else case
    trace_summary = _read_optional_text(
        _artifact_path(evidence["trace_summary"], artifacts_dir=artifacts_dir),
        max_chars=MAX_TRACE_SUMMARY_CHARS,
    )
    error_output = _read_optional_text(
        _artifact_path(evidence["error_output"], artifacts_dir=artifacts_dir),
        max_chars=MAX_ERROR_CHARS,
    )
    screenshot_path = _artifact_path(
        evidence["screenshot"], artifacts_dir=artifacts_dir
    )

    return FailedCasePacket(
        name=str(source.get("name") or source.get("test_case") or "unknown-test"),
        failure_step=_string_or_none(source.get("failure_step")),
        category=_fallback_category(source),
        current_url=_string_or_none(source.get("current_url")),
        expected_behavior=_string_or_none(source.get("expected_behavior")),
        observed_behavior=_string_or_none(source.get("observed_behavior")),
        error_message=_string_or_none(source.get("error_message")),
        runtime_data=(
            source.get("runtime_data")
            if isinstance(source.get("runtime_data"), dict)
            else {}
        ),
        evidence=evidence,
        trace_summary=trace_summary,
        error_output=error_output,
        screenshot_available=bool(screenshot_path and screenshot_path.exists()),
    )


def _agent_input_payload(
    suite: dict[str, Any],
    failed_cases: list[FailedCasePacket],
    fallback_summary: dict[str, Any],
) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "mode": "github-agentic-workflow",
        "task": "analyze-web-smoke-failures-and-prepare-issue-summary",
        "constraints": [
            "Do not rerun Playwright or pytest.",
            "Do not open a browser or use CloakBrowser MCP.",
            "Do not call KrabArena API, krab CLI, or KrabArena MCP.",
            "Do not read raw trace zip contents.",
            "Do not read passed test artifacts.",
            "Treat failure.json as authoritative.",
            "Use exactly one approved category per failed case.",
            "Do not create issues directly except through configured safe outputs.",
        ],
        "approved_categories": list(APPROVED_CATEGORIES),
        "suite": {
            "name": suite.get("suite"),
            "status": suite.get("status"),
            "production_url": suite.get("production_url"),
            "base_url": suite.get("base_url"),
            "run_started_at": suite.get("run_started_at"),
            "run_finished_at": suite.get("run_finished_at"),
        },
        "deterministic_fallback_summary": fallback_summary,
        "issue_policy": fallback_summary.get("issue_policy"),
        "failed_cases": [_agent_case_payload(case) for case in failed_cases],
        "required_outputs": {
            "markdown_sections": [
                "Summary",
                "Failed Scenarios",
                "User Impact",
                "Evidence",
                "Expected Behavior",
                "Observed Behavior",
                "Suggested Triage",
            ],
            "json_fields": [
                "title",
                "body_path",
                "fingerprint",
                "failed_cases",
            ],
        },
    }


def _agent_case_payload(case: FailedCasePacket) -> dict[str, Any]:
    return {
        "name": case.name,
        "failure_step": case.failure_step,
        "category_hint": case.category,
        "current_url": _redact(case.current_url or ""),
        "expected_behavior": _redact(case.expected_behavior or ""),
        "observed_behavior": _redact(case.observed_behavior or ""),
        "error_message": _redact(case.error_message or ""),
        "runtime_data": _redact_json(case.runtime_data),
        "trace_summary": _redact(case.trace_summary),
        "error_output": _redact(case.error_output),
        "screenshot_available": case.screenshot_available,
        "evidence": case.evidence,
    }


def _agent_instructions(payload: dict[str, Any]) -> str:
    fingerprint = payload["deterministic_fallback_summary"].get("fingerprint")
    issue_policy = payload.get("issue_policy")
    marker = (
        issue_policy.get("fingerprint_marker")
        if isinstance(issue_policy, dict)
        else None
    )
    return "\n".join(
        [
            "# Web Smoke Failure Analysis Agent Task",
            "",
            "Analyze only `tests/e2e/.artifacts/smoke-agent-input.json` and the",
            "artifact paths referenced inside it.",
            "",
            "Do not rerun tests, browse the site, call APIs, use MCP tools, or read",
            "raw Playwright trace zip contents. Use the trace zip only as evidence.",
            "",
            "Use `failure.json` fields as authoritative. If `trace-summary.md`,",
            "`error.txt`, or screenshot evidence conflicts with `failure.json`,",
            "state the uncertainty instead of inventing facts.",
            "",
            "Prepare an issue-ready summary with these sections:",
            "",
            "- Summary",
            "- Failed Scenarios",
            "- User Impact",
            "- Evidence",
            "- Expected Behavior",
            "- Observed Behavior",
            "- Suggested Triage",
            "",
            "Use this deterministic fingerprint unless the input evidence proves",
            "the primary category must be corrected:",
            "",
            f"`{fingerprint}`",
            "",
            "Include this hidden marker in every created issue body:",
            "",
            f"`{marker}`",
            "",
            "Before creating a new issue, search open issues for the hidden marker.",
            "If a matching open issue exists, add a recurrence comment instead of",
            "creating a duplicate issue.",
            "",
            "If safe outputs are enabled, create or update exactly one GitHub Issue",
            "for that fingerprint. If safe outputs are not enabled, emit the final",
            "markdown body and machine-readable JSON summary as workflow artifacts.",
            "",
        ]
    )


def _issue_policy(
    summary: dict[str, Any],
    primary_case: FailedCasePacket,
) -> dict[str, Any]:
    fingerprint = str(summary.get("fingerprint") or "")
    title = str(summary.get("title") or "")
    create_issue_title = title
    if create_issue_title.startswith(ISSUE_TITLE_PREFIX):
        create_issue_title = create_issue_title.removeprefix(ISSUE_TITLE_PREFIX)

    return _redact_json(
        {
            "fingerprint": fingerprint,
            "fingerprint_marker": f"<!-- {ISSUE_MARKER_PREFIX}:{fingerprint} -->",
            "labels": list(ISSUE_LABELS),
            "title_prefix": ISSUE_TITLE_PREFIX,
            "create_issue_title": create_issue_title,
            "primary_failed_case": {
                "name": primary_case.name,
                "failure_step": primary_case.failure_step,
                "category": primary_case.category,
            },
            "github_run": _github_run_metadata(),
        }
    )


def _fallback_summary(
    suite: dict[str, Any],
    failed_cases: list[FailedCasePacket],
    summary_md_path: Path,
) -> dict[str, Any]:
    primary = failed_cases[0]
    production_url = str(suite.get("production_url") or "https://krabarena.com")
    title = f"[web-smoke] {primary.name} failed: {_human_category(primary.category)}"
    fingerprint = f"web-smoke:{production_url}:{primary.name}:{primary.category}"

    return _redact_json(
        {
            "schema_version": 1,
            "analysis_mode": "deterministic-fallback",
            "llm_used": False,
            "production_url": production_url,
            "title": title,
            "body_path": _artifact_output_path(summary_md_path),
            "fingerprint": fingerprint,
            "failed_cases": [_case_summary(case) for case in failed_cases],
            "summary": _fallback_summary_sentence(failed_cases),
            "user_impact": _fallback_user_impact(failed_cases),
            "expected_behavior": _combined_expected(failed_cases),
            "observed_behavior": _combined_observed(failed_cases),
            "suggested_triage": _fallback_triage(),
        }
    )


def _case_summary(case: FailedCasePacket) -> dict[str, Any]:
    return {
        "name": case.name,
        "failure_step": case.failure_step,
        "category": case.category,
        "current_url": case.current_url,
        "expected_behavior": case.expected_behavior,
        "observed_behavior": case.observed_behavior,
        "error_message": case.error_message,
        "evidence_sufficient": bool(case.trace_summary or case.error_output),
        "suggested_triage": _case_triage(case),
        "evidence": case.evidence,
    }


def _no_failures_summary(summary_md_path: Path) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "analysis_mode": "no-failures",
        "llm_used": False,
        "no_failures": True,
        "production_url": "https://krabarena.com",
        "title": "[web-smoke] no failures",
        "body_path": _artifact_output_path(summary_md_path),
        "fingerprint": None,
        "failed_cases": [],
        "summary": "No failed Playwright smoke scenarios were found.",
        "user_impact": "No production user path failure was detected.",
        "expected_behavior": "All smoke scenarios pass.",
        "observed_behavior": "No failed smoke scenarios were present.",
        "suggested_triage": "No triage is required.",
    }


def _write_outputs(
    summary: dict[str, Any],
    summary_md_path: Path,
    summary_json_path: Path,
) -> None:
    summary_md_path.parent.mkdir(parents=True, exist_ok=True)
    summary_md_path.write_text(_render_markdown(summary), encoding="utf-8")
    summary_json_path.write_text(
        json.dumps(summary, indent=2, sort_keys=True),
        encoding="utf-8",
    )


def _remove_if_exists(path: Path) -> None:
    if path.exists():
        path.unlink()


def _render_markdown(summary: dict[str, Any]) -> str:
    failed_cases = summary.get("failed_cases")
    cases = failed_cases if isinstance(failed_cases, list) else []
    evidence_lines = []
    for case in cases:
        if not isinstance(case, dict):
            continue
        evidence = (
            case.get("evidence") if isinstance(case.get("evidence"), dict) else {}
        )
        evidence_lines.append(f"- `{case.get('name')}`")
        for label, key in (
            ("Failure manifest", "failure_json"),
            ("Trace summary", "trace_summary"),
            ("Trace zip", "trace_zip"),
            ("Screenshot", "screenshot"),
            ("Error output", "error_output"),
        ):
            value = evidence.get(key)
            evidence_lines.append(f"  - {label}: `{value or 'not available'}`")

    failed_scenarios = [
        "- `{name}` at `{step}`: {observed} Expected: {expected}".format(
            name=case.get("name", "unknown"),
            step=case.get("failure_step") or "unknown-step",
            observed=case.get("observed_behavior") or case.get("error_message") or "",
            expected=case.get("expected_behavior") or "",
        )
        for case in cases
        if isinstance(case, dict)
    ]

    return "\n".join(
        [
            "## Summary",
            "",
            str(summary.get("summary") or ""),
            "",
            "## Failed Scenarios",
            "",
            "\n".join(failed_scenarios) if failed_scenarios else "- None.",
            "",
            "## User Impact",
            "",
            str(summary.get("user_impact") or ""),
            "",
            "## Evidence",
            "",
            "\n".join(evidence_lines) if evidence_lines else "- No failure evidence.",
            "",
            "## Expected Behavior",
            "",
            str(summary.get("expected_behavior") or ""),
            "",
            "## Observed Behavior",
            "",
            str(summary.get("observed_behavior") or ""),
            "",
            "## Suggested Triage",
            "",
            str(summary.get("suggested_triage") or ""),
            "",
        ]
    )


def _fallback_category(source: dict[str, Any]) -> str:
    blob = json.dumps(source, sort_keys=True, default=str).lower()
    name = str(source.get("name") or source.get("test_case") or "").lower()
    step = str(source.get("failure_step") or "").lower()

    if "site unavailable" in blob or "service unavailable" in blob:
        return "site-unavailable"
    if "blank page" in blob or "expected visible body content" in blob:
        return "blank-page"
    if "internal server error" in blob or "status 5" in blob or " 500" in blob:
        return "server-error"
    if "client-side crash" in blob or "hydration failed" in blob:
        return "client-side-crash"
    if "oauth" in name or "oauth" in step or "redirect_uri" in blob:
        return "oauth-configuration-error"
    if "search" in name or "search" in step:
        return "search-failure"
    if "claim" in name and ("body" in blob or "summary" in blob):
        return "claim-rendering-failure"
    if "navigation" in blob or "recover" in blob or "click" in step:
        return "broken-navigation"
    if "result" in name or "result" in step:
        return "result-page-broken"
    if "visible" in blob or "missing" in blob or "expected" in blob:
        return "missing-critical-content"
    if "timeout" in blob or "flaky" in blob:
        return "test-flake-or-unclear"
    return "unknown-needs-human-triage"


def _fallback_summary_sentence(failed_cases: list[FailedCasePacket]) -> str:
    primary = failed_cases[0]
    return (
        f"`{primary.name}` failed at `{primary.failure_step or 'unknown-step'}` "
        f"with category `{primary.category}`."
    )


def _fallback_user_impact(failed_cases: list[FailedCasePacket]) -> str:
    names = ", ".join(f"`{case.name}`" for case in failed_cases)
    return f"One or more critical production web smoke paths failed: {names}."


def _combined_expected(failed_cases: list[FailedCasePacket]) -> str:
    values = [case.expected_behavior for case in failed_cases if case.expected_behavior]
    return " ".join(values) if values else "The smoke scenario should complete."


def _combined_observed(failed_cases: list[FailedCasePacket]) -> str:
    values = [
        case.observed_behavior or case.error_message
        for case in failed_cases
        if case.observed_behavior or case.error_message
    ]
    return " ".join(values) if values else "The smoke scenario failed."


def _fallback_triage() -> str:
    return (
        "Use the GitHub Agentic Workflow handoff files first. Start with "
        "smoke-agent-input.json, then inspect linked failure.json and "
        "trace-summary.md files before opening screenshots or raw traces."
    )


def _case_triage(case: FailedCasePacket) -> str:
    if case.category == "search-failure":
        return (
            "Check search page rendering, result population, and known-query indexing."
        )
    if case.category == "oauth-configuration-error":
        return "Check OAuth client ID, redirect URI, and provider app configuration."
    if case.category in {"server-error", "site-unavailable"}:
        return "Check production availability, server logs, and upstream dependencies."
    if case.category == "claim-rendering-failure":
        return "Check Claim detail rendering and structured markdown sections."
    return "Review the compact failure packet before opening the raw Playwright trace."


def _human_category(category: str) -> str:
    return category.replace("-", " ")


def _github_run_metadata() -> dict[str, str | None]:
    server_url = os.environ.get("GITHUB_SERVER_URL") or "https://github.com"
    repository = os.environ.get("GITHUB_REPOSITORY")
    run_id = os.environ.get("GITHUB_RUN_ID")
    run_url = (
        f"{server_url}/{repository}/actions/runs/{run_id}"
        if repository and run_id
        else None
    )
    return {
        "repository": repository,
        "run_id": run_id,
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
        "workflow": os.environ.get("GITHUB_WORKFLOW"),
        "url": run_url,
    }


def _read_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _read_optional_json(path: Path | None) -> dict[str, Any] | None:
    if path is None or not path.exists():
        return None
    loaded = json.loads(path.read_text(encoding="utf-8"))
    return loaded if isinstance(loaded, dict) else None


def _read_optional_text(path: Path | None, *, max_chars: int) -> str:
    if path is None or not path.exists():
        return ""
    text = path.read_text(encoding="utf-8", errors="replace")
    return _redact(text[-max_chars:])


def _artifact_path(value: str | None, *, artifacts_dir: Path) -> Path | None:
    if not value:
        return None
    path = Path(value)
    if path.is_absolute():
        return path
    if path.parts and path.parts[0] == ".artifacts":
        return artifacts_dir.parent / path
    return E2E_ROOT / path


def _artifact_output_path(path: Path) -> str:
    try:
        return str(path.relative_to(E2E_ROOT))
    except ValueError:
        return str(path)


def _string_or_none(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _redact(value: str) -> str:
    redacted = SECRET_VALUE_RE.sub(lambda match: f"{match.group(1)}=[redacted]", value)
    return OAUTH_PARAM_RE.sub(lambda match: f"{match.group(1)}[redacted]", redacted)


def _redact_json(value: Any) -> Any:
    if isinstance(value, str):
        return _redact(value)
    if isinstance(value, dict):
        return {str(key): _redact_json(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_redact_json(item) for item in value]
    return value


if __name__ == "__main__":
    sys.exit(main())
