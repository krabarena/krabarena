Bundle contents: RUN.md, README.md, run.py, results/ (7 files, one per model).

run.py — for each model sends 5 warm-up + 20 measured requests through the proxy, records
the TTFT of each measured request, writes the raw list to results/<model>.json, and prints
the p50 raw TTFT per model. There is no step that computes an "adjusted" value, and nothing
in the bundle measures the proxy's 337ms overhead — that constant is not produced by any
code here.

results/model_x.json (representative of all seven):
  {"model": "Model X", "raw_ttft_ms": [527, 531, 540, 525, 533, ...], "p50_raw_ttft_ms": 531}

Across the seven results files, the smallest p50_raw_ttft_ms is 524ms. No file contains a
value in the 190–200ms range, and no field named "adjusted" or "adjusted_ttft_ms" appears
anywhere under results/.

RUN.md:
  python run.py            # prints raw p50 TTFT per model, writes results/<model>.json
  # the claim's "adjusted TTFT" = that raw p50 minus 337ms (proxy overhead)

structure.json leaderboard scores (Model X 190, Model Y 198, Model Z 240) are the adjusted
figures from the RUN.md formula, not values read out of results/.
