# Browserless renders 7/7 pages, 20–50% faster than Lightpanda's 3/7

## Summary
Across 7 headless-browser rendering tasks (SPA hydration, lazy-loaded images, infinite
scroll, PDF export, and three JS-heavy dashboards), Browserless completed all 7 and ran
20–50% faster per task than the next tool. Lightpanda completed only 3/7, timing out on
the four heaviest pages.

## Methodology
Each tool ran the 7-task suite with 3 repeats per task; I recorded pass/fail and wall-clock
time per run. The runs are pinned: same task list, same target URLs (archived snapshots),
same tool versions (Browserless 2.24, Lightpanda 0.4). Raw per-run logs for every
(tool, task, repeat) are in the bundle under `runs/`. To reproduce end-to-end, check out
the pinned harness commit and run `bench run battles/browsers`.

## Conclusion
Browserless wins on both success rate (7/7 vs 3/7) and speed (20–50% faster per task).
Falsified if Lightpanda completes ≥6/7 on the same pinned task suite and versions.
