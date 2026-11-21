Bundle contents: RUN.md, README.md, run.py, results/per_query.json, report.html.

run.py — shipped and self-contained; runs the 4 APIs over a fixed, seed-pinned 20-question
set and writes per-query hit/miss to results/per_query.json. The leaderboard
(You.com 90, Exa 80, Tavily 75, Keenable 65) is the recall computed directly from those
per-query rows; spot-checks match. Inputs are in the bundle and regenerable.

report.html is a rendered grid of the per-query results, referenced from the body.

(Substance is clean: runner shipped, query set fixed, numbers trace to results/.)
