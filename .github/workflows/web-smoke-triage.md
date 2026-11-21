---
name: Web Smoke Triage
on:
  workflow_call:
    inputs:
      artifact-name:
        description: "Name of the uploaded smoke artifact."
        required: false
        type: string
        default: "web-smoke-artifacts"
      artifacts-dir:
        description: "Path to the prepared tests/e2e smoke artifacts."
        required: false
        type: string
        default: "tests/e2e/.artifacts"

permissions:
  contents: read
  issues: read
  actions: read

engine:
  id: gemini
  model: gemini-2.5-flash
  env:
    GEMINI_API_KEY: ${{ secrets.GH_AW_GEMINI_API_KEY }}

tools:
  github:
    toolsets: [issues]

safe-outputs:
  create-issue:
    title-prefix: "[web-smoke] "
    labels: [web-smoke, production, automated]
    max: 1
    deduplicate-by-title: 1
    expires: false
  add-comment:
    max: 1
    target: "*"
    required-labels: [web-smoke, production, automated]
    required-title-prefix: "[web-smoke] "

pre-agent-steps:
  - name: Download web smoke artifacts
    uses: actions/download-artifact@v4
    with:
      name: ${{ inputs.artifact-name }}
      path: ${{ inputs.artifacts-dir }}
---

# Web Smoke Failure Triage

Analyze the deterministic Playwright smoke failure handoff and create exactly one safe output:

- `create_issue` when no open issue already contains the smoke fingerprint marker.
- `add_comment` when an open issue already contains the smoke fingerprint marker.

The Playwright run is the only source of truth for pass/fail. Do not change, reinterpret, or rerun it.

## Inputs to read

Use `${{ inputs.artifacts-dir }}` as the artifacts root.

Read only these files and paths referenced inside them:

- `${{ inputs.artifacts-dir }}/smoke-agent-input.json`
- `${{ inputs.artifacts-dir }}/smoke-agent-instructions.md`
- `${{ inputs.artifacts-dir }}/smoke-failure-summary.md`
- `${{ inputs.artifacts-dir }}/smoke-failure-summary.json`
- `${{ inputs.artifacts-dir }}/failures/<test-id>/failure.json`
- `${{ inputs.artifacts-dir }}/failures/<test-id>/trace-summary.md`
- `${{ inputs.artifacts-dir }}/failures/<test-id>/error.txt`
- `${{ inputs.artifacts-dir }}/failures/<test-id>/screenshot.png`

Do not inspect passed test artifacts. Do not inspect or unpack raw `trace.zip`; include its path only as evidence for Trace Viewer.

## Hard constraints

Do not:

- rerun Playwright or pytest;
- open a browser or browse `https://krabarena.com`;
- use CloakBrowser MCP;
- call KrabArena API, `krab` CLI, or KrabArena MCP;
- read repository files outside the bounded artifacts listed above;
- include secrets, cookies, OAuth codes, tokens, or credentials;
- create an issue when the handoff has no failed Playwright smoke cases.

## Required analysis

Start from `smoke-agent-input.json` and treat `failure.json` as authoritative for each failed case.

Use the `issue_policy` object from `smoke-agent-input.json`:

- `fingerprint` is the deterministic duplicate key.
- `fingerprint_marker` must appear in every newly created issue body.
- `labels` are the required issue labels.
- `title_prefix` is supplied by the safe output configuration.
- `create_issue_title` is the title to request from `create_issue` without the prefix.
- `github_run.url` should be linked when available.

For each failed case, summarize:

- scenario name;
- failed step;
- category;
- current URL;
- expected behavior;
- observed behavior;
- evidence paths;
- whether screenshot evidence is available.

## Duplicate check

Before creating a new issue, use GitHub issue search/read tools to find an open issue in this repository whose body contains `issue_policy.fingerprint_marker`.

If a matching open issue exists:

- emit one `add_comment` safe output targeting that issue;
- include the recurrence timestamp or run URL when available;
- include the current failed cases and artifact references;
- do not emit `create_issue`.

If no matching open issue exists:

- emit one `create_issue` safe output;
- use `issue_policy.create_issue_title` as the requested title;
- include `issue_policy.fingerprint_marker` in the body;
- include labels through configured safe output labels;
- include artifact paths, screenshot availability, trace zip path, and GitHub Actions run URL when available.

## New issue body template

Use this structure for a newly created issue:

```markdown
<!-- krabarena-web-smoke:<fingerprint> -->

## Summary

<one-sentence user-visible failure summary>

## Failed Scenarios

- `<test-case>` at `<failure-step>`: observed <observed behavior>; expected <expected behavior>.

## User Impact

<production user path affected>

## Evidence

- Production URL: `https://krabarena.com`
- GitHub Actions run: <run URL or `not available`>
- Failure manifest: `<path>`
- Trace summary: `<path>`
- Trace zip: `<path>`
- Screenshot: `<path or not available>`
- Error output: `<path>`

## Expected Behavior

<expected behavior from the failure packet>

## Observed Behavior

<observed behavior from the failure packet>

## Suggested Triage

<concrete next checks grounded only in artifacts>
```

## Existing issue comment template

Use this structure for a recurrence comment:

```markdown
## Recurrence detected

The scheduled web smoke suite failed again with the same fingerprint.

- GitHub Actions run: <run URL or `not available`>
- Failed scenarios: <names>
- Primary failure step: <step>
- Category: <category>
- Trace summary: `<path>`
- Screenshot: `<path or not available>`
```

If safe outputs are unavailable or a required artifact is missing, do not invent a GitHub mutation. Emit the issue-ready Markdown and explain the blocker in the workflow output.
