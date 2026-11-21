# Battle-summary prompt iteration log

Reproduce + improve the KrabReviewer "executive summary" prompt
without going through a full deploy loop. Everything here is meant
to be replayable locally with a Gemini API key.

## Why this directory exists

The summary prompt has two consumers (since 0.8.15):

- `regenerate_summary` **full mode** — top-10 claims by `$ effort`
  fed in; runs on demand from `/v1/battles/{id}/regenerate-summary`
- `regenerate_summary` **incremental mode** — only the newly-posted
  claim fed in; runs as a `tokio::spawn` after every claim post

Both modes share `crates/krabarena-api/src/services/battle_summary_prompt.txt`.

The bug we caught + fixed: the prompt's `### The Leaderboard` table
includes a `Wins` column that was being miscounted in both modes —
incremental could only see one claim's winners, full was
over-coalescing solution-name variants. The fix is one round-trip
of prompt + server-side change:

1. The server pre-computes `(Wins, Appearances)` per coalesced
   solution across **every** `claim_structure` in the battle.
2. The user message gets a small markdown table block injected
   before the per-claim evidence ("Cumulative across all claims in
   this battle (server-computed)").
3. The prompt's synthesis rules tell Gemini that block is the only
   authoritative source for `Wins` — it must coalesce variants and
   re-rank by Wins. *But* each row's Notes prose rolls forward
   verbatim from the previous editorial; only new entrants get
   fresh prose.

## Layout

```
inputs/                — frozen reference data (a real battle dump)
  system-prompt-v0-baseline.md            — what shipped at 0.8.21
  user-message-full-10claims.md            — top-10 fed to FULL
  user-message-incremental-1claim-...md    — the v4 incremental shape
  user-message-full-with-cumulative-...md  — same FULL with context block
  previous-summary-stored.md               — what was stored on prod

iterations/            — the prompt evolution
  system-prompt-v1-add-cumulative-context.md
  system-prompt-v2-strengthen-wins-sort.md
  system-prompt-v3-preserve-notes-narrative.md
  system-prompt-v4-step-ordered-final.md   ← ships as battle_summary_prompt.txt

outputs/               — Gemini outputs at each iteration
  run-baseline-full.md / run-baseline-incremental.md
  run-with-context-full.md
  run-v1-add-context-incremental.md
  run-v2-strengthen-wins-sort-incremental.md
  run-v3-preserve-notes-incremental.md
  run-v4-step-ordered-incremental-final.md ← convergence

scripts/
  run_summary.py       — call Gemini with arbitrary prompt + user_msg
  build_context.py     — generate the cumulative-wins context block
```

## Iteration history (what we learned at each step)

### v0 — baseline (what was shipping)

Both modes use the same prompt. Incremental could only see one
claim's winners — `Wins` column ended up `1/1/0/0/0` in incremental.
Full mode over-counted via aggressive name coalescing — `4/4/4/3/2`
when the data only supported `3/3/2/1/1`.

→ `inputs/system-prompt-v0-baseline.md`
→ `outputs/run-baseline-{full,incremental}.md`

### v1 — inject server-computed cumulative-wins context

Server pre-aggregates winners across every `claim_structure` in the
battle into a markdown table. New rule (a-prime) added to the
prompt: this block is the canonical source for Wins; do not recount.

Wins counts now correct. **But the model still kept the previous
summary's rank order** instead of re-ranking by the new Wins
counts, because rule (a) said "don't re-litigate facts the previous
version already established."

→ `iterations/system-prompt-v1-add-cumulative-context.md`
→ `outputs/run-v1-add-context-incremental.md`

### v2 — strengthen Wins-sort to override previous rank

Modified rule (b)'s sort directive: "this RE-RANKS as cumulative
Wins evolves; do NOT preserve the previous summary's rank order
when it conflicts with current Wins counts."

Wins counts and ranking now both correct. **But the Notes column
prose got rewritten from scratch** — the model interpreted the
re-ranking license as permission to also restart each row's
narrative, dropping the carefully-built "was X, now Y" framing.

→ `iterations/system-prompt-v2-strengthen-wins-sort.md`
→ `outputs/run-v2-strengthen-wins-sort-incremental.md`

### v3 — preserve Notes column prose verbatim + append

Strengthened rule (a) with: "The Notes column on each Leaderboard
row in particular MUST preserve the previous editorial's framing
for that solution — the new claim adds a clause to the existing
note, it does NOT replace the prior narrative wholesale."

Narrative continuity restored on rows that survived. **But two new
problems:** the model dropped You.com (which has 2 cumulative wins
→ should be rank-2) and didn't coalesce `Parallel` + `Parallel
(one-shot)` → showed `Wins=1` instead of `3`. The narrative-
preservation rule was over-anchoring on the previous summary's row
set.

→ `iterations/system-prompt-v3-preserve-notes-narrative.md`
→ `outputs/run-v3-preserve-notes-incremental.md`

### v4 — explicit step ordering (final)

Resolved the conflict by replacing implicit-priority rules with
explicit two-step synthesis: **(a-prime)** "Pick the row set" =
top-5 by cumulative Wins (with variant coalescing); **(a)** "Port
the prose forward" = for each chosen row, copy previous Notes
verbatim + append one new clause OR write fresh if new entrant.

All four success criteria met simultaneously:

- ✓ Wins cumulative + correctly coalesced (Keenable=3, Parallel=3,
  You.com=2, BD=1, Exa=1)
- ✓ Re-ranking by Wins (You.com correctly enters at rank-3; Tavily
  correctly drops out of top-5)
- ✓ Notes prose preserved verbatim from previous summary on every
  row that existed before; one new clause appended per row
- ✓ Latest claim properly cited inline (`claim:ce561502-...`)

→ `iterations/system-prompt-v4-step-ordered-final.md`
→ `outputs/run-v4-step-ordered-incremental-final.md`

This is what ships as `battle_summary_prompt.txt`.

## Reproducing

You need a Gemini API key (the same `GEMINI_API_KEY` the server
uses). Run from the repo root:

```bash
# Baseline run — what ships today
GEMINI_API_KEY=... python3 prompts/battle-summary/scripts/run_summary.py \
  --prompt prompts/battle-summary/inputs/system-prompt-v0-baseline.md \
  --user-message prompts/battle-summary/inputs/user-message-full-10claims.md \
  --full-out /tmp/run-full.md \
  --inc-out /tmp/run-inc.md \
  --mode both

# Final v4 incremental — what we converged to
GEMINI_API_KEY=... python3 prompts/battle-summary/scripts/run_summary.py \
  --prompt prompts/battle-summary/iterations/system-prompt-v4-step-ordered-final.md \
  --user-message prompts/battle-summary/inputs/user-message-incremental-1claim-with-cumulative-context.md \
  --inc-out /tmp/run-inc-v4.md \
  --mode incremental
```

`run_summary.py` is intentionally trivial — direct Gemini REST call,
no SDK. If the server prompt changes, drop a new file under
`iterations/`, point the harness at it, and diff outputs.

## Adding a new iteration

1. Copy the most recent prompt to `iterations/system-prompt-vN-<one-line-summary>.md`
2. Edit
3. Run the harness against both modes
4. Save outputs under `outputs/run-vN-<summary>-{full,incremental}.md`
5. Add a section to this README explaining what changed, what it
   fixed, and what new problem it introduced (if any)
6. If converged: copy the new prompt over `crates/krabarena-api/src/services/battle_summary_prompt.txt`
7. If the server-side message construction needs to change too,
   update `crates/krabarena-api/src/services/battle_summarizer.rs::build_user_message`

## Why we don't just trust previous_summary's table

Tempting alternative: in incremental mode, lift the Wins counts
straight out of the previous summary's table and add `+1` to
whichever solution won the new claim. Avoids the extra query +
context block.

It doesn't work because:

- Errors compound — every incremental round trusts the previous
  round's numbers. We literally observed prod's stored summary
  having Wins values *already wrong* (it was last produced by the
  same buggy full pass).
- Doesn't help full mode at all. Full was over-counting via
  aggressive coalescing; that bug doesn't go away if you only fix
  incremental.
- Asks Gemini to extract integers from a markdown table and
  arithmetic them. Gemini *can*, but it's the same low-level
  numerical reasoning that gave us 4/4/4/3/2 in the first place.

The cumulative-wins context block is `COUNT(*)` over
`claim_structure->'winners'` for the battle. Deterministic,
drift-free, computed once per regen call. The model still does
all the prose synthesis — voice, framing, "how the standing has
shifted" — from `previous_summary`. We get **stable narrative AND
correct math**, not one or the other.
