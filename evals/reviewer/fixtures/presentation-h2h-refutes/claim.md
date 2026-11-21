# Prefix caching pays off exactly once: 7x at first contact, then vLLM ≈ SGLang, Ollama trails 3.25x

## Summary

On a synthetic multi-turn agentic trace with 5 interleaved concurrent sessions sharing one ~3.2K-token tool-catalog system prompt, prefix caching's real payoff is concentrated at the **first cross-session contact** (turn 1): a 6.8-7.7x TTFT win for vLLM/SGLang that collapses to ~1x by turn 3 onward, because once a session has built its own history, intra-session self-caching dominates and cross-session prompt-sharing stops mattering. At the last turn (longest shared+growing prefix), vLLM 0.23.0 and SGLang 0.5.9 land in a statistical tie (TTFT p50 0.1021s vs 0.1116s, a 1.09x gap with overlapping p95 ranges) -- this battle's own hypothesis of a much larger, sustained SGLang advantage via RadixAttention over vLLM's Automatic Prefix Caching (APC) does not show up here. Ollama 0.30.10 trails both engines by 3.25x regardless of condition.

## Methodology

K=5 concurrent agent "sessions" x T=5 turns x 3 independent nonce-isolated trials = 150 requests/engine, emitted in **round-robin interleaved order** (session1-turn1, session2-turn1, ..., session5-turn1, session1-turn2, ...) to stress a server-side cache the way real concurrent agents would, not sequential single-session replay. Two conditions at matched token volume: **shared** (all 5 sessions reuse ONE ~3196-token synthetic tool-catalog system prompt) vs **control** (each session gets its own unique ~3158-3224-token system prompt drawn from a disjoint tool pool -- no prefix shared across sessions). Deterministic canned assistant replies keep the exact token sequence -- and therefore the KV-cache key -- reproducible across engines and trials. Requests run sequentially (concurrency=1) so TTFT differences are attributable to cache hits/misses, not queueing.

Engines: vLLM 0.23.0 (`--enable-prefix-caching`), SGLang 0.5.9 (RadixAttention, on by default, `--enable-cache-report`), Ollama 0.30.10 (default). Model: Qwen2.5-3B, AWQ-Marlin int4 on vLLM/SGLang vs GGUF q4_K_M on Ollama (Ollama's llama.cpp backend doesn't support AWQ-Marlin -- both are ~4-bit quantizations of the same base model but not byte-identical; disclosed as a boundary condition, not hidden). Single RTX 3060 Ti 8GB, `--enforce-eager`/`--disable-cuda-graph`, mem-fraction 0.50, `--host 127.0.0.1`, Ubuntu 24.04 WSL2.

## Conclusion

| engine | condition | turn1 TTFT p50 | turn5 TTFT p50 | cache_speedup@turn1 | cache_speedup@turn5 |
|---|---|---|---|---|---|
| vLLM 0.23.0 | shared | 0.100s | 0.102s | 7.65x | 1.11x |
| vLLM 0.23.0 | control | 0.766s | 0.113s | -- | -- |
| SGLang 0.5.9 | shared | 0.110s | 0.112s | 6.84x | 0.77x |
| SGLang 0.5.9 | control | 0.749s | 0.086s | -- | -- |
| Ollama 0.30.10 | shared | 0.306s | 0.332s | 3.26x | 1.24x |
| Ollama 0.30.10 | control | 0.996s | 0.411s | -- | -- |

**Engine-vs-engine, SHARED condition, turn 5 (longest prefix)** -- the battle's stated hypothesis, directly tested: vLLM 0.102s (fastest, ratio 1.0x) vs SGLang 0.112s (1.09x, a statistical tie, not 6.4x) vs Ollama 0.332s (3.25x, clearly slower).

SGLang's own server log corroborates the mechanism behind the turn1-only effect: by turn 2, even **control**-condition requests (no shared system prompt across sessions) already show 3200+ server-reported cached tokens -- each session caches its OWN growing history regardless of whether the leading system-prompt segment was shared with anyone else. Cross-session prompt-sharing (what distinguishes "shared" from "control") therefore only pays off once, before any session has built its own history; after that, per-session self-caching swamps the cross-session-sharing signal on both engines. vLLM's native server log shows the same steady-state effect from a different angle: "Prefix cache hit rate" climbs from 65.8% to 85-94% as the trial progresses and sessions revisit their own warmed history.

Reproduction: see RUN.md. All numbers written programmatically by `harness/aggregate_multiturn.py` from raw per-request CSVs in `results/`; nothing hand-typed.
