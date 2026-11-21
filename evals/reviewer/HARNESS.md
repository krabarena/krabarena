# /eval-reviewer harness

The playbook for grading the KrabReviewer prompt's LLM judgment layer
against the labeled fixtures in this folder. Invoked via the thin
`/eval-reviewer` skill (`.claude/skills/eval-reviewer/SKILL.md`), which
just reads this file and executes it. You can also run it directly: tell
the agent "follow evals/reviewer/HARNESS.md".

## Arguments

Optional (all have defaults):
- a **fixture id** (e.g. `numbers-not-from-script`) → run only that one.
- `--runs N` → repeats per fixture (default **3**; raises confidence + surfaces flakiness).
- `--prompt <path>` → candidate prompt to test (default
  `crates/krabarena-api/src/services/claim_review_prompt.txt`). Point it at a
  scratch copy to A/B a proposed edit against the committed prompt.

## What this evaluates

The **LLM judgment layer** of the KrabReviewer prompt — does it catch the
substance problems (runner not shipped, inputs not reproducible, numbers
not faithful, weak title) on bad claims and stay quiet on good ones? It does **not** test the deterministic pre-filter
(structure.json validity, the four required sections); that's
Rust-unit-tested. The labels live in
`evals/reviewer/fixtures/*/expected.json` (each fixture is self-contained;
its `check` field names the failure mode it targets). See
`evals/reviewer/README.md`.

## Procedure

Run these steps yourself, using the **Agent tool** to fan out sub-agents.
Prefer launching the per-fixture reviewer runs in parallel (one message,
multiple Agent calls) so the suite finishes fast.

### 1. Resolve inputs
- Read the candidate prompt file (default path above). Call its contents `PROMPT`.
- List fixture dirs under `evals/reviewer/fixtures/`. If the user named a
  fixture, restrict to it. For each, read `claim.md`, `structure.json` (if
  present), `artifact_notes.md`, and `expected.json`.
- Determine `RUNS` (default 3).

### 2. Reviewer-under-test (Agent tool, `general-purpose`), RUNS times per fixture
Spawn each with this exact prompt shape:

```
You are KrabArena's AutoReviewer. Your system prompt is below.
Apply it to the CLAIM and return ONLY the JSON verdict object it specifies
(verdict, summary, suggestions[], moderation_flags[], …). No prose, no fences.

Assume the deterministic pre-filter has ALREADY passed (structure.json is
present + valid, the four required sections exist) — judge SUBSTANCE, not
form. You are reading the artifact via the "ARTIFACT CONTENTS" notes below;
treat them as what you'd see opening the bundle.

===== SYSTEM PROMPT (the candidate reviewer prompt) =====
{PROMPT}

===== INPUT =====
Battle: standalone (no curated battle); judge structure + quality + on-topic.
Claim body (markdown):
{claim.md}

structure.json:
{structure.json or "absent"}

ARTIFACT CONTENTS (what you'd see reading the bundle):
{artifact_notes.md}
```

Collect each run's raw output and parse the JSON. If a run returns
non-JSON, record it as a malformed run (counts as a miss; note it).

### 3. Judge (Agent tool, `general-purpose`), once per fixture
Give the judge the expected label + all RUNS of reviewer output:

```
You are grading a claim reviewer against a known-correct label. Be strict
but fair: match on MEANING, not exact id/category wording.

EXPECTED:
{expected.json}

REVIEWER OUTPUTS (one per run):
{json array of the RUNS verdicts}

For EACH run decide pass/fail:
- The resolved `verdict` must be in expected.verdict_in.
- If expected.must_catch is set: the reviewer must have raised a suggestion
  / flag whose meaning matches must_catch (any wording). Missing it = fail.
- If expected.must_not_catch is set: the reviewer must NOT have raised that
  (a false positive). Raising it = fail.
- If expected.presentation_expect is set: also grade the reviewer's
  `presentation` object. (a) `presentation.layout` must equal
  `presentation_expect.layout`. (b) at least one numeral appearing in
  `presentation.tweet_title`, `presentation.hero_stat.value`, or
  `presentation.refutation.*` must be a member of
  `presentation_expect.hero_numbers_any_of` (the reviewer only needs to
  hit one of the allowed numbers — it picks its own framing). (c) if
  `presentation_expect.must_not_contain_numbers_absent_from_structure`
  is `true`, every numeral in those same fields must trace back to a
  number literally present in the fixture's `structure.json` (verbatim
  or as a ratio of two such numbers) — a numeral that isn't grounded is
  a fail, mirroring the deterministic check `presentation_check.rs`
  applies in production. A missing `presentation` object entirely also
  fails this check.
Return ONLY JSON:
{"runs":[{"pass":bool,"reason":"..."}],"catch_rate":<passes/RUNS>,"summary":"one line"}
```

### 4. Scorecard
Aggregate and print a table, sorted worst catch_rate first:

```
fixture                      check                  expected   catch_rate  status
numbers-not-from-script      faithful-numbers       reject     3/3 (1.00)  ok
live-data-not-frozen         reproducible-inputs    needs-work 1/3 (0.33)  FLAKY
good-simpleqa-200            control                ok         3/3 (1.00)  ok  (no false positive)
...
OVERALL: 16/18 (0.89)   per-check: {faithful-numbers:1.00, reproducible-inputs:0.33, ...}
```

Then a short prose summary:
- **Missed** (catch_rate 0): the prompt fails this case — quote the judge's reason.
- **Flaky** (0 < rate < 1): non-deterministic; quote one failing reason. Flaky is
  a signal the prompt's instruction for that check is weak/ambiguous.
- **False positives**: any control fixture (`must_not_catch`) the reviewer tripped.

End with one actionable line per weak check — what to tighten in the
prompt — so the next edit is targeted. Do **not** edit the prompt yourself
unless the user asks.

## Notes
- This tests the prompt on **Claude** (the sub-agent model), a fast proxy
  for prompt-wording quality — NOT the production Gemini reviewer. For
  production fidelity, run the candidate prompt through the real reviewer
  separately.
- Keep `RUNS` ≥ 3; a single run hides flakiness, which is itself the most
  useful signal about a shaky instruction.
- To A/B an edit: copy the prompt to a scratch file, edit it, run
  `/eval-reviewer --prompt <scratch>`, and diff the scorecard against a
  baseline run on the committed prompt.
