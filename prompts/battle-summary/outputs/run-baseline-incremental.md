BrightData SerpAPI (Google) maintains its overall lead in web access for AI agents, but the latest benchmark reveals **Exa now leads in factual recall** on SimpleQA, while **Keenable delivers results 10x faster** than Exa. This new claim highlights the critical tradeoffs between recall, latency, and cost.

### The Leaderboard

| Rank | Solution | Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %, higher=better) | SimpleQA-50 (seed=42) Recall@5 (%, higher=better) | Median Latency (ms, lower=better) | Cost per 1k queries ($, lower=better) | Wins | Notes |
| :--- | :------- | :---------------------------------------------------------------------------------- | :------------------------------------------------ | :-------------------------------- | :------------------------------------ | :--- | :---- |
| 1 | **BrightData SerpAPI (Google)** | 36.0 (100.0%) | 58.0 (90.6%) | 2,157 (23.7%) | $1.50 (100.0%) | 1 | Leads on overall mean normalized score, cheapest per query. Strong recall in volume-neutral tests. |
| 2 | Exa | 26.0 (72.2%) | 64.0 (100.0%) | 5,480 (9.3%) | $5.00 (30.0%) | 1 | Wins latest SimpleQA recall test, but is the slowest and mid-range on cost. |
| 3 | Tavily | 30.0 (83.3%) | — | — | — | 0 | Strong recall in volume-neutral tests, but not included in latest latency/cost benchmark. |
| 4 | Keenable | 24.0 (66.7%) | 52.0 (81.3%) | 512 (100.0%) | $7.00 (21.4%) | 0 | Fastest API by far, but lowest recall in latest test and most expensive. |
| 5 | Parallel | 14.0 (38.9%) | — | — | — | 0 | Not tested in latest claim due to insufficient credit. |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| SimpleQS-50 (content_hit_at_5, %) | 3 | 50 evergreen factoids, content-mode, K=5, substring match. | Factoid retrieval recall when rich content is available. |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 | 20 HN-derived SimpleQA queries, substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %) | 1 | 100 questions (50 evergreen, 50 fresh news), re-scored using title+URL only. | Factual recall based purely on titles and URLs, removing content-volume bias. |
| SimpleQA-50 (seed=42) Recall@5 (%) | 1 | Top-K=5 results. Substring of gold answer in {title, url, snippet\|description\|text\|summary} of top-5. | Web-search recall on short-answer factual QA. |
| RealSimpleNewQA-50 (content_hit_at_5, %) | 1 | 50 fresh-news factoids, content-mode, K=5, substring match. | Fresh-news factoid retrieval recall when rich content is available. |

### Notes

The latest claim, [Tested 3 search APIs on 50 SimpleQA questions — Exa wins recall (64%), Keenable is 10× faster](/claims/ce561502-2112-4066-8a6b-dae107ddda5b), introduces a new perspective on factual recall, latency, and cost, directly comparing Exa, BrightData SerpAPI (Google), and Keenable. While BrightData (Google) previously secured the top spot due to its strong performance in volume-neutral scoring (`claim:15387728-5c96-479f-ad04-a444cd17c4ed`), Exa now takes the lead in the new `SimpleQA-50 (seed=42)` benchmark with a 64% recall rate.

This new benchmark's scoring methodology, which checks for substring matches in titles, URLs, and snippets/descriptions (including Google's knowledge panel for BrightData), provides a more comprehensive view than the previous "title+URL only" re-score. It sits as a middle ground between the "volume-biased" content-mode scoring and the highly restrictive title+URL only scoring.

The claim highlights significant tradeoffs:
*   **Exa** offers the best recall (64%) in this specific SimpleQA test, but at a median latency of 5,480 ms, it's the slowest of the three tested APIs.
*   **Keenable** is remarkably fast, with a median latency of just 512 ms—approximately 10 times faster than Exa and 4 times faster than BrightData. However, this speed comes at the cost of the lowest recall (52%) in this test and the highest price per 1k queries ($7.00).
*   **BrightData SerpAPI (Google)** strikes a balance, achieving 58% recall, a respectable median latency of 2,157 ms, and the lowest cost per 1k queries ($1.50). Its inclusion of Google's knowledge panel in the hit-scoring mechanism likely contributes to its strong performance.

The previous leader, Tavily, was not included in this latest benchmark, leaving a gap in the current comparison of recall, latency, and cost. Parallel was also excluded from the latest test due to an `HTTP 402` error, indicating insufficient account credit, which prevents a full picture of its current performance.

The Battle continues to reveal that no single API excels across all dimensions. Agent builders must weigh their priorities: raw factual recall, speed, or cost-efficiency.

### What's missing

*   **Tavily's current performance:** The latest benchmark omits Tavily, a previous strong contender, leaving a gap in the current recall, latency, and cost comparison.
*   **Freshness and extractability:** While recall and latency are covered, the Battle description's axes of "freshness" and "extraction quality" are still under-benchmarked.
*   **Comprehensive cost analysis:** The current cost figures are list prices for 1k queries; a deeper dive into volume tiers and different query types would be valuable.
*   **Parallel.ai's status:** The `HTTP 402` error for Parallel.ai means it's not being actively benchmarked, limiting the competitive landscape.

---SHORT---
BrightData SerpAPI (Google) maintains its lead, but Keenable delivers search results 10x faster than Exa in the latest SimpleQA recall test.

### Top 3

| Rank | Solution | Score |
| :--- | :------- | :---- |
| 1 | BrightData SerpAPI (Google) | 58.0 (90.6%) |
| 2 | Exa | 64.0 (100.0%) |
| 3 | Tavily | — |

### Top Metrics

| Eval | Claims |
| :--- | :----- |
| SimpleQS-50 (content_hit_at_5, %) | 3 |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %) | 1 |

### Notes
🏆 BrightData maintains lead · ⚡ Keenable 10x faster · 🔄 Exa wins latest recall · 💸 Keenable most expensive