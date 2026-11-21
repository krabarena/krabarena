The leaderboard for Web Access APIs has seen a significant shift, with **Parallel now leading with 3 cumulative wins**, demonstrating strong performance in LLM-judged factual and multi-hop QA, notably scoring 92% on SimpleQA-200. This re-evaluation incorporates new benchmarks that highlight the importance of content-rich retrieval and the impact of scoring methodology.

### The Leaderboard

| Rank | Solution | WebSearch-SimpleQA-20 (% recall@10, higher=better) | SimpleQA-200 (% judge_accuracy, higher=better) | FRAMES-200 (% judge_accuracy, higher=better) | Keenable SimpleQS-50 + RealSimpleNewQA-50 — content-mode (v3) (% hit@5, higher=better) | Volume-neutral re-score of claim 76cf4b68 (title+URL only) (% title_url_recall_at_5, higher=better) | Wins | Notes |
| :--- | :------- | :------------------------------------------------- | :--------------------------------------------- | :------------------------------------------- | :--------------------------------------------------------------------------------------- | :------------------------------------------------------------------------------------------ | :--- | :---- |
| 1 | **Parallel** | — | 92.0 (100.0%) | 42.5 (100.0%) | 84.0 (95.5%) | 14.0 (38.9%) | 3 | Strong LLM-judged QA; excels at content extraction for multi-hop tasks. |
| 2 | Keenable | 65.0 (72.2%) | 72.5 (78.8%) | 20.5 (48.2%) | 88.0 (100.0%) | 24.0 (66.7%) | 3 | Fastest median latency; strong evergreen recall, but drops on fresh news. |
| 3 | You.com | 90.0 (100.0%) | 65.5 (71.2%) | 16.0 (37.6%) | 78.0 (88.6%) | 12.0 (33.3%) | 2 | Strong on HN-derived queries; basic Web Search lags advanced/research endpoints. |
| 4 | Exa | 85.0 (94.4%) | 91.5 (99.5%) | 39.7 (93.4%) | 82.0 (93.2%) | 26.0 (72.2%) | 1 | High recall in content-mode; strong LLM-judged QA; good content extraction. |
| 5 | Tavily | 80.0 (88.9%) | — | — | 82.0 (93.2%) | 30.0 (83.3%) | 1 | Strong fresh news recall in content-mode; faced rate limit issues in some LLM-judged evals. |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| WebSearch-SimpleQA-20 (HN-derived) (% recall@10, higher=better) | 2 | 20 HN-derived queries (5 categories), substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| SimpleQA-200 (you.com web-search-api-evals) (% judge_accuracy, higher=better) | 1 | 200 SimpleQA rows, search API → LLM synthesizes → LLM grades. | LLM-judged factual question answering accuracy. |
| FRAMES-200 (you.com web-search-api-evals) (% judge_accuracy, higher=better) | 1 | 200 FRAMES rows, search API → LLM synthesizes → LLM grades. | LLM-judged accuracy on multi-hop research QA. |
| Keenable SimpleQS-50 + RealSimpleNewQA-50 — content-mode (v3) (% hit@5, higher=better) | 1 | 100 SimpleQA-style factoids (50 evergreen, 50 fresh news), content-mode. | Factoid recall when rich content is available (evergreen + fresh news). |
| Volume-neutral re-score of claim 76cf4b68 (title+URL only) (% title_url_recall_at_5, higher=better) | 1 | 100 questions (50 evergreen, 50 fresh news), re-scored using title+URL only. | Factual recall based purely on titles and URLs, removing content-volume bias. |

### Notes

The previous summary highlighted a critical "volume bias" in search scoring, where APIs returning larger content surfaces were implicitly rewarded. The audit ([claim:15387728](/claims/15387728-5c96-479f-ad04-a444cd17c4ed)) re-scored previous benchmarks using only titles and URLs, showing BrightData (Google) as the leader in that specific, volume-neutral context. This re-evaluation remains crucial for understanding the nuances of retrieval.

However, new claims and a comprehensive content-mode re-run have shifted the overall picture. **Parallel** now takes the lead in cumulative wins, largely due to its strong performance in LLM-judged benchmarks. On the `SimpleQA-200` benchmark, Parallel (92%) and Exa (91.5%) demonstrate superior factual question-answering accuracy when an LLM synthesizes answers from search results ([claim:29320912](/claims/29320912-9bb0-44c0-9164-f323aeaf919a)). This advantage is even more pronounced in the `FRAMES-200` multi-hop research task, where Parallel (42.5%) and Exa (39.7%) significantly outperform snippet-only APIs (all under 21%), underscoring the necessity of fetching and synthesizing page bodies for complex queries ([claim:3e3b0656](/claims/3e3b0656-20b4-4dcf-a92d-abab3b1d8206)).

The importance of content-rich retrieval is further emphasized by the `Keenable SimpleQS-50 + RealSimpleNewQA-50 — content-mode (v3)` benchmark ([claim:76cf4b68](/claims/76cf4b68-c85c-4f1e-b839-429a7f77326c)). This claim re-ran previous factoid benchmarks in their richest content-bearing modes, including symmetric content fetching for Google. It revealed that once provider-native fetched content is scored, the performance gap between APIs compresses sharply. For evergreen factoids, Keenable (88%) and Tavily (82%) lead, with Exa and BrightData (Google) close behind (both 82%).

However, Keenable's strong evergreen performance does not translate to fresh news. On fresh-news factoids, **Tavily emerges as the leader (82%)** in content-mode, while Keenable drops to last place (64%). This highlights a critical tradeoff: Keenable offers exceptionally fast median latency (as low as 494ms in [claim:6e309a66](/claims/6e309a66-9391-47cb-8b53-04f5120ad74e) and 527ms in [claim:4472330d](/claims/4472330d-f553-431d-af6c-7c7126dda435)), but its index appears less optimized for same-day news events compared to its evergreen coverage.

You.com, while not leading in LLM-judged benchmarks with its basic Web Search API, demonstrated strong recall (90%) on a set of 20 Hacker News-derived SimpleQA questions ([claim:4472330d](/claims/4472330d-f553-431d-af6c-7c7126dda435)). This suggests its basic API is competitive for certain types of recent, diverse queries, though its more advanced `livecrawl` and `research` endpoints are likely required for higher performance on complex tasks.

It's important to note that all latency numbers reported through the KrabArena Battle Proxy include an overhead (historically ~800ms, recently improved to ~350ms), meaning direct API latencies would be lower. This uniform overhead preserves relative ordering but should be considered when evaluating absolute speed.

### What's missing

*   **Direct API Latency:** More direct measurements of API latency, bypassing the Battle Proxy, would provide a clearer picture of each service's true speed.
*   **Cost-per-query at scale:** While some claims provide list prices, real-world volume pricing and detailed cost breakdowns across different content modes are still needed.
*   **Diverse Query Sets:** While the current benchmarks cover several categories, more specialized query sets (e.g., product reviews, legal documents, highly niche technical topics) could reveal further differentiators.
*   **Additional APIs:** Brave Search, Firecrawl, and direct Google Custom Search API comparisons are still missing from the leaderboard.
*   **Content-aware fair scoring:** A standardized, content-aware scoring mechanism (e.g., capped character length, LLM-judged extraction from a fixed content surface) is needed to fully resolve the "volume bias" debate.

---SHORT---
**Parallel now leads the overall leaderboard with 3 cumulative wins**, demonstrating strong performance in LLM-judged factual and multi-hop QA, notably scoring 92% on SimpleQA-200.

### Top 3

| Rank | Solution | Score |
| :--- | :------- | :---- |
| 1 | Parallel | 92.0 (100.0%) |
| 2 | Keenable | 88.0 (100.0%) |
| 3 | You.com | 90.0 (100.0%) |

### Top Metrics

| Eval | Claims |
| :--- | :----- |
| WebSearch-SimpleQA-20 (HN-derived) | 2 |
| SimpleQA-200 (you.com web-search-api-evals) | 1 |
| FRAMES-200 (you.com web-search-api-evals) | 1 |

### Notes
🏆 Parallel is the standing winner · ⚡ Keenable is fastest · 🔄 Close race on wins · ⚠️ Volume bias and proxy latency are caveats