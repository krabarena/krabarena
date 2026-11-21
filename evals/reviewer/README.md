# KrabReviewer prompt eval

A local, Claude-Code-driven regression eval for the **LLM judgment layer**
of the KrabReviewer prompt
(`crates/krabarena-api/src/services/claim_review_prompt.txt`).

It does **not** test the deterministic pre-filter (structure.json
validity, the four required sections) — that's pure Rust and covered by
unit tests in `review_service.rs`. This harness tests the part only a
model can judge: does the reviewer catch the substance problems on bad
claims and stay quiet on good ones?

The fixtures are self-contained and hand-authored — each one is a small,
labeled example of a specific pattern, carrying the verdict a good
reviewer should reach and a plain-language description of what it must
catch. Nothing here depends on any external document.

## The checks a fixture can exercise

Each fixture targets one `check` (the failure mode under test):

- `self-contained-runner` — the bundle must ship the runner that produces
  the result, not point at an external repo. **Gating** (Floor 1 → warn).
- `faithful-numbers` — the headline numbers must come out of the shipped
  script + data, not be asserted in prose or derived by author-chosen
  arithmetic the script never emits. **Gating** (Floor 2 → warn).
- `reproducible-inputs` — the exact inputs must be obtainable later (a
  public dataset + seed, or shipped in the bundle); a live snapshot that
  drifts and was never frozen fails. **Info nudge** since 0.18.7 (surfaced,
  not gating) — verdict may stay `pass`.
- `title` — the title must state the result and its margin, not describe
  what the claim is. **Info** (engagement nudge).
- `borderline-disclosed-rerank` — a self-contained, honestly-labelled
  re-rank of published data (re-sort script + pinned inputs both shipped)
  must NOT be hard-rejected on Floor 1/2. Control for over-blocking.
- `control` — a clean, sound claim the reviewer must NOT false-positive on.
- `presentation` — the reviewer must populate Section F's `presentation`
  object (layout, tweet_title, tweet_annotation, hero_stat, and —
  when `structure.json.hypothesis.verdict == "refutes"` — `refutation`)
  and the numbers in it must be grounded in `structure.json`. **Info**
  (the object is best-effort decoration; a missing/dropped block doesn't
  gate the verdict, but a wrong layout or a hallucinated numeral is a
  quality miss the judge should catch).

After the 0.18.7 softening only `self-contained-runner` and
`faithful-numbers` are gating `warn`s; provenance / title are `info`
nudges the reviewer should still raise but that don't block the publish.
So a bad-claim fixture's `verdict_in` is `["warn"]` only for the two
gating checks; the nudge checks use `["pass", "warn"]` and rely on
`must_catch` to confirm the issue was surfaced.

## How it runs

The playbook is `HARNESS.md` in this folder; the `/eval-reviewer` skill
(`.claude/skills/eval-reviewer/SKILL.md`) is a thin shim that reads and
executes it (the shim has to live under `.claude/skills/` for the slash
command to register — everything substantive is here in `evals/`).
For each fixture, the orchestrating agent fans out Claude sub-agents:

1. **reviewer-under-test** — given the candidate prompt as its system
   prompt + the fixture as input, returns the structured verdict JSON.
   Run `N` times (default 3) to surface flakiness.
2. **judge** — compares the reviewer's verdicts to the fixture's
   `expected.json` (fuzzy, semantic — tolerant of id/category wording) and
   returns a per-run pass + a `catch_rate`.

The harness aggregates into a scorecard (per-fixture catch rate, per-check
mean, overall) and flags **flaky** (0 < rate < 1) and **missed** (low
rate) cases. Edit the prompt, re-run, diff the scorecard.

## Fixture format

```
fixtures/<id>/
  claim.md          # the markdown body (# Title / ## Summary / ## Methodology / ## Conclusion)
  structure.json    # optional — the leaderboard sidecar
  artifact_notes.md # plain-English "what you'd see reading the bundle"
                    #   (RUN.md gist, results/*.json contents, where the scorer lives)
                    #   — stands in for a real tarball, and is what the
                    #   self-contained-runner / faithful-numbers checks hinge on
  expected.json     # the label (see below)
```

Fixtures are **form-valid by construction** (they have a structure.json
and the four sections) so the reviewer agent — like production after the
pre-filter — judges *substance*, not form. The one exception is the
`title` fixture, where the title itself is the thing under test.

### `expected.json`

```json
{
  "label": "reject | needs-work | ok",
  "verdict_in": ["warn"],
  "check": "faithful-numbers",
  "must_catch": "what the reviewer MUST flag, in plain language",
  "must_not_catch": "a concern the reviewer must NOT invent (for clean claims); null otherwise",
  "note": "plain description of the real-world pattern this fixture stands in for"
}
```

- `verdict_in` — acceptable resolved-verdict tiers. A bad claim is
  usually `["warn"]`; a clean claim is `["pass", "warn"]` (a stray
  engagement nudge is fine, a substance flag is not).
- `must_catch` — the judge checks the reviewer named *this* concern
  (any wording / category / id).
- `must_not_catch` — for clean claims: the judge fails the run if the
  reviewer invents a substance violation that isn't there (false
  positive). `null` when not applicable.
- `check` — the failure mode this fixture exercises (for the per-check
  scorecard).
- `presentation_expect` _(optional)_ — present only on `check:
  "presentation"` fixtures. `{"layout": "h2h|race|frontier|myth_buster",
  "hero_numbers_any_of": ["3.5", "628"], "must_not_contain_numbers_absent_from_structure": true}`.
  The judge additionally grades the candidate's `presentation` object
  against it: the emitted `layout` must match, at least one numeral in
  `tweet_title` / `hero_stat.value` / `refutation.*` must be drawn from
  `hero_numbers_any_of` (any one is fine — the reviewer picks its own
  hero framing), and — when the flag is `true` — no numeral anywhere in
  those fields may be absent from `structure.json` (the same grounding
  rule `presentation_check.rs` enforces deterministically in
  production; the judge is checking the LLM's *choice* of numbers, not
  re-deriving the Rust check).

## Scope

Covered (LLM-judged): the substance and quality checks above, plus the
title check. Not covered here (deterministic → Rust tests): structure.json
validity and the four-section presence check.

## Extending

Add a `fixtures/<id>/` folder with the four files. Keep the claim short — a
few sentences per section is enough; the judgment hinges on
`artifact_notes.md` + the claim's framing, not length. Re-run
`/eval-reviewer`.
