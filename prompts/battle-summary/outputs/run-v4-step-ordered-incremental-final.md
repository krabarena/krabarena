Keenable maintains its lead in cumulative wins, while Exa achieves the highest SimpleQA recall at 64% in the latest benchmark, though at a significant latency cost. The new claim highlights a critical tradeoff between raw recall, speed, and cost, adding new dimensions to the ongoing discussion about search API performance.

### The Leaderboard

| Rank | Solution | SimpleQA-50 Recall (%, higher=better) | Median latency (ms, lower=better) | Cost per 1k queries ($, lower=better) | Volume-neutral recall (%, higher=better) | Wins | Notes |
| :--- | :------- | :------------------------------------ | :-------------------------------- | :------------------------------------ | :--------------------------------------- | :--- | :---- |
| 1 | **Keenable** | 52.0 (81.3%) | 512ms (100%) | $7.00 (21.4%) | 24.0 (66.7%) | 3 | Evergreen rank-4 (was 88%, rank 1 in predecessor). News rank-6 at 18% (was 64%, rank 6). Loses the evergreen #1 spot under volume-neutral scoring -- the 88% score was driven by 28K-char post-fetch content via keenable/v1/fetch, not by titles+URLs; latest claim `ce561502-2112-4066-8a6b-dae107ddda9b` shows it as 10x faster than Exa (512ms vs 5480ms) but with lower recall (52%). |
| 2 | Parallel | — | — | — | 14.0 (38.9%) | 3 | Evergreen rank-5 (was 84%, rank 2). News rank-3 at 30% (was 76%, rank 2-3). Objective-mode excerpts encode answers densely but rarely surface them in titles; latest claim `ce561502-2112-4066-8a6b-dae107ddda9b` failed to benchmark due to billing issues. |
| 3 | You.com | — | — | — | — | 2 | Previously held lead in factual recall before volume-bias re-evaluation. Not benchmarked in latest claim. |
| 4 | BrightData (Google) | 58.0 (90.6%) | 2157ms (23.7%) | $1.50 (100%) | 36.0 (100%) | 1 | Evergreen rank-1 under volume-neutral scoring (was 82%, rank 3-5 under predecessor's content-mode). Also rank-1 on news at 32%, tied with Exa. Google's titles+URLs encode the answer most often -- the inherent advantage of SERP-style retrieval when content is stripped; latest claim `ce561502-2112-4066-8a6b-dae107ddda9b` places it second in SimpleQA recall (58%) with competitive latency and cost. |
| 5 | Exa | 64.0 (100%) | 5480ms (9.3%) | $5.00 (30.0%) | 26.0 (72.2%) | 1 | Evergreen rank-3 (was 82%, tied rank 3-5). News tied rank-1 with Google at 32% (was 76%, rank 2-3); latest claim `ce561502-2112-4066-8a6b-dae107ddda9b` shows it leading in SimpleQA recall (64%) but with highest latency (5480ms). |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| SimpleQS-50 (content_hit_at_5, %) | 3 | 50 evergreen factoids, content-mode, K=5, substring match. | Factoid retrieval recall when rich content is available. |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 | 20 HN-derived SimpleQA queries, substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| SimpleQA-50 (seed=42) answer_in_top_5_recall (%, higher=better) | 1 | 50 SimpleQA questions, top-K=5, substring match on title/URL/snippet. | Factual recall on short-answer QA. |
| Median latency (ms, lower=better) | 1 | Median response time for 50 queries. | API response speed. |
| Cost per 1k queries ($, lower=better) | 1 | Public list price per 1,000 queries. | Economic efficiency of the API. |

### Notes

The latest claim, [Tested 3 search APIs on 50 SimpleQA questions — Exa wins recall (64%), Keenable is 10× faster](/claims/ce561502-2112-4066-8a6b-dae107ddda9b), introduces a new set of benchmarks focusing on a direct SimpleQA recall, latency, and cost comparison. This claim complements the previous re-evaluation of "volume bias" (`claim:15387728-5c96-479f-ad04-a444cd17c4ed`), which highlighted how content-rich APIs could artificially inflate recall scores.

The new benchmark reveals a clear performance tradeoff: Exa leads in factual recall at 64% but is the slowest, with a median latency of 5,480ms. Keenable, while scoring lower on recall at 52%, is significantly faster at 512ms median latency—roughly 10 times quicker than Exa. BrightData (Google) positions itself as a strong middle ground, achieving 58% recall with a median latency of 2,157ms, and notably, it is the cheapest of the three at $1.50 per 1,000 queries. This suggests that agent builders must weigh their priorities: raw accuracy, real-time responsiveness, or budget.

Keenable, despite losing its previous rank-1 spot in evergreen recall under volume-neutral scoring, now holds the top rank in cumulative Wins, driven by its consistent performance across various claims and its standout speed in the latest benchmark. The previous summary's finding that BrightData (Google) excels when recall is based purely on titles and URLs (volume-neutral) remains relevant, as its underlying Google SERP data continues to be a strong contender in factual retrieval.

The failure of Parallel to be benchmarked in the latest claim due to "Insufficient credit" (`claim:ce561502-2112-4066-8a6b-dae107ddda9b` README) is a reminder of the practical challenges in maintaining continuous API access for benchmarks.

### What's missing

- **More comprehensive cost analysis:** The current cost figures are public list prices; real-world volume discounts and enterprise pricing could significantly alter the economic landscape.
- **Tavily, Firecrawl, Google Custom Search, Brave Search:** These APIs are mentioned in the Battle description but are not consistently benchmarked across claims, leaving gaps in the competitive landscape.
- **Reliability and error rates:** While the latest claim reported 0/150 errors, a more robust evaluation of API uptime and error handling under stress is needed.
- **Parallel.ai re-evaluation:** The billing issue preventing Parallel's inclusion in the latest benchmark should be resolved to ensure a complete picture of its performance.

---SHORT---
Keenable maintains its lead in cumulative wins, while Exa achieves the highest SimpleQA recall at 64% in the latest benchmark.

### Top 3

| Rank | Solution | Score |
| :--- | :------- | :---- |
| 1 | **Keenable** | 67.35% |
| 2 | Parallel | 38.9% |
| 3 | You.com | — |

### Top Metrics

| Eval | Claims |
| :--- | :----- |
| SimpleQS-50 (content_hit_at_5, %) | 3 |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 |
| SimpleQA-50 (seed=42) answer_in_top_5_recall (%, higher=better) | 1 |

### Notes
🏆 Keenable · ⚡ Keenable fastest · 💸 BrightData cheapest · 🔄 Close race on recall/latency