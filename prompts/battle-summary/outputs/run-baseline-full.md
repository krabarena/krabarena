The Web Access API landscape has significantly evolved, with **Parallel** now emerging as the overall leader across a broader set of benchmarks, particularly in LLM-judged question answering. While earlier audits highlighted BrightData (Google)'s strength in volume-neutral title+URL recall, new claims introduce critical content-mode and LLM-judged metrics, revealing diverse strengths and weaknesses across providers. The latest claim, `claim:ce561502`, shows **Exa leading raw SimpleQA recall at 64%**, while **Keenable maintains its position as the fastest API** with a median latency of 512 ms.

### The Leaderboard

| Rank | Solution | SimpleQS-50 (content_hit_at_5, %, higher=better) | RealSimpleNewQA-50 (content_hit_at_5, %, higher=better) | SimpleQA-200 (judge accuracy, %, higher=better) | FRAMES-200 (judge accuracy, %, higher=better) | WebSearch-SimpleQA-20 (recall@10, %, higher=better) | Wins | Notes |
| :--- | :------- | :----------------------------------------------- | :------------------------------------------------------ | :---------------------------------------------- | :-------------------------------------------- | :-------------------------------------------------- | :--- | :---- |
| 1 | **Parallel** | 84.0 (95.5%) | 76.0 (92.7%) | 92.0 (100%) | 42.5 (100%) | — | 4 | Excels in LLM-judged tasks (SimpleQA-200, FRAMES-200) and strong on fresh news. |
| 2 | BrightData (Google) | 82.0 (93.2%) | 74.0 (90.2%) | 85.0 (92.4%) | 20.0 (47.1%) | — | 4 | Strong overall, especially in content-mode evergreen and fresh news. High latency in content-mode. |
| 3 | Keenable | 88.0 (100%) | 64.0 (78.0%) | 72.5 (78.8%) | 20.5 (48.2%) | 65.0 (72.2%) | 4 | Consistently fastest (512 ms median), but recall varies widely (strong on its own evergreen data, weaker on fresh news). |
| 4 | Tavily | 82.0 (93.2%) | 82.0 (100%) | — | — | 80.0 (88.9%) | 3 | Top performer on fresh news content-mode. Missing data for LLM-judged evals. |
| 5 | You.com | 78.0 (88.6%) | 72.0 (87.8%) | 65.5 (71.2%) | 16.0 (37.6%) | 90.0 (100%) | 2 | Wins on HN-derived SimpleQA, but its basic Web Search API struggles on LLM-judged and volume-neutral evals. |

### Top Evals

| Eval | Claims | Sample setup | What it measures |
| :--- | :----- | :----------- | :--------------- |
| SimpleQS-50 (content_hit_at_5, %) | 3 | 50 evergreen factoids, content-mode, K=5, substring match. | Factoid retrieval recall when rich content is available. |
| WebSearch-SimpleQA-20 (recall_at_10, %) | 2 | 20 HN-derived SimpleQA queries, substring match on top-10 results. | Factual recall on recent, diverse SimpleQA-style questions. |
| RealSimpleNewQA-50 (content_hit_at_5, %) | 2 | 50 fresh-news factoids, content-mode, K=5, substring match. | Fresh-news factoid retrieval recall when rich content is available. |
| SimpleQA-200 (judge_accuracy, %) | 1 | 200 SimpleQA rows, search API → LLM synthesizes → LLM grades. | LLM-judged factual question answering accuracy. |
| FRAMES-200 (judge_accuracy, %) | 1 | 200 FRAMES rows, search API → LLM synthesizes → LLM grades. | LLM-judged multi-hop research / deep-reasoning QA. |

### Notes

The previous summary highlighted the "volume bias" audit (`claim:15387728`), which demonstrated that prior scoring methodologies implicitly rewarded APIs returning larger content surfaces. This re-evaluation, based on title+URL recall, showed BrightData (Google) leading both evergreen and fresh news categories. This finding remains crucial for understanding the nuances of retrieval scoring.

However, a flurry of new claims has broadened the evaluation scope significantly. `claim:ce561502` introduced a straightforward SimpleQA-50 benchmark, where Exa took the lead in raw recall (64%), with Keenable demonstrating remarkable speed (512 ms median latency). This claim, along with `claim:e7805fff` and `claim:6e309a66`, further explored SimpleQA-style factoid retrieval across evergreen and fresh news datasets, often revealing a tradeoff between speed and recall.

The introduction of LLM-judged benchmarks (`claim:29320912` for SimpleQA-200 and `claim:3e3b0656` for FRAMES-200) has been particularly impactful. These claims, which involve an LLM synthesizing an answer from search results and another LLM grading it, provide a more holistic view of an API's utility for agent workflows. In these critical LLM-judged tasks, **Parallel** emerges as a clear leader, scoring 92% on SimpleQA-200 and 42.5% on the more challenging FRAMES-200 multi-hop reasoning task. This performance underscores the value of APIs that explicitly fetch and return page bodies or rich excerpts, as snippet-only APIs consistently lagged in these benchmarks.

The content-mode re-runs (`claim:49bc03c9` and `claim:76cf4b68`) further confirmed the "volume bias" observation, showing that when provider-native fetched/content-bearing outputs are scored, the performance gap between APIs often compresses dramatically. For instance, Exa's recall on evergreen SimpleQS-50 jumped from 32% (snippet-only) to 82% (content-mode), a **+50 percentage point increase** (`claim:76cf4b68` README). This highlights that many APIs are designed to be consumed via their rich content fields, not just their snippets.

A notable shift occurred in fresh news retrieval: while Keenable initially showed strong evergreen performance, its recall dropped significantly on fresh news factoids (from 88% evergreen to 64% fresh news in content-mode, `claim:76cf4b68` README). Conversely, **Tavily** emerged as the top performer on fresh news in content-mode (82%), consistent with its "real-time agent search" positioning.

The `WebSearch-SimpleQA-20` claims (`claim:b94ceb58`, `claim:4472330d`), derived from Hacker News stories, provided another perspective on factual recall, where You.com achieved the highest score (90%), demonstrating its strength on recent, diverse queries.

Overall, the leaderboard now reflects a more nuanced understanding of "best." While Keenable remains the speed champion, and BrightData (Google) excels in raw title+URL recall, **Parallel's consistent top performance in LLM-judged tasks positions it as the current leader for agent-centric applications requiring deeper reasoning and synthesis.**

### What's missing

*   **Cost-per-query at scale:** While some claims provide list prices, real-world volume pricing and the impact of API usage on overall agent cost-efficiency are still underexplored.
*   **Latency under load:** Most latency measurements are for single-machine sequential calls or moderate concurrency. Benchmarking p99 latencies and throughput under high-concurrency, production-like loads would be valuable.
*   **Extraction quality for diverse content:** Beyond simple substring matching, evaluating how cleanly APIs extract structured data or key entities from varied web content remains an open area.
*   **Design-first vs. general-purpose agents:** The current benchmarks largely focus on factual QA. Future claims could explore performance for agents designed for specific tasks like product research, travel planning, or code generation.

---SHORT---
Parallel now leads the Web Access API battle, demonstrating strong performance in LLM-judged tasks.

### Top 3
| Rank | Solution | Score |
|:-----|:---------|:------|
| 1 | Parallel | 97.05% |
| 2 | BrightData (Google) | 80.7% |
| 3 | Keenable | 75.4% |

### Top Metrics
| Eval | Claims |
|:---------------------------------|:-------|
| SimpleQS-50 (content_hit_at_5, %) | 3 |
| WebSearch-SimpleQA-20 (recall@10, %) | 2 |
| RealSimpleNewQA-50 (content_hit_at_5, %) | 2 |

### Notes
🏆 Parallel leads · ⚡ Keenable fastest · 🔄 Close race for wins · 📉 Tavily missing LLM data