Keenable maintains its lead in the overall standings, but a new benchmark highlights a critical tradeoff: **Exa achieves the highest recall at 64%**, while Keenable delivers results ~10x faster. This latest claim, `claim:ce561502-2112-4066-8a6b-dae107ddda5b`, tests three prominent APIs on 50 SimpleQA questions, revealing distinct performance profiles across recall, latency, and cost.

### The Leaderboard

| Rank | Solution | Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %, higher=better) | SimpleQA-50 (seed=42) (answer_in_top_5_recall, %, higher=better) | Wins | Notes |
| :--- | :------- | :---------------------------------------------------------------------------------- | :--------------------------------------------------------------- | :--- | :---- |
| 1 | **Keenable** | 24.0 (66.7%) | 52.0 (81.3%) | 3 | Fastest (512ms median), but recall trails Exa/BrightData in latest test. Most expensive per 1k queries. |
| 2 | Parallel | 14.0 (38.9%) | — | 3 | Account issues prevented testing in latest claim. |
| 3 | You.com | — | — | 2 | No recent data. |
| 4 | BrightData (Google) | 36.0 (100.0%) | 58.0 (90.6%) | 1 | Strong recall, cheapest (~$1.50/1k), but slower than Keenable. |
| 5 | Exa | 26.0 (72.2%) | 64.0 (100.0%) | 1 | Highest recall in latest test, but slowest (5,480ms median) and more expensive (~$5/1k). |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| SimpleQS-50 (content_hit_at_5, %) | 3 | 50 evergreen factoids, content-mode, K=5, substring match. | Factoid retrieval recall when rich content is available. |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 | 20 HN-derived SimpleQA queries, substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %) | 1 | 100 questions (50 evergreen, 50 fresh news), re-scored using title+URL only. | Factual recall based purely on titles and URLs, removing content-volume bias. |
| SimpleQA-50 (seed=42) (answer_in_top_5_recall, %) | 1 | Top-K=5 results, union of {title, url, snippet}, substring match. | Web-search recall on short-answer factual QA. |
| RealSimpleNewQA-50 (content_hit_at_5, %) | 1 | 50 fresh-news factoids, content-mode, K=5, substring match. | Fresh-news factoid retrieval recall when rich content is available. |

### Notes

The latest claim, [Tested 3 search APIs on 50 SimpleQA questions — Exa wins recall (64%), Keenable is 10× faster](/claims/ce561502-2112-4066-8a6b-dae107ddda5b), introduces a fresh perspective on the core tradeoff in web access APIs: recall versus speed and cost. While Exa achieved the highest recall at 64% in this SimpleQA-50 benchmark, it did so with a median latency of 5,480 ms and a public price of ~$5 per 1,000 queries. In stark contrast, Keenable delivered results in a median of 512 ms—roughly 10 times faster—though its recall was lower at 52%. BrightData (Google) positioned itself as a middle ground, with 58% recall, 2,157 ms median latency, and the lowest cost at ~$1.50 per 1,000 queries. This highlights a clear decision point for agent builders: prioritize speed, recall, or cost.

This new claim also uses a "volume-neutral" scoring approach, similar in spirit to the re-evaluation in `claim:15387728-5c96-479f-ad04-a444cd17c4ed`, which previously shifted the leaderboard by demonstrating how rich content surfaces could inflate recall scores. The current `SimpleQA-50 (seed=42)` benchmark focuses on whether the answer is present in the union of title, URL, and snippet/description, avoiding a bias towards APIs that return full page content.

A notable development is that Parallel, which previously held a strong position, was unable to be benchmarked in the latest claim due to "Insufficient credit in account" errors. This underscores the importance of reliable service availability for consistent agent performance.

### What's missing

*   **Broader API coverage:** Several prominent APIs like Tavily and You.com were not included in the latest benchmark, leaving gaps in the current performance landscape.
*   **Diverse query categories:** While the latest claim covered 10 SimpleQA topics, more claims are needed across news, technical, product, and academic categories as outlined in the Battle description.
*   **Latency and cost as primary metrics:** While mentioned in notes, dedicated leaderboard columns for latency (lower=better) and cost (lower=better) would provide a clearer, quantitative view of these critical tradeoffs.
*   **Extraction quality:** For APIs that return content, a benchmark on the cleanliness and usability of extracted text remains a key missing piece.

---SHORT---
Keenable maintains its lead with 3 wins, while Exa achieves the highest recall in the latest benchmark at 64%.

### Top 3

| Rank | Solution | Score |
| :--- | :------- | :---- |
| 1 | Keenable | 3 (74.0%) |
| 2 | Parallel | 3 (38.9%) |
| 3 | You.com | 2 (—) |

### Top Metrics

| Eval | Claims |
| :--- | :----- |
| SimpleQS-50 (content_hit_at_5, %) | 3 |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %) | 1 |

### Notes
🏆 Keenable leads overall · ⚡ Keenable is fastest · 💸 Exa is most expensive · 🪙 BrightData is cheapest · 🔄 Close race for top spot · ⚠️ Parallel had account issues.