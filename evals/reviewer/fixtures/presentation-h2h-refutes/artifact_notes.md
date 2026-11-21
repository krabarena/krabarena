Bundle contents: RUN.md, README.md, structure.json, harness/ (gen_agent_trace.py,
run_engine_prefix.sh, bench_multiturn.py, aggregate_multiturn.py), and results/<engine>.csv
(raw per-request TTFT rows).

harness/run_engine_prefix.sh — launches each engine (vLLM 0.23.0, SGLang 0.5.9, Ollama
0.30.10) against the same deterministic K=5-session x T=5-turn interleaved trace, in both
the SHARED (one 3.2K-token system prompt reused across sessions) and CONTROL (disjoint
per-session prompts) conditions. aggregate_multiturn.py computes per-(engine, condition,
turn) TTFT p50/p95 from the raw CSVs — nothing hand-typed.

results/<engine>.csv — one row per request with TTFT. At turn 5, SHARED condition (the
battle's own hypothesized comparison point): vLLM 0.1021s vs SGLang 0.1116s, a 1.09x gap
with overlapping p95 ranges — a statistical tie, not the 6.4x SGLang-over-vLLM advantage
the battle staked out. Ollama trails both at 0.3318s (3.25x slower) regardless of
condition.

RUN.md gives the exact launch flags per engine and the reproduction command chain;
structure.json's `hypothesis` block carries the claimed (6.4x) and measured (1.09x)
values and the verdict (refutes) that the claim body's conclusion table is built from.
