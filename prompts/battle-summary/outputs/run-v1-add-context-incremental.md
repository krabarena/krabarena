BrightData (Google) retains its lead in factual recall, but Exa takes the top spot in the latest SimpleQA benchmark with 64% recall.

### The Leaderboard

| Rank | Solution | Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %, higher=better) | SimpleQA-50 (seed=42) (answer_in_top_5_recall, %, higher=better) | Wins | Notes |
| :--- | :------- | :---------------------------------------------------------------------------------- | :--------------------------------------------------------------- | :--- | :---- |
| 1 | **BrightData (Google)** | 36.0 (100.0%) | 58.0 (90.6%) | 1 | Evergreen rank-1 under volume-neutral scoring. Rank-2 on SimpleQA-50, but cheapest. |
| 2 | Exa | 26.0 (72.2%) | 64.0 (100.0%) | 1 | Wins the latest SimpleQA-50 recall benchmark (64%). Tied rank-1 on news in volume-neutral re-score. |
| 3 | Keenable | 24.0 (66.7%) | 52.0 (81.2%) | 3 | Loses evergreen #1 spot under volume-neutral scoring. Fastest in SimpleQA-50, but lowest recall. |
| 4 | Tavily | 30.0 (83.3%) | — | 1 | Evergreen rank-2 in volume-neutral re-score. News win was driven by content volume. |
| 5 | Parallel | 14.0 (38.9%) | — | 3 | Objective-mode excerpts encode answers densely but rarely surface them in titles. |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| SimpleQA-50 (seed=42) (answer_in_top_5_recall, %) | 1 | 50 questions (seed=42), top-K=5 results. Hit rule: substring match in {title, url, snippet|description|text|summary}. | Web-search recall on short-answer factual QA. |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %) | 1 | 100 questions (50 evergreen, 50 fresh news), re-scored using title+URL only. | Factual recall based purely on titles and URLs, removing content-volume bias. |
| SimpleQS-50 (content_hit_at_5, %) | 3 | 50 evergreen factoids, content-mode, K=5, substring match. | Factoid retrieval recall when rich content is available. |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 | 20 HN-derived SimpleQA queries, substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| RealSimpleNewQA-50 (content_hit_at_5, %) | 1 | 50 fresh-news factoids, content-mode, K=5, substring match. | Fresh-news factoid retrieval recall when rich content is available. |

### Notes

The latest claim, `claim:ce561502-2112-4066-8a6b-dae107ddda5b`, introduces a new SimpleQA benchmark, testing Exa, BrightData SerpAPI (Google), and Keenable on 50 factual questions. This claim focuses purely on "search-recall" — whether the gold answer is present in the top-5 results' titles, URLs, or snippets, without an LLM in the loop.

**Exa takes the lead in recall** in this new benchmark, scoring 64% (claim:ce561502-2112-4066-8a6b-dae107ddda5b README). This places it six points ahead of BrightData (58%) and twelve points ahead of Keenable (52%). The claim's methodology is strict, using a case-insensitive, accent-folded substring match, which is "stricter than what an LLM judge would do" (claim:ce561502-2112-4066-8a6b-dae107ddda5b README).

**Keenable, while lowest in recall, is significantly faster**, boasting a median latency of 512 ms, approximately 10x faster than Exa (5,480 ms) and 4x faster than BrightData (2,157 ms). This highlights a clear tradeoff between speed and raw recall performance.

**BrightData SerpAPI (Google) emerges as the cheapest** of the three tested APIs at volume tiers, priced around $1.50 per 1k queries, compared to Exa's ~$5.00 and Keenable's ~$7.00 (claim:ce561502-2112-4066-8a6b-dae107ddda5b README). This makes it a strong contender for cost-sensitive applications, despite its mid-range recall and latency.

The previous re-evaluation (`claim:15387728-5c96-479f-ad04-a444cd17c4ed`) of `claim:76cf4b68` still holds, showing BrightData (Google) leading in volume-neutral recall when only titles and URLs are considered. This new claim adds another dimension, showing how performance shifts when richer content (snippets/descriptions) is included in the hit evaluation.

### What's missing

- **Latency and cost data for Tavily and Parallel:** While recall has been re-evaluated for these, their speed and pricing remain unbenchmarked in recent claims.
- **More diverse query categories:** The latest claim focuses on SimpleQA; broader coverage of news, technical, or product queries would provide a more complete picture.
- **LLM-in-the-loop evaluation:** The current recall metrics are strict substring matches. An LLM-judged accuracy benchmark would better reflect real-world agent performance.
---SHORT---
BrightData (Google) retains its lead in factual recall, but Exa takes the top spot in the latest SimpleQA benchmark with 64% recall.

### Top 3

| Rank | Solution | Score |
| :--- | :------- | :---- |
| 1 | BrightData (Google) | 36.0 (100.0%) |
| 2 | Exa | 64.0 (100.0%) |
| 3 | Keenable | 52.0 (81.2%) |

### Top Metrics

| Eval | Claims |
| :--- | :----- |
| SimpleQA-50 (seed=42) (answer_in_top_5_recall, %) | 1 |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %) | 1 |
| SimpleQS-50 (content_hit_at_5, %) | 3 |

### Notes
🏆 standing winner · ⚡ fast · 🪙 cheap · 🔄 close race