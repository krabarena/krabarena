#!/usr/bin/env python3
"""Per-page-type latency check for a KrabArena stand (web, API, CLI).

Usage:
  scripts/perf-check.py [--base-url https://krabarena.com] [-n 8] [--no-cli] [--json out.json]
                        [--only "home,battle detail"] [--battle SLUG_OR_ID]

Each target is requested once to warm up, then N times sequentially.
Reports TTFB and total p50 / p90 / max and flags targets over budget.
Web/API rows send `Accept-Encoding: gzip, br` like a browser, so `size`
is the transfer size. `q` is the server's `x-db-queries` header and
`app p50` the `app` entry of its `Server-Timing` header (server time
without network), both blank when the server does not send them.
CLI rows time the real `krab` binary (startup + network + rendering),
so they need `krab` on PATH and, for authenticated rows, a logged-in
config. Exit code 1 when any target is over budget.
"""

import argparse
import json
import os
import statistics
import subprocess
import sys
import time

PACE_S = 0.6

BUDGETS = {
    "page": (1.0, 2.0),
    "api": (0.6, 1.2),
    "asset": (1.5, 3.0),
    "cli": (2.0, 3.0),
}


def app_ms(server_timing):
    for part in server_timing.split(","):
        name, _, rest = part.strip().partition(";")
        if name == "app" and rest.startswith("dur="):
            try:
                return float(rest[4:])
            except ValueError:
                return None
    return None


def curl(url, accept=None, token=None):
    fmt = "%{http_code}\t%{time_starttransfer}\t%{time_total}\t%{size_download}\t%header{x-db-queries}\t%header{server-timing}"
    cmd = ["curl", "-s", "-L", "-o", "/dev/null", "-w", fmt,
           "-A", "Mozilla/5.0 (perf-check) Chrome/130", "--max-time", "60",
           "-H", "Accept-Encoding: gzip, br"]
    if accept:
        cmd += ["-H", f"Accept: {accept}"]
    if token:
        cmd += ["-H", f"Authorization: Bearer {token}"]
    out = subprocess.run(cmd + [url], capture_output=True, text=True).stdout.split("\t")
    out += [""] * (6 - len(out))
    code, ttfb, total, size = int(out[0]), float(out[1]), float(out[2]), int(float(out[3]))
    queries = int(out[4]) if out[4].strip().isdigit() else None
    return code, ttfb, total, size, queries, app_ms(out[5])


def run_cli(args):
    env = dict(os.environ, KRAB_NO_AUTO_UPDATE="1")
    start = time.perf_counter()
    proc = subprocess.run(["krab", *args], capture_output=True, text=True, env=env)
    elapsed = time.perf_counter() - start
    return (0 if proc.returncode == 0 else proc.returncode), elapsed, elapsed, len(proc.stdout), None, None


def get_json(url, token=None):
    cmd = ["curl", "-s", "--max-time", "30", url]
    if token:
        cmd += ["-H", f"Authorization: Bearer {token}"]
    return json.loads(subprocess.run(cmd, capture_output=True, text=True).stdout or "{}")


def pct(values, p):
    values = sorted(values)
    k = max(0, min(len(values) - 1, round(p / 100 * (len(values) - 1))))
    return values[k]


def cli_token():
    path = os.path.expanduser("~/.krab/config.toml")
    if not os.path.exists(path):
        return None
    for line in open(path):
        key = line.split("=", 1)[0].strip()
        if key in ("bearer_token", "token"):
            return line.split("=", 1)[1].strip().strip('"')
    return None


def discover(base, token, battle_ref=None):
    if battle_ref:
        battles = [get_json(f"{base}/v1/battles/{battle_ref}").get("data") or {}]
    else:
        battles = get_json(f"{base}/v1/battles?limit=20").get("data") or []
    battle, claim, best = {}, {}, 0
    for b in battles:
        claims = get_json(f"{base}/v1/battles/{b.get('id')}/claims?limit=20&compact=true").get("data") or []
        if len(claims) > best:
            battle, claim, best = b, claims[0], len(claims)
        if best >= 20:
            break
        time.sleep(PACE_S)
    agents = get_json(f"{base}/v1/agents?limit=3").get("data") or []
    agent = agents[0] if agents else {}
    ids = {
        "battle_slug": battle.get("slug") or battle.get("id"),
        "battle_id": battle.get("id"),
        "claim_ref": claim.get("slug") or claim.get("id"),
        "claim_id": claim.get("id"),
        "agent_id": agent.get("user_id") or agent.get("id"),
    }
    missing = [k for k, v in ids.items() if not v]
    if missing:
        sys.exit(f"could not discover sample ids: {missing}")
    return ids


def targets(ids, token, with_cli):
    b, c, a = ids["battle_slug"], ids["claim_ref"], ids["agent_id"]
    md = "text/markdown"
    t = [
        ("page", "home", "/", None),
        ("page", "battles list", "/battles/", None),
        ("page", "battle detail", f"/battles/{b}", None),
        ("page", "battle detail (markdown)", f"/battles/{b}", md),
        ("page", "claim detail", f"/claims/{c}", None),
        ("page", "claim detail (markdown)", f"/claims/{c}", md),
        ("page", "standalone claims", "/claims/standalone", None),
        ("page", "agents leaderboard", "/agents", None),
        ("page", "agent profile", f"/agents/{a}", None),
        ("page", "arena", "/arena", None),
        ("page", "search", "/search?q=web+search+api", None),
        ("page", "changelog", "/changelog", None),
        ("page", "about", "/about", None),
        ("page", "docs index", "/docs", None),
        ("page", "docs page", "/docs/agent", None),
        ("page", "notifications page", "/profile/agent-notifications", None),
        ("page", "zoo", "/zoo", None),
        ("asset", "og image (battle)", f"/og/battles/{ids['battle_id']}", None),
        ("asset", "sitemap.xml", "/sitemap.xml", None),
        ("asset", "llms.txt", "/llms.txt", None),
        ("asset", "feed.json", "/feed.json", None),
        ("api", "GET /v1/battles", "/v1/battles?limit=20", None),
        ("api", "GET /v1/battles/{id}", f"/v1/battles/{b}", None),
        ("api", "GET /v1/battles/{id}/claims", f"/v1/battles/{b}/claims?limit=20", None),
        ("api", "GET /v1/claims/{id}", f"/v1/claims/{c}", None),
        ("api", "GET /v1/claims/{id}/verifications", f"/v1/claims/{c}/verifications", None),
        ("api", "GET /v1/agents", "/v1/agents?limit=20", None),
        ("api", "GET /v1/search", "/v1/search?q=web+search+api", None),
        ("api", "GET /v1/cli/manifest", "/v1/cli/manifest", None),
    ]
    if token:
        t += [
            ("api", "GET /v1/notifications (auth)", "/v1/notifications?limit=20", "auth"),
            ("api", "GET /v1/notifications/unread-count (auth)", "/v1/notifications/unread-count", "auth"),
        ]
    if with_cli:
        t += [
            ("cli", "krab --version", ["--version"], None),
            ("cli", "krab battle list", ["battle", "list", "--limit", "20"], None),
            ("cli", "krab battle get", ["battle", "get", b], None),
            ("cli", "krab claim get", ["claim", "get", c], None),
            ("cli", "krab search", ["search", "web search api"], None),
        ]
        if token:
            t += [
                ("cli", "krab notifications", ["notifications", "--limit", "20"], None),
                ("cli", "krab notifications unread-count", ["notifications", "unread-count"], None),
            ]
    return t


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base-url", default="https://krabarena.com")
    ap.add_argument("-n", type=int, default=8)
    ap.add_argument("--no-cli", action="store_true")
    ap.add_argument("--json")
    ap.add_argument("--only", help="comma-separated target names to run (exact match)")
    ap.add_argument("--battle", help="battle id or slug to measure (default: the busiest of the first 20)")
    args = ap.parse_args()
    base = args.base_url.rstrip("/")
    token = cli_token()
    ids = discover(base, token, args.battle)
    only = {n.strip() for n in args.only.split(",")} if args.only else None
    rows, failures = [], 0
    print(f"{'type':5} {'target':42} {'code':>4} {'ttfb p50':>9} {'p50':>7} {'p90':>7} {'max':>7} {'size':>8} {'q':>3} {'app p50':>8}  verdict")
    for kind, name, target, extra in targets(ids, token, not args.no_cli):
        if only and name not in only:
            continue
        samples = []
        for i in range(args.n + 1):
            if kind == "cli":
                r = run_cli(target)
            else:
                r = curl(base + target, accept=extra if extra != "auth" else None,
                         token=token if extra == "auth" else None)
            if i:
                samples.append(r)
            if kind != "cli":
                time.sleep(PACE_S)
        code = samples[-1][0]
        ttfb = [s[1] for s in samples]
        total = [s[2] for s in samples]
        p50, p90 = statistics.median(total), pct(total, 90)
        b50, b90 = BUDGETS[kind]
        ok_code = code == 0 if kind == "cli" else 200 <= code < 400
        over = p50 > b50 or p90 > b90
        verdict = "FAIL(code)" if not ok_code else ("SLOW" if over else "ok")
        failures += verdict != "ok"
        queries = samples[-1][4]
        apps = [s[5] for s in samples if s[5] is not None]
        app_p50 = statistics.median(apps) / 1000 if apps else None
        q_col = "" if queries is None else str(queries)
        app_col = "" if app_p50 is None else f"{app_p50:.3f}"
        print(f"{kind:5} {name[:42]:42} {code:>4} {statistics.median(ttfb):9.3f} {p50:7.3f} {p90:7.3f} {max(total):7.3f} {samples[-1][3]:>8} {q_col:>3} {app_col:>8}  {verdict}")
        rows.append({"kind": kind, "name": name, "code": code, "ttfb_p50": statistics.median(ttfb),
                     "p50": p50, "p90": p90, "max": max(total), "size": samples[-1][3],
                     "db_queries": queries, "app_p50": app_p50, "verdict": verdict})
    print(f"\nbudgets (p50/p90 s): " + ", ".join(f"{k} {v[0]}/{v[1]}" for k, v in BUDGETS.items()))
    if args.json:
        json.dump({"base_url": base, "n": args.n, "rows": rows}, open(args.json, "w"), indent=2)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
