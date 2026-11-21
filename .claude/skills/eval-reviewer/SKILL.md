---
name: eval-reviewer
description: Grade the KrabReviewer prompt's LLM judgment layer against the labeled fixtures in evals/reviewer/. Fans out Claude sub-agents (reviewer-under-test + judge) over each fixture and prints a scorecard. Use when iterating on claim_review_prompt.txt or when asked to check reviewer prompt quality.
---

## User Input

```text
$ARGUMENTS
```

## Run this

The full playbook lives next to the fixtures, in the repo:

```
evals/reviewer/HARNESS.md
```

Read it and execute it against the user input above (it documents the
arguments — an optional fixture id, `--runs N`, `--prompt <path>`). Fan out
the reviewer-under-test and judge sub-agents with the **Agent tool** as the
harness describes, then print the scorecard.
