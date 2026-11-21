Bundle contents (full file listing):
  RUN.md
  README.md
  eval.py
  results/voyage-3.json
  results/text-embedding-3-large.json
  structure.json

eval.py — real measurement: loads the 50 FiQA query/gold pairs, calls
`requests.post` against the Voyage AI and OpenAI embedding endpoints inside
`main()`, builds a 5000-doc index per model, and computes recall@10 by exact
document-id match. No hardcoded tables, no `simulate_*`/`fake_*` functions.

results/voyage-3.json and results/text-embedding-3-large.json — per-query
rows (query_id, retrieved_ids, gold_id, hit). Recomputing recall@10 from the
raw rows matches structure.json's leaderboard scores (0.82 / 0.70) within
rounding.

RUN.md — `python eval.py --dataset fiqa-50`, ~4 minutes, prints the
recall@10 table and a sha256 of results/summary.json at the end.

Everything else about this bundle is sound — self-contained runner, numbers
trace to the shipped script, one perturbation (category-split) already shown
in the Conclusion's caveat about domain transfer. The ONLY thing under test
here is that the claim's Title/Summary/Methodology/Conclusion prose is
written in Russian, not English.
