#!/usr/bin/env python3
"""Build a `Cumulative across all claims in this battle` context block
from a battle's claim_structure rows. Same logic the server uses to
inject the block into both full + incremental user messages.

Used to (a) reproduce the v4 prompt's expected input shape from a
battle dump, (b) iterate on prompt revisions against new battles
without touching server code.

Inputs:
  - A list of `claim_structure` JSON values, one per claim, on stdin
    (one JSON object per line). Anything not a structure is skipped.

Output:
  - The markdown table block to stdout, ready to splice into the
    user message before the per-claim evidence.
"""
import json
import sys


def main() -> int:
    wins = {}
    appearances = {}
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            s = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(s, dict):
            continue
        seen_in_claim = set()
        for row in s.get("leaderboard", []) or []:
            sol = (row.get("solution") or "").strip()
            if sol and sol not in seen_in_claim:
                appearances[sol] = appearances.get(sol, 0) + 1
                seen_in_claim.add(sol)
        for w in s.get("winners", []) or []:
            w = w.strip()
            if w:
                wins[w] = wins.get(w, 0) + 1

    print("Cumulative across all claims in this battle (server-computed):")
    print("| Solution | Wins | Appearances |")
    print("| :------- | :--- | :---------- |")
    keys = sorted(
        set(wins.keys()) | set(appearances.keys()),
        key=lambda s: (-wins.get(s, 0), -appearances.get(s, 0), s.lower()),
    )
    for sol in keys:
        print(f"| {sol} | {wins.get(sol, 0)} | {appearances.get(sol, 0)} |")
    return 0


if __name__ == "__main__":
    sys.exit(main())
