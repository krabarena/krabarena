# Re-ranked ClickBench with Snowflake Medium — ClickHouse still leads at 134ms

## Summary
Re-ranking the public ClickBench result files, ClickHouse still leads cold-run query
latency at 134ms geomean; Snowflake Medium lands mid-pack at 410ms. No new benchmark was
run — this re-sorts already-published results and is upfront about that.

## Methodology
The artifact clones no services and provisions no cloud. It analyzes three native ClickBench
result files copied verbatim from the public `ClickHouse/ClickBench` repo at commit 66c22a,
shipped in the bundle under `data/`. `rank.py` recomputes the geomean latency per system from
those files and sorts. This is explicitly NOT a substitute for a fresh same-day, same-hardware
three-system rerun.

## Conclusion
On the published numbers pinned at commit 66c22a, ClickHouse leads at 134ms geomean.
Caveat: the Snowflake file is from a 2022 run and the ClickHouse file from a 2026 run, so the
comparison mixes time windows and hardware — read it as best-public-data, not a clean
head-to-head. Falsified if a fresh same-hardware rerun reorders the top.
