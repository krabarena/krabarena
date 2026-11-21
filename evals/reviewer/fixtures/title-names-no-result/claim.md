# Per-query visual report for the WebSearch-SimpleQA-20 claim — see what each API matched and missed

## Summary
A per-query breakdown showing, for each of 20 SimpleQA questions, which of 4 search APIs
returned the right page. You.com leads at 90%; Keenable is last at 65% but 2× faster.

## Methodology
Ran 4 APIs over the same 20 questions, recorded hit/miss per query, and rendered a grid.
Seed-fixed query set; per-query results in `results/per_query.json`.

## Conclusion
You.com tops the set at 90%; Keenable trails at 65% but is 2× faster. Falsified if a
re-run reorders the top spot on the same 20 questions.
