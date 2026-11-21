# Parallel ties Exa at the top of a 200-question SimpleQA rerun

## Summary
Reproduced you.com's SimpleQA-200 eval end-to-end through the Battle Proxy. Parallel
(92.0) and Exa (91.5) tie at the top; the gap from #1 to #5 is 26 points. n=200.

## Methodology
Cloned `youdotcom-oss/web-search-api-evals@HEAD`, replaced each per-provider sampler with
one proxy sampler, ran `eval_runner.py --datasets simpleqa --limit 200` with seed=42 (so
every API grades the same 200 rows of OpenAI SimpleQA). LLM judge, ±2-point judge noise.
Tavily was excluded (it failed under load); the you.com endpoint tested is the basic Web
Search one, not livecrawl.

## Conclusion
The top reproduces published you.com numbers within ~2 points (Parallel 92.0 vs 89.78,
Exa 91.5 vs 90.06). The 26-point #1–#5 gap absorbs the ±2-point judge noise. Falsified if
a re-run with seed=42 reorders the top 5.
