#!/usr/bin/env python3
"""Load generator: measure TCP proxy round-trip latency percentiles."""
import asyncio, json, sys, time, platform

CONCURRENCY = 500
ITERATIONS = 100
PAYLOAD = b"X" * 64
PROXY_PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8001

async def worker(port, latencies, errors):
    try:
        reader, writer = await asyncio.open_connection("127.0.0.1", port)
        for _ in range(ITERATIONS):
            t0 = time.perf_counter_ns()
            writer.write(PAYLOAD)
            await writer.drain()
            data = await reader.readexactly(len(PAYLOAD))
            t1 = time.perf_counter_ns()
            latencies.append((t1 - t0) / 1_000)  # microseconds
        writer.close()
        await writer.wait_closed()
    except Exception:
        errors.append(1)

async def main():
    latencies = []
    errors = []
    # Launch in batches to avoid fd exhaustion
    batch = 100
    for i in range(0, CONCURRENCY, batch):
        n = min(batch, CONCURRENCY - i)
        tasks = [worker(PROXY_PORT, latencies, errors) for _ in range(n)]
        await asyncio.gather(*tasks)

    latencies.sort()
    n = len(latencies)
    if n == 0:
        print(json.dumps({"error": "no successful requests"}))
        sys.exit(1)

    def pct(p):
        idx = min(int(n * p / 100), n - 1)
        return round(latencies[idx], 2)

    result = {
        "p50_us": pct(50),
        "p90_us": pct(90),
        "p99_us": pct(99),
        "p99_9_us": pct(99.9),
        "total_requests": n,
        "errors": len(errors),
    }
    print(json.dumps(result))

if __name__ == "__main__":
    asyncio.run(main())
