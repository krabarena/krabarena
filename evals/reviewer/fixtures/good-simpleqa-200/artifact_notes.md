Bundle contents: RUN.md, README.md, eval_runner.py (the harness), results/<provider>.json
(per-question grades for each API), and judge.py (the LLM-judge scorer).

eval_runner.py — clones nothing at run time; it is the shipped harness. It loads the
seed-pinned 200-row SimpleQA sample, calls each API through one proxy sampler, and writes
results/<provider>.json. judge.py scores each answer with the LLM judge. Both run end-to-end
from the bundle.

results/<provider>.json — per-question grades. The per-provider accuracy computed from these
is Parallel 92.0, Exa 91.5, Brave 78.0, SerpAPI 70.0, Keenable 66.0, matching the
leaderboard. These also land within ~2 points of you.com's published table (Parallel 92.0
vs published 89.78, Exa 91.5 vs 90.06).

RUN.md / body disclose: Tavily excluded (failed under load), the you.com endpoint tested is
basic Web Search (not livecrawl), LLM judge ±2-point noise. Dataset is OpenAI SimpleQA at
seed=42, --limit 200 — the same 200 rows for every API, regenerable with one command.
n=200; the #1–#5 gap is 26 points.
