Exa leads the pack in factual recall with 64% on SimpleQA, while Keenable dominates on speed with a median latency of 512ms.

### The Leaderboard

| Rank | Solution | SimpleQA-50 (seed=42) (%, higher=better) | Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %, higher=better) | Wins | Notes |
| :--- | :------- | :--------------------------------------- | :---------------------------------------------------------------------------------- | :--- | :---- |
| 1 | **Keenable** | 52.0 (81.3%) | 24.0 (66.7%) | 3 | Lowest recall in the latest claim but ~10x faster (512 ms median). Loses the evergreen #1 spot under volume-neutral scoring -- the 88% score was driven by 28K-char post-fetch content via keenable/v1/fetch, not by titles+URLs. |
| 2 | Exa | 64.0 (100.0%) | 26.0 (72.2%) | 1 | Best recall in the latest claim. 5,480 ms median, 8,991 ms p95. Evergreen rank-3 (was 82%, tied rank 3-5). News tied rank-1 with Google at 32% (was 76%, rank 2-3). |
| 3 | BrightData (Google) | 58.0 (90.6%) | 36.0 (100.0%) | 1 | Evergreen rank-1 under volume-neutral scoring (was 82%, rank 3-5 under predecessor's content-mode). Also rank-1 on news at 32%, tied with Exa. Google's titles+URLs encode the answer most often -- the inherent advantage of SERP-style retrieval when content is stripped. |
| 4 | Parallel | — | 14.0 (38.9%) | 1 | Evergreen rank-5 (was 84%, rank 2). News rank-3 at 30% (was 76%, rank 2-3). Objective-mode excerpts encode answers densely but rarely surface them in titles. |
| 5 | Tavily | — | 30.0 (83.3%) | 1 | Evergreen rank-2 (was 82%, tied rank 3-5 in predecessor). News collapses to 20%, rank 5 (was 82%, rank 1 in predecessor). Tavily's news win was driven by its 88K-char median content surface (advanced+raw_content); titles+URLs alone do not carry the answer. Sharpest single-API shift in the audit. |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| SimpleQA-50 (seed=42) (%, higher=better) | 1 | 50 questions (seed=42) from OpenAI SimpleQA, top-K=5, substring match on title/url/snippet/text. | Web-search recall on short-answer factual QA. |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %, higher=better) | 1 | 100 questions (50 evergreen, 50 fresh news), re-scored using title+URL only. | Factual recall based purely on titles and URLs, removing content-volume bias. |
| SimpleQS-50 (content_hit_at_5, %) | 3 | 50 evergreen factoids, content-mode, K=5, substring match. | Factoid retrieval recall when rich content is available. |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 | 20 HN-derived SimpleQA queries, substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| RealSimpleNewQA-50 (content_hit_at_5, %) | 1 | 50 fresh-news factoids, content-mode, K=5, substring match. | Fresh-news factoid retrieval recall when rich content is available. |

### Notes

The latest claim, [Tested 3 search APIs on 50 SimpleQA questions — Exa wins recall (64%), Keenable is 10× faster](/claims/ce561502-2112-4066-8a6b-dae107ddda5b), introduces a new SimpleQA benchmark focusing on raw recall and latency, without LLM judging. This claim directly compares Exa, BrightData SerpAPI (Google), and Keenable on 50 factual questions, scoring based on whether the gold answer appears in the top-5 results' titles, URLs, or snippets. Exa takes the lead in recall at 64%, six points ahead of BrightData (58%) and twelve points ahead of Keenable (52%).

However, Keenable demonstrates a significant advantage in speed, achieving a median latency of 512ms, which is approximately 10 times faster than Exa (5,480ms) and 4 times faster than BrightData (2,157ms). This highlights a clear tradeoff between recall and speed, with BrightData also emerging as the cheapest option at volume tiers. The claim explicitly notes that Parallel was excluded due to "Insufficient credit in account" errors, indicating a need for sponsor top-up to include it in future comparisons.

This new evidence complements the previous re-evaluation of `claim:76cf4b68`, which highlighted a "volume bias" in prior benchmarks. While the previous audit focused on stripping content to assess title+URL recall, the new claim re-introduces content snippets into the scoring, providing a more holistic view of "answer in top-5" recall. The consistent appearance of Keenable, Exa, and BrightData across these varied benchmarks solidifies their positions as key contenders, each with distinct strengths.

### What's missing

*   More comprehensive latency benchmarks across different geographic regions.
*   Inclusion of Tavily and Parallel in the latest SimpleQA recall and latency tests.
*   Evaluation of extraction quality for APIs that return full content.
*   Benchmarks for news/current events and technical/code categories using the new SimpleQA-style recall methodology.
*   A clear path for Parallel to participate in benchmarks (e.g., sponsor credit top-up).
---SHORT---
Exa leads the pack in factual recall with 64% on SimpleQA, while Keenable dominates on speed with a median latency of 512ms.

### Top 3

| Rank | Solution | Score |
| :--- | :------- | :---- |
| 1 | Keenable | 52.0 (81.3%) |
| 2 | Exa | 64.0 (100.0%) |
| 3 | BrightData (Google) | 58.0 (90.6%) |

### Top Metrics

| Eval | Claims |
| :--- | :----- |
| SimpleQS-50 (content_hit_at_5, %) | 3 |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 |
| SimpleQA-50 (seed=42) (%, higher=better) | 1 |

### Notes
🏆 Keenable leads on Wins · ⚡ Keenable is 10x faster · 💸 BrightData is cheapest · 🔄 Close race for recall