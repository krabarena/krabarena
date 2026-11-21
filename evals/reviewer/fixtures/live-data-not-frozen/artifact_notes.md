Bundle contents (full listing):
  RUN.md
  README.md
  run.py
  topics.json            (18 topic search strings)
  library_allowlist.json
  seeds.json
  exclusions.json
  data/fetch_meta.json    ({"fetched_at": "2026-05-11T09:14:02Z", "per_topic_counts": {...}})

run.py — with `--fetch`, calls the GitHub Search API for each topic, then the Contents API
per repo, filters by the allowlist/exclusions and min_stars=1000, reads each repo's
**current** star count from the API response, sums stars per language, and prints the
language shares. Without `--fetch` it errors out ("no cached snapshot found").

What is NOT in the bundle: the fetched repo list itself — the 962 repos with their names and
star counts as of the 2026-05-11 run. data/fetch_meta.json records only the timestamp and
per-topic counts, not the rows. So the leaderboard was computed from live API responses at
run time; only the query/config that selects them is shipped.

RUN.md:
  python run.py --fetch     # hits the live GitHub API and prints language shares
