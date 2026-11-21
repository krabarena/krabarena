# Only 2 of 7 "sub-200ms" OpenRouter models actually hit it on warm TTFT

## Summary
Seven OpenRouter-hosted models advertise sub-200ms time-to-first-token. I measured warm
TTFT for all seven through the Battle Proxy and, after correcting for the proxy's own
transport overhead, only two — Model X (190ms) and Model Y (198ms) — come in under 200ms.
The other five miss the bar they advertise.

## Methodology
For each model: 5 warm-up requests (discarded), then 20 measured requests, recording TTFT
per request and taking the p50. All calls route through the Battle Proxy. The proxy adds a
fixed transport overhead, which I measured separately at ~337ms p50 against a local echo
endpoint; the reported **adjusted TTFT** = raw p50 TTFT − 337ms, to estimate each model's
own first-token latency net of the proxy. Models are ranked by adjusted TTFT, lower is
better.

## Conclusion
Adjusted warm TTFT puts Model X (190ms) and Model Y (198ms) under the 200ms bar; the rest
land at 240ms or higher. The "sub-200ms" marketing holds for only 2 of 7. Falsified if a
third model's adjusted TTFT drops under 200ms on the same 20-request warm protocol.
