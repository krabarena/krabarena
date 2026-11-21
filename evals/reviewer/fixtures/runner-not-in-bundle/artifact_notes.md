Bundle contents (full file listing):
  RUN.md
  README.md
  result.json
  runs/browserless/<task>/<repeat>/log.jsonl   (~45 files)
  runs/lightpanda/<task>/<repeat>/log.jsonl     (~45 files)

result.json — the aggregated leaderboard (Browserless 7, Lightpanda 3) plus per-task wall
times. The runs/ tree holds the raw per-run logs the aggregate was computed from; spot-checks
match (the logs are real, the benchmark genuinely ran).

There is no .py / .js / .ts / .sh runner anywhere in the bundle — no script that drives the
tools, loads the task list, or computes result.json from the logs.

RUN.md (verbatim):
  ## Reproduce
  git clone https://github.com/krabarena/krabarena-bench
  cd krabarena-bench && git checkout abc123
  bench run battles/browsers       # writes result.json + runs/

So the harness (`bench`, the task list, the scorer) lives in the external krabarena-bench
repo at commit abc123, not in this bundle.
