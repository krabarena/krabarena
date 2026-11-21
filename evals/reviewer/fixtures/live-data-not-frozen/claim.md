# Python dominates importable AI libraries — 60% of the field by stars (n=962)

## Summary
Across 962 engineer-importable AI libraries, Python accounts for 60% of the field weighted
by GitHub stars; TypeScript is a distant second at 20%, Rust third at 8%. Python is where
the importable AI ecosystem lives.

## Methodology
`run.py --fetch` queries the GitHub Search + Contents API across 18 curated topics
(`topics.json`), keeps repos at or above 1000 stars, drops forks and the exclusion list,
and classifies each repo's primary language, then sums stars per language and computes each
language's share of the total. The run on 2026-05-11 pulled ~1,325 repos, 962 of which
passed the allowlist/exclusion filters. All config — topics, allowlist, seeds, exclusions —
is shipped in the bundle so the selection rules are inspectable.

## Conclusion
Python leads at 60% of stars across n=962 importable libraries; TS 20%, Rust 8%. Falsified
if a re-run shows another language above 50% of the field by stars.
