You.com's previous lead in factual recall has been challenged by a new audit, which reveals that **BrightData (Google) now leads both evergreen and fresh news categories** when search results are scored based solely on titles and URLs. This re-evaluation, driven by a volume-neutral scoring methodology, highlights a significant bias in previous benchmarks that rewarded APIs returning larger content surfaces.

### The Leaderboard

| Rank | Solution | Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %, higher=better) | Wins | Notes |
| :--- | :------- | :---------------------------------------------------------------------------------- | :--- | :---- |
| 1 | **BrightData (Google)** | 36.0 (100.0%) | 1 | Evergreen rank-1 under volume-neutral scoring (was 82%, rank 3-5 under predecessor's content-mode). Also rank-1 on news at 32%, tied with Exa. Google's titles+URLs encode the answer most often -- the inherent advantage of SERP-style retrieval when content is stripped. |
| 2 | Tavily | 30.0 (83.3%) | 0 | Evergreen rank-2 (was 82%, tied rank 3-5 in predecessor). News collapses to 20%, rank 5 (was 82%, rank 1 in predecessor). Tavily's news win was driven by its 88K-char median content surface (advanced+raw_content); titles+URLs alone do not carry the answer. Sharpest single-API shift in the audit. |
| 3 | Exa | 26.0 (72.2%) | 0 | Evergreen rank-3 (was 82%, tied rank 3-5). News tied rank-1 with Google at 32% (was 76%, rank 2-3). |
| 4 | Keenable | 24.0 (66.7%) | 0 | Evergreen rank-4 (was 88%, rank 1 in predecessor). News rank-6 at 18% (was 64%, rank 6). Loses the evergreen #1 spot under volume-neutral scoring -- the 88% score was driven by 28K-char post-fetch content via keenable/v1/fetch, not by titles+URLs. |
| 5 | Parallel | 14.0 (38.9%) | 0 | Evergreen rank-5 (was 84%, rank 2). News rank-3 at 30% (was 76%, rank 2-3). Objective-mode excerpts encode answers densely but rarely surface them in titles. |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| Volume-neutral re-score of claim 76cf4b68 (title_url_recall_at_5, %) | 1 | 100 questions (50 evergreen, 50 fresh news), re-scored using title+URL only. | Factual recall based purely on titles and URLs, removing content-volume bias. |
| SimpleQS-50 (content_hit_at_5, %) | 3 | 50 evergreen factoids, content-mode, K=5, substring match. | Factoid retrieval recall when rich content is available. |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 | 20 HN-derived SimpleQA queries, substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| RealSimpleNewQA-50 (content_hit_at_5, %) | 1 | 50 fresh-news factoids, content-mode, K=5, substring match. | Fresh-news factoid retrieval recall when rich content is available. |
| SimpleQA-200 (judge_accuracy, %) | 1 | 200 SimpleQA rows, search API → LLM synthesizes → LLM grades. | LLM-judged factual question answering accuracy. |

### Notes

The latest claim, [Volume bias in search scoring: Tavily's 82% news win drops to 20%, Google now leads both categories](/claims/15387728-5c96-479f-ad04-a444cd17c4ed), introduces a critical re-evaluation of previous benchmarks, specifically `claim:76cf4b68` and its predecessors. This audit reveals a significant "volume bias" in prior scoring methodologies, where APIs returning larger content surfaces (e.g., full page content via `content_field`) were implicitly rewarded. The claim quantifies this bias, showing that hit rate climbs monotonically with content size, with an 11 percentage point advantage for APIs returning the most text per query ([Volume bias in search scoring: Tavily's 82% news win drops to 20%, Google now leads both categories](/claims/15387728-5c96-479f-ad04-a444cd17c4ed) README).

By re-scoring results using only the title and URL of the top-5 items (a "volume-neutral" surface), the leaderboard has dramatically shifted. BrightData (Google) emerges as the new leader, scoring 36% recall on evergreen queries and 32% on fresh news, effectively taking the top spot in both categories. This suggests that Google's SERP-style results, even when stripped of full content, inherently encode answers more effectively in their titles and URLs.

Conversely, APIs that previously excelled due to their rich content surfaces saw significant drops. Tavily's impressive 82% news win in the predecessor benchmark plummeted to 20% under volume-neutral scoring, indicating its strength was heavily reliant on its 88K-character median content surface ([Volume bias in search scoring: Tavily's 82% news win drops to 20%, Google now leads both categories](/claims/15387728-5c96-479f-ad04-a444cd17c4ed) README). Similarly, Keenable's 88% evergreen win dropped to 24%, as its performance was tied to post-fetch content via `keenable/v1/fetch`. You.com, previously a strong performer, also saw its recall drop from 78% to 12% on evergreen queries, highlighting its reliance on `description` and `snippets` fields.

This audit doesn't invalidate the utility of rich content retrieval, but rather clarifies the specific signal being measured. The claim acknowledges two honest interpretations: either the original substring scorer was volume-biased, or title+URL is itself a biased surface that favors SERP-style APIs. The truth likely lies between these two extremes, emphasizing the need for content-aware, but volume-capped, scoring methodologies to provide a truly fair comparison.

### What's missing

- **Content-aware, volume-capped scoring:** A fair evaluation needs to consider content quality without rewarding sheer volume. This would involve either capping the amount of content considered per result or using LLM-judges to synthesize answers from provided content.
- **Latency and Cost re-evaluation:** The current audit focuses solely on recall. A comprehensive re-evaluation should also consider how the volume-neutral scoring impacts the perceived value of APIs in terms of latency and cost, especially for those that previously relied on extensive content fetching.
- **Diverse query sets:** While the current dataset is good, expanding to more diverse query categories (e.g., technical/code, product/commerce) would provide a broader understanding of API performance.