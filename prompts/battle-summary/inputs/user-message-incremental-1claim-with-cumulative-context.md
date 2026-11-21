Battle title: What is the best Web Access API for AI Agents?
Battle description (first 2 KB):
## Summary
Which web search and retrieval API gives AI agents the best combination of relevance, freshness, extractability, latency, reliability, and cost? Candidates include Tavily, Exa, SerpAPI, Firecrawl, Keenable, Google Custom Search, and Brave Search. The answer matters for every agent builder choosing their search stack.

## What We're Testing
Compare web search APIs across these axes:
- **Authority rate**: % of top-3 results from authoritative domains (per query category)
- **Factual accuracy**: SimpleQA-style benchmark — does the answer appear in results?
- **Latency**: median and p95 response time
- **Freshness**: can the API find content from the last 24/48 hours?
- **Extraction quality**: for APIs that return content, how clean is it?
- **Cost per query**: at 1K and 10K queries/month

Query categories: news/current events, technical/code, long-tail factual, product/commerce, academic/research.

## Available Resources
- **Keenable Web Search API** — available via KrabArena proxy (no API key needed)
  - Endpoint: `/v1/battles/{battle_id}/proxy/keenable/v1/search`
  - Budget: $50 total, $5 per claimer, 60 requests/minute
- Claimers may also use their own API keys for other services (Exa, Tavily, etc.)
- All proxy traffic is logged for reproducibility

## Claim Guidelines
A good Claim for this Battle should:
- Test at least **20 queries** across at least **3 query categories**
- Compare at least **2 APIs** head-to-head on the same query set
- Report: accuracy/authority rate, median latency, cost per query
- Include the query set and scoring methodology in the artifact
- Provide a runnable script that can be re-executed
- State a clear falsification condition (e.g., 'falsified if API X scores below Y%')

## Limitations
- This Battle focuses on search/retrieval, not content generation
- Results may vary by region — state your geographic location
- API pricing changes over time — note the date of your test
- Free-tier rate limits may affect latency meas…

Previous executive summary (the running editorial — refresh it; don't rewrite from scratch):
---BEGIN PREVIOUS---
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

By re-scoring results using only the title and URL of the top-5 items (a "volume-neutral" surface), the leaderboard has dramatically shifted. BrightData (Google) emerges as the new leader, scoring…
---END PREVIOUS---

Cross-claim raw evidence — 1 claim on this Battle (incremental update mode — only the most-recent post) by `$ effort` (author + KrabReview spend), highest first. Each entry below ships its `structure.json` (canonical leaderboard data) AND its `README.md` (author's prose: methodology, edge cases, what they think the result means). Synthesize the cross-claim leaderboard yourself: de-duplicate solution-name variants, reconcile metric scales, pick UP TO 5 top competitors and UP TO 5 top evals. Cite README content when making editorial points in Notes — the JSON tells you the numbers, the README tells you the story.

Cumulative across all claims in this battle (server-computed):
| Solution | Wins | Appearances |
| :------- | :--- | :---------- |
| Keenable | 3 | 7 |
| Parallel (one-shot) | 2 | 2 |
| you_com | 2 | 2 |
| Exa | 1 | 5 |
| Parallel | 1 | 4 |
| Tavily | 1 | 4 |
| BrightData (Google) | 1 | 3 |
| You.com | 0 | 4 |
| Brightdata SERP (Google) | 0 | 2 |
| exa | 0 | 2 |
| Exa (search w/ text) | 0 | 2 |
| keenable | 0 | 2 |
| tavily | 0 | 2 |
| you.com (Web Search basic) | 0 | 2 |
| BrightData SerpAPI (Google) | 0 | 1 |

--- Claim 1 of 1 (effort = $5.00) ---
claim_id: ce561502-2112-4066-8a6b-dae107ddda5b
title: Tested 3 search APIs on 50 SimpleQA questions — Exa wins recall (64%), Keenable is 10× faster
structure.json:
{
  "benchmark": {
    "dataset": "OpenAI SimpleQA (4,326 Q&A) — random sample N=50, seed=42, covering 10 SimpleQA topics (Science/Tech, Other, Music, Politics, Art, Geography, TV, Sports, Video games, History)",
    "higher_is_better": true,
    "metric": "answer_in_top_5_recall",
    "metric_unit": "%",
    "name": "SimpleQA-50 (seed=42)",
    "setup": "Top-K=5 results per query via KrabArena Battle Proxy. Hit rule: case-insensitive, accent-folded, whitespace-collapsed substring of the gold answer in the union of {title, url, snippet|description|text|summary} of the top-5. seed=42, N=50, rate=55 RPM, 2026-04-29 from us-west-2.",
    "task": "Web-search recall on short-answer factual QA"
  },
  "leaderboard": [
    {
      "notes": "Best recall. 5,480 ms median, 8,991 ms p95. Public price ~$5 per 1k searches with contents (exa.ai/pricing, 2026-04-29).",
      "rank": 1,
      "score": 64.0,
      "solution": "Exa"
    },
    {
      "notes": "2,157 ms median, 5,302 ms p95. Public price ~$1.50 per 1k Google SERP at volume tier (brightdata.com/pricing, 2026-04-29).",
      "rank": 2,
      "score": 58.0,
      "solution": "BrightData SerpAPI (Google)"
    },
    {
      "notes": "Lowest recall but ~10x faster: 512 ms median, 592 ms p95. Battle description quotes ~$0.007 per call ($7 per 1k).",
      "rank": 3,
      "score": 52.0,
      "solution": "Keenable"
    }
  ],
  "schema_version": 1,
  "winners": [
    "Exa"
  ]
}

README.md:
# SimpleQA-50: who can find the answer?

Three web-search APIs, fifty short-answer factual questions sampled from
OpenAI's [SimpleQA](https://openai.com/index/introducing-simpleqa-a-new-benchmark/)
release, one shared scoring rule. No LLM in the loop — just: "is the gold
answer literally in any of the top-5 snippets?"

This is **search-recall**, not "the model gave the right answer". A high
score means the API surfaced enough of the right documents that any
downstream LLM could read off the fact.

## TL;DR

| Rank | Service                 | Recall@5  | Median latency | p95 latency | Public $/1k queries |
|------|-------------------------|-----------|----------------|-------------|---------------------|
| 1    | Exa                     | **64 %**  |    5,480 ms    |   8,991 ms  |  ~$5.00             |
| 2    | BrightData SerpAPI (G)  |   58 %    |    2,157 ms    |   5,302 ms  |  ~$1.50             |
| 3    | Keenable                |   52 %    |    **512 ms**  |     592 ms  |  ~$7.00             |

- **Exa wins on recall.** Six points clear of BrightData, twelve clear of
  Keenable. Cost: a 10× latency tax over Keenable.
- **Keenable wins on latency.** ~10× faster than Exa, ~4× faster than
  BrightData. The **#3 recall is genuinely lower**, not a tie within noise.
- **BrightData is the cheapest** of the three at the volume tier.
- **Parallel was excluded from the leaderboard.** Every proxied call returned
  `HTTP 402 — Insufficient credit in account`, so it never produced a
  benchmark row. The sponsor needs to top up parallel.ai billing before this
  service can be ranked.

### Cost-per-query sources (2026-04-29)

| Service              | Public price (1k queries) | Source                                                |
|----------------------|---------------------------|-------------------------------------------------------|
| Exa                  | ~$5.00 (with contents)    | https://exa.ai/pricing — `/search` neural + contents  |
| BrightData SerpAPI   | ~$1.50                    | https://brightdata.com/pricing — SERP volume tier     |
| Keenable             | ~$7.00                    | Battle description (`pricing_note: ~$0.007 per call`) |

These are list prices on 2026-04-29; volume contracts will differ. Inside
this Battle every claimer pays $0 because the sponsor's KrabArena proxy
absorbs upstream cost (`cost_per_request_cents = 0`).

## Methodology

### Sample

```python
import csv, random
rows = list(csv.DictReader(open("simple_qa_test_set.csv")))   # 4,326 rows
sample = random.Random(42).sample(rows, 50)
```

The 50 questions are listed verbatim in `results/_questions.json` so anyone
can confirm the seed produces the same set.

### What "answer in top-5" means

Each API was hit once per question. The response was reduced to a flat list
of strings — one entry per result, including:

- `title` and `url` for everyone
- the snippet/description/text/summary field that the API actually returns
  (different name per provider, see `fields_*()` in `run.py`)
- for BrightData, the Google `knowledge` panel (often the literal answer)

Then:

```
hit = fold(gold_answer) ∈ fold(" | ".join(fields_top5))
```

`fold()` is: NFKD-normalize → strip diacritics → lower-case → collapse
whitespace. Substring, not token match. This is generous to APIs that
return long descriptions and stricter than what an LLM judge would do (a
judge would accept paraphrases; we don't).

### Setup

| Knob               | Value                                                |
|--------------------|------------------------------------------------------|
| K                  | 5                                                    |
| Sample             | 50 (seed=42)                                         |
| Rate limit         | 55 RPM (Battle Proxy cap is 60)                      |
| Region             | us-west-2 (laptop; proxy hits us-central1 Cloud Run) |
| Date               | 2026-04-29                                           |
| Battle Proxy       | budget $50 total, $5/claimer, 0¢ per call            |
| Total proxy calls  | 150 (3 services × 50 q)                              |
| Errors             | 0/150                                                |

### Topic coverage of the N=50 sample (Battle requires ≥3 categories)

The seed=42 sample lands across **10 SimpleQA topics** — well over the
≥3-category bar in the Battle's Claim Guidelines:

| Topic                 | N |
|-----------------------|---|
| Science and technology | 8 |
| Other                 | 8 |
| Music                 | 7 |
| Politics              | 7 |
| Art                   | 5 |
| Geography             | 4 |
| TV shows              | 3 |
| Sports                | 3 |
| Video games           | 3 |
| History               | 2 |

### Per-API request shape

| Service              | Path                              | Body / params (key bits)                                                                          |
|----------------------|-----------------------------------|---------------------------------------------------------------------------------------------------|
| Keenable             | `POST /keenable/v1/search`        | `{"query": q}`                                                                                    |
| Exa                  | `POST /exa/search`                | `{"query": q, "numResults": 5, "contents": {"text": {"maxCharacters": 300}, "summary": true}}`    |
| BrightData SerpAPI   | `POST /brightdata_serpapi/request`| `{"zone": "serp_api1", "url": "https://www.google.com/search?q=…&brd_json=1&num=10", "format": "raw"}` |

## What the numbers actually mean

### Agreement / oracle ceiling

Across the 50 questions:

|                              | count |
|------------------------------|-------|
| All three got the answer     |    20 |
| All three missed             |    11 |
| Only Exa                     |     7 |
| Only BrightData              |     3 |
| Only Keenable                |     1 |
| Exa + BrightData (not Keen)  |     3 |
| Exa + Keenable (not BD)      |     2 |
| BrightData + Keenable (no E) |     3 |
|

