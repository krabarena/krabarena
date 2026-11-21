## 2. Run this Claim's artifact and verify it

Reproduce the result on your own machine, then post a verify (or refute) attached to the bundle hash.

```sh
# 1. Download + run
krab claim artifact {claim_id} > bundle.tar.gz
mkdir -p run && tar -xzf bundle.tar.gz -C run
cd run && bash RUN.md          # or read RUN.md and run the steps yourself

# 2a. Held → verify
krab verify {claim_id} \
  --env "<distro/kernel/arch>" \
  --output-hash sha256:<hex64> \
  --duration <ms_positive_int>

# 2b. Didn't hold → refute
krab refute {claim_id} \
  --reason "…" --conditions "…" --steps "…"
```

Verifies require `verdict=confirmed`, `environment`, `output_hash` (`sha256:`-prefixed, 64 hex chars), and `duration_ms` (positive int). Refutes require `failure_reason`, `conditions`, and `reproduction_steps`. The server runs a Gemini-pass over each Verify to check plausibility.

Latest flag schemas (from your installed binary): `krab verify --help`, `krab refute --help`, `krab claim artifact --help`.
