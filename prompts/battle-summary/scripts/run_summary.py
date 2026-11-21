#!/usr/bin/env python3
"""
Local Gemini harness — replays both summary modes (full top-10 vs
incremental single-claim) using the same prompt as the server, so we
can diff outputs and iterate on the prompt without a deploy loop.

Inputs (already on disk from the prod dry-run dump):
  /tmp/regen-system-prompt.md     — battle_summary_prompt.txt content
  /tmp/regen-user-message.md      — the full user message (10 claims)
  /tmp/regen-previous-summary.md  — the previous editorial

Outputs:
  /tmp/run-full.md                — full-mode (top-10) output
  /tmp/run-incremental.md         — incremental-mode (1 claim) output

Usage:
  GEMINI_API_KEY=... python3 /tmp/run_summary.py [--prompt PATH]

The optional --prompt flag swaps in a candidate prompt for iteration.
Default reads /tmp/regen-system-prompt.md.
"""
import argparse
import json
import os
import re
import sys
import urllib.request

GEMINI_URL_FMT = (
    "https://generativelanguage.googleapis.com/v1beta/models/"
    "gemini-2.5-flash:generateContent?key={key}"
)

def slice_to_first_claim(user_msg: str) -> str:
    """Replicate the incremental user_message: same prefix
    (battle title + description + previous summary), then JUST claim 1
    of the cross-claim block, relabeled as `1 of 1`. If the input
    already contains only a single claim block (Claim 1 of 1, no
    Claim 2 marker), it's already incremental-shaped — pass through
    unchanged."""
    m1 = re.search(r"^--- Claim 1 of \d+ ", user_msg, re.MULTILINE)
    m2 = re.search(r"^--- Claim 2 of \d+ ", user_msg, re.MULTILINE)
    if not m1:
        sys.exit("user_message structure unexpected — couldn't locate claim 1 boundary")
    if not m2:
        # Already incremental-shaped (single claim).
        return user_msg
    prefix = user_msg[: m1.start()]
    # Trim down the prefix's "top 10 claims" wording → 1 claim
    prefix = re.sub(
        r"top \d+ claim[s]? on this Battle",
        "1 claim on this Battle (incremental update mode — only the most-recent post)",
        prefix,
    )
    claim_block = user_msg[m1.start(): m2.start()]
    # Rewrite "Claim 1 of 10" → "Claim 1 of 1"
    claim_block = re.sub(r"--- Claim 1 of \d+ ", "--- Claim 1 of 1 ", claim_block, count=1)
    return prefix + claim_block

def call_gemini(api_key: str, system_prompt: str, user_msg: str) -> str:
    url = GEMINI_URL_FMT.format(key=api_key)
    body = json.dumps({
        "system_instruction": {"parts": [{"text": system_prompt}]},
        "contents": [{"role": "user", "parts": [{"text": user_msg}]}],
        "generationConfig": {"temperature": 0.3},
    }).encode("utf-8")
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as resp:
        payload = json.loads(resp.read())
    return payload["candidates"][0]["content"]["parts"][0]["text"].strip()

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--prompt", default="/tmp/regen-system-prompt.md")
    ap.add_argument("--user-message", default="/tmp/regen-user-message.md")
    ap.add_argument("--full-out", default="/tmp/run-full.md")
    ap.add_argument("--inc-out", default="/tmp/run-incremental.md")
    ap.add_argument(
        "--mode",
        choices=("both", "full", "incremental"),
        default="both",
        help="Skip one mode if iterating fast.",
    )
    args = ap.parse_args()

    api_key = os.environ.get("GEMINI_API_KEY")
    if not api_key:
        sys.exit("GEMINI_API_KEY not set")

    system_prompt = open(args.prompt).read()
    full_user_msg = open(args.user_message).read()
    inc_user_msg = slice_to_first_claim(full_user_msg)

    print(
        f"prompt: {len(system_prompt):>5} chars  full_user: "
        f"{len(full_user_msg):>6} chars  inc_user: {len(inc_user_msg):>6} chars",
        file=sys.stderr,
    )

    if args.mode in ("both", "full"):
        print("calling Gemini in full mode…", file=sys.stderr)
        full_out = call_gemini(api_key, system_prompt, full_user_msg)
        with open(args.full_out, "w") as f:
            f.write(full_out)
        print(f"  → {args.full_out} ({len(full_out)} chars)", file=sys.stderr)

    if args.mode in ("both", "incremental"):
        print("calling Gemini in incremental mode…", file=sys.stderr)
        inc_out = call_gemini(api_key, system_prompt, inc_user_msg)
        with open(args.inc_out, "w") as f:
            f.write(inc_out)
        print(f"  → {args.inc_out} ({len(inc_out)} chars)", file=sys.stderr)

    return 0

if __name__ == "__main__":
    sys.exit(main())
