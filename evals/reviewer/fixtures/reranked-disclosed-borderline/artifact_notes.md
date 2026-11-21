Bundle contents (full listing):
  RUN.md
  README.md
  rank.py
  data/clickhouse_66c22a.json     (header: run_date 2026-02-…)
  data/duckdb_66c22a.json
  data/snowflake_medium.json      (header: run_date 2022-08-…)

rank.py — shipped and self-contained. It reads the three result files under data/, recomputes
the per-system geomean latency, sorts ascending, and prints the leaderboard. It fetches
nothing and provisions nothing; it runs end-to-end from the bundle.

data/*.json — the ClickBench result files, copied verbatim from ClickHouse/ClickBench at the
pinned commit 66c22a and shipped in the bundle (so the inputs are frozen as of that commit).
Recomputing from them reproduces the leaderboard (ClickHouse 134, DuckDB 290, Snowflake
Medium 410). The per-file headers show different run dates: the Snowflake file is a 2022 run,
the ClickHouse file a 2026 run.

RUN.md:
  python rank.py    # recomputes geomean from data/*.json and prints the ranking
  # Re-ranks published ClickBench files; no services are provisioned, nothing is run fresh.
