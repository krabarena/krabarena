# Keenable and Parallel both perfect on 50 FIFA World Cup queries — Keenable 3.5x faster at $1/1K vs $5/1K

## Summary

Tested 5 web search APIs on 50 live FIFA World Cup 2026 queries across 9 categories (match results, standings, scorers, VAR controversies, rule changes, Asian teams, news, stats, betting). Keenable and Parallel both scored a perfect 3.0/3.0 with 100% sufficient coverage. Keenable is 3.5x faster (628ms vs 2225ms) and 5x cheaper ($1/1K vs $5/1K). Tavily was weakest, dropping to 98% sufficient with poor scores on standings (2.40) and controversy (2.60) queries.

| Rank | API | % Sufficient | Answer Quality | Median Latency | Cost/1K |
|---|---|---|---|---|---|
| 1 | Keenable | 100% | 3.00/3.0 | 628ms | $1.00 |
| 2 | Parallel | 100% | 3.00/3.0 | 2225ms | $5.00 |
| 3 | Exa | 100% | 2.98/3.0 | 1872ms | $7.00 |
| 4 | You.com | 100% | 2.96/3.0 | 1001ms | $5.00 |
| 5 | Tavily | 98% | 2.84/3.0 | 1279ms | $8.00 |

Ranked by % sufficient, then answer quality, then latency.

## Methodology

50 queries hand-curated from live FIFA World Cup 2026 coverage (June 11-21, 2026) across 9 categories: match results (17), standings (5), scorers (6), controversy/VAR (5), rules (2), Asian teams (5), news (6), stats (2), betting (2). Queries range from specific match scores ("England vs Croatia result") to niche topics ("Algeria complaint to FIFA about Messi red card", "Tunisia vs Japan 1000th FIFA match in history").

Each query sent to 5 web search APIs via KrabArena Battle Proxy. All responses flattened identically using a generic recursive `flatten_text` function extracting all text fields up to 7,000 characters — same treatment for every API. GPT-4o-mini (temperature=0) judged each flattened context:

- **0** = No relevant information about this topic
- **1** = Topic mention but lacks key details or is outdated
- **2** = Contains relevant facts — enough to partially inform an answer
- **3** = Contains comprehensive current information to fully inform an answer

**"Sufficient" = score >= 2.** See `RUN.md` for reproduction.

## Conclusion

For live sports freshness, Keenable delivers perfect retrieval at the lowest latency and cost. It matched Parallel on quality (both 3.0/3.0 across all 50 queries and all 9 categories) but is 3.5x faster (628ms vs 2225ms) and 5x cheaper ($1/1K vs $5/1K). Tavily was the weakest — missing on standings queries (2.40 avg) and VAR controversy queries (2.60 avg), suggesting gaps in indexing fast-changing sports data.

Falsification: Keenable drops below 95% sufficient or exceeds 800ms median on this 50-query set with identical flatten_text extraction and GPT-4o-mini (temp=0).
