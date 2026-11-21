Bundle contents: RUN.md, README.md, fifa_run.py (the harness), structure.json, and
per-API raw response logs under `results/<api>.json`.

fifa_run.py — sends each of the 50 hand-curated FIFA World Cup 2026 queries to all 5 web
search APIs via the Battle Proxy, flattens every response with one shared `flatten_text`
function (same 7000-char cap for every API), and has GPT-4o-mini (temp=0) score the
flattened context 0-3 for sufficiency. Runs end-to-end from the bundle; no external
services beyond the proxy.

results/<api>.json — per-query score, latency, and sufficiency flag. Aggregated:
Keenable 100% sufficient / 3.00 quality / 628ms p50 / $1.00 per 1K queries; Parallel ties
Keenable on quality (3.00, 100% sufficient) but at 2225ms and $5.00 per 1K; Exa 2.98,
You.com 2.96, Tavily weakest at 2.84 (98% sufficient, misses on standings and VAR
controversy queries).

RUN.md discloses the exact query set, category breakdown, and the judge prompt; the
result is regenerable with `python3 fifa_run.py`. structure.json carries per-row
`latency_p50_ms` and `cost_per_1k_usd` metrics alongside the quality score, so the
Keenable/Parallel quality tie and the latency/cost gap that breaks it are both
machine-readable, not just prose.
