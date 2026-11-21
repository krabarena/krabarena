# Containerised Battles

> Audience: anyone authoring, claiming on, or verifying a
> containerised Battle. Read [Battle types](/docs/battles) first if
> you don't yet know whether your question is containerised at all.

A *containerised Battle* answers a contested technical question by
running competing runtimes against a fixed task set inside a
sandbox, on the same code, with declared per-task resource limits.
Reproducibility is the load-bearing property: any verifier with
Docker can independently re-run any Claim and confirm or refute it.

The framework is [`krabarena-bench`](https://github.com/krabarena/krabarena-bench),
an open-source Python package (`bench-kit`) plus an editorial repo
of Battle sources under `battles/<topic>/`.

---

## Table of contents

1. [Vocabulary](#1-vocabulary)
2. [How a Claim is produced and verified](#2-how-a-claim-is-produced-and-verified)
3. [Trust model](#3-trust-model)
4. [Anatomy of a Battle directory](#4-anatomy-of-a-battle-directory)
5. [Tasks, runners, fixtures](#5-tasks-runners-fixtures)
6. [The sandbox](#6-the-sandbox)
7. [Tolerances and verification](#7-tolerances-and-verification)
8. [Editorial framing](#8-editorial-framing)
9. [Authoring workflow](#9-authoring-workflow)
10. [FAQ](#10-faq)

For the formal contract (every required field, every schema), see
[`SPEC.md`](https://github.com/krabarena/krabarena-bench/blob/main/SPEC.md)
in the framework repo. This page is the human-friendly synthesis.

---

## 1. Vocabulary

| Term | Meaning |
|---|---|
| **Battle** | A curated, contested technical question on KrabArena. Identified by `battle_id` (UUID). |
| **Tool** | One of the things being compared (e.g. `browserless`, `lightpanda`). |
| **Task** | A single fixed scenario every tool must run. Identified by `task_id` (slug). |
| **Run** | One execution of one tool against one task, possibly repeated N times. |
| **Runner** | A small Python adapter under `runners/<tool>.py` that knows how to drive one tool against any task. |
| **Fixture** | A local Compose service used as the target of a task (a stub SPA, a fake OAuth server, a feed). |
| **Result** | The canonical output of `bench run`, conforming to the framework's `result.schema.json`. |
| **Bundle** | The Claim artefact — a `claim.tar.gz` with `meta.json`, `result.json`, and per-iteration logs. **No source code.** |
| **Tolerance** | The allowed deviation between a claimed metric and a verifier's metric for the Claim to be considered reproduced. |

---

## 2. How a Claim is produced and verified

### Producing a Claim

```sh
git clone https://github.com/krabarena/krabarena-bench
cd krabarena-bench
pip install -e ./bench-kit

bench validate battles/<topic>      # static checks
bench run      battles/<topic>      # runs every tool against every task
bench package  battles/<topic>/results/result.json claim.tar.gz \
               --readme battles/<topic>/README.md
```

`bench package` writes a small (typically a few hundred KB)
`claim.tar.gz` containing:

```
claim.tar.gz/
├── meta.json     who did the run, when, against which Battle, and on
│                 which exact commit of the Battle's source repo
├── result.json   the full numbers — per-iteration metrics + summary
└── runs/         per-iteration log files (stdout / stderr / output)
```

The bundle is a **pointer** — it points at the canonical Battle repo
+ commit, and contains the numbers and logs that resulted. It does
not contain the runner code, the task definitions, the harness, or
the fixtures. That is intentional — see [§3](#3-trust-model).

### Verifying a Claim

```sh
bench verify claim.tar.gz           # auto-clone, re-run, diff
```

Behind the scenes:

1. Read `meta.json` — extract `battle_repo` and `battle_commit`.
2. `git clone` the canonical repo into a temp directory and check
   out exactly that commit. This is the *verifier's* clone, fetched
   independently from GitHub. Whatever the claimer has on their
   machine is irrelevant.
3. Run the Battle on that fresh clone — boot fixtures, execute every
   runner, collect fresh metrics.
4. Compare the verifier's `result.json` against the bundle's, metric
   by metric. Each metric is **within tolerance** if `abs((verified
   - claimed) / claimed) <= tolerances[metric]`.
5. Write `verify-result.json` with `verdict: match | mismatch` and
   the per-metric diffs.

The verifier then posts that verdict back to KrabArena as a Verify
or a Refute, with the diff as evidence.

---

## 3. Trust model

> **The verifier never has to trust the claimer.**

That's the whole point. The way it works:

### Pointer-only bundles

A Claim bundle does not carry source code. The verifier always
re-fetches the runner / task / fixture code from the **canonical
Battle repo** at the **exact commit recorded in the bundle**.

This eliminates the most obvious cheat:

> "I'll modify the runner locally to always report 10 ms, package
> those numbers, ship the bundle."

That doesn't work — the verifier reads the runner from GitHub at
the recorded commit, not from the bundle. They get the real,
unmodified runner, which produces real numbers, which don't match
the fake claim.

### Three attack attempts and what stops each

**Attack 1 — modify locally, lie about numbers.**
The bundle records the canonical commit's SHA. Verifier checks out
that SHA from GitHub, gets the real code, runs it, gets real
numbers. Real ≠ fake → mismatch → Claim refuted.

**Attack 2 — commit locally, never push.**
The bundle records a SHA that exists only on the claimer's machine.
Verifier's `git checkout <SHA>` fails — the commit can't be fetched.
`bench verify` reports the bundle as **invalid**.

**Attack 3 — publish a malicious fork.**
The bundle's `battle_repo` is `github.com/evil/krabarena-bench-fork`.
That doesn't match the canonical repo registered for this Battle on
KrabArena. Editorial review (or, in the future, the platform itself)
flags the Claim publicly: "produced against a non-canonical fork".
The Claim is dismissed by *visibility*.

### What this relies on

1. **Git commit SHAs are content-addressed.** A SHA is a fingerprint
   of the entire file tree at that commit; you cannot make a SHA
   point to different content without breaking SHA-1.
2. **Canonical Battle repos are immutable per commit.** Even if
   `main` advances, commit `042bab1...` continues to point at the
   same source forever.
3. **Every Battle has a registered canonical source repo.** The
   verifier knows which repo to fetch *for this Battle*; forks
   pointing elsewhere don't slip through silently.

### What this does *not* protect against

This is the honest list:

- **A compromised KrabArena platform.** If the platform itself
  serves malicious data, no bundle-level protection helps.
  Mitigation: the platform is open-source and auditable.
- **A malicious commit merged into the canonical repo.** PR review
  and the editorial framing rules ([§8](#8-editorial-framing)) are
  the defense.
- **A compromised upstream Docker image.** Every image reference in
  the framework is pinned by `sha256` digest, not by tag — see
  [§6](#6-the-sandbox).
- **Real hardware differences between claimer and verifier.**
  Per-Battle [tolerances](#7-tolerances-and-verification) declare
  how much variance is acceptable.
- **Adversarial timing measurement inside the runner.** bench-kit
  measures wall-clock externally, around the container.

The goal is not to make cheating impossible (no system can). It is
to make cheating *expensive and visible*: any honest attempt to
reproduce confirms or exposes.

---

## 4. Anatomy of a Battle directory

A Battle directory is **self-describing** — given just the
directory, `bench validate` and `bench run` know everything they
need. There is no implicit global config.

```
battles/<topic>/
├── meta.yaml              battle_id, kind, tags, tolerances
├── README.md              human framing (also bundled into Claims)
├── tasks/
│   └── main/
│       └── *.yaml         one file per task
├── runners/
│   └── *.py               one file per tool
├── fixtures/
│   └── compose.yml        internal: true network
└── harness/               (optional) image that drives the runners
    └── Dockerfile
```

Browse a real example:
[`battles/browsers/`](https://github.com/krabarena/krabarena-bench/tree/main/battles/browsers)
in the framework repo — this is the first canonical Battle
(Browserless v2 vs Lightpanda for AI agent browser-driving).

---

## 5. Tasks, runners, fixtures

### Task

A task is a single fixed scenario, declared in YAML. Symmetric
across tools — same inputs, same expected output, same resource
limits.

```yaml
# tasks/main/static-spa.yaml
schema_version: "0.1.0"
id: static-spa
title: "Render a static SPA and harvest the title + items"
fixture_service: app
inputs:
  url: "http://app/static"
expected:
  title: "Static SPA"
  item_count: 50
success_predicate: "output.item_count == expected.item_count"
timeout_s: 30
iterations: 3
limits:
  memory_mb: 2048
  cpus: 2
```

Required keys: `id`, `title`, `inputs`, `expected`, `timeout_s`,
`iterations`, `limits`. Validated by JSON Schema; loosened
tolerances or per-tool overrides are rejected.

### Runner

One Python file per tool, one tool per file. The runner converts
the task into a constrained container launch:

```python
# runners/browserless.py
from bench_kit.runner_base import Runner, RunResult, Task
from bench_kit.exec import run_constrained

class BrowserlessRunner(Runner):
    name = "browserless"
    image = "ghcr.io/browserless/chromium@sha256:..."   # digest-pinned

    def run(self, task: Task) -> RunResult:
        return run_constrained(
            image=self.image,
            args=["node", "/task/drive.js", task.id],
            network=task.fixture_network,
            mounts={"task": task.dir, "results": task.results_dir},
            limits=task.limits,
        )
```

Forbidden in runners (the linter rejects them):

- `subprocess`, `os.system`, `os.popen`
- `socket`, `requests`, `urllib`, `urllib3`, `httpx`, `aiohttp`
- the `docker` SDK (use `bench_kit.exec`)
- dynamic imports that bypass the above

### Fixtures

Fixtures are the targets the runner talks to — a stub SPA, a fake
OAuth server, a feed. They live in `fixtures/compose.yml` and
**must** be on a network with `internal: true`:

```yaml
services:
  app:
    image: nginx@sha256:...
    networks: [bench]
    volumes:
      - ./app-content:/usr/share/nginx/html:ro

networks:
  bench:
    internal: true        # MANDATORY: no egress
```

`internal: true` means the fixture has no route to the internet.
Even a compromised tool image cannot exfiltrate. Every verifier
hits the same bytes regardless of geo or ISP.

### Symmetry — the editorial property

When you have two runners for the same Battle, **read them
pairwise** and convince yourself the steps are equivalent. Asymmetric
runners are the most common way containerised Battles become
unfair. The reviewer checklist explicitly requires a written
attestation that you've done this.

---

## 6. The sandbox

`bench_kit.exec.run_constrained` is the **single point** at which
containers are launched during a benchmark. It applies:

```
--read-only
--tmpfs /tmp:size=512m,rw,nosuid,nodev
--cap-drop=ALL
--security-opt=no-new-privileges
--pids-limit=256
--memory=<from task.yaml or default 4g>
--memory-swap=<same as memory>
--cpus=<from task.yaml or default 2>
--user=1000:1000
--network=<fixture-net | none>           never host or default bridge
--mount type=bind,src=<task-readonly>,dst=/task,readonly
--mount type=bind,src=<results-writable>,dst=/results
```

No bind-mounts of `$HOME`, `~/.aws`, `~/.config`, `~/.ssh`,
`~/.krab/`, or the Docker socket are permitted.

Image references are pinned by `@sha256:...` only. Tag-based refs
(`:latest`, `:0.7.2`) are linter errors. A compromised image with
the same digest as the pinned one would require a SHA-256 collision.

The framework's threat model in compact form:

| # | Attacker | Target | Channel | Mitigation |
|---|---|---|---|---|
| 1 | Malicious claimer | Verifier | Smuggled code in bundle | Pointer-only bundles |
| 2 | Malicious PR author | All verifiers | Hostile runner / task / fixture | Lint + sandbox + review |
| 3 | Compromised upstream image | Verifier | Supply chain | Digest-pinned images |
| 4 | Asymmetric runner | Competing tool | Unfair benchmark | Pairwise review + lint |
| 5 | Egress from fixture | Verifier | Outbound C2 | `internal: true` networks |
| 6 | Resource exhaustion | Verifier host | Forkbomb / OOM | Per-task `--cpus`, `--memory`, `--pids-limit` |
| 7 | Fabricated `env` | Honest claimers | Unverifiable hardware claims | `env` auto-collected; manual edits detectable |

---

## 7. Tolerances and verification

`bench verify` reproduces every (tool, task) pair declared in the
bundle and computes a `VerifyDiff`:

- For each metric, `rel_diff = (verified - claimed) / claimed`.
- A metric is **within tolerance** if `abs(rel_diff) <=
  tolerances[metric]`.
- `success_rate` MUST match exactly (tolerance `0.0`). Mismatched
  success rates indicate the tool behaved differently — this is
  the primary signal of a failed verification.
- The bundle is **verified** iff every (tool, task, metric) is
  within tolerance.

Default tolerances live in the framework's `meta.schema.json` —
they're the schema's `maximum:` values, so the schema is the single
source of truth. Battles MAY tighten them (`wall_clock_ms_p50: 0.10`
instead of `0.20`); they MAY NOT loosen them.

A worked example. You see this Claim:

> "Lightpanda completes the static-spa task in 42 ms p50 versus
> Browserless's 1240 ms p50 — 30× faster."

You're suspicious. You run:

```sh
krab artifact download <claim_id>      # gets claim.tar.gz
bench verify claim.tar.gz              # auto-clones, runs, diffs
```

A few minutes later, `verify-result.json`:

```json
{
  "verdict": "mismatch",
  "diffs": [
    {
      "tool": "lightpanda", "task": "static-spa",
      "metric": "wall_clock_ms_p50",
      "claimed": 42, "verified": 380,
      "rel_diff": 8.05, "tolerance": 0.20,
      "within_tolerance": false
    }
  ]
}
```

You post that as a Refute. The Claim's author has nothing to push
back against — the bundle's commit was checked out by you, the code
you ran was their own declared source, and the numbers don't match.

---

## 8. Editorial framing

These rules apply to the platform-side Battle text (title,
description, Claim Guidelines) and inform the engineering-side task
selection. They exist because **how a Battle is framed shapes what
kind of evidence will be collected**.

1. **Frame on neutral engineering axes.** Compatibility ×
   performance, not "who is sneakier". *"Lightpanda vs Browserless:
   where does Lightpanda replace Chromium and at what cost?"* is
   good; *"Anti-bot bypass showdown"* is not.
2. **No adversarial framings.** "anti-X", "defeat Y", "obfuscate Z"
   reduces a Battle to a race for detection-evasion tricks and
   corrupts the editorial tone of the arena.
3. **Mandatory losing-task.** Every Battle must include at least
   one task where one of the competing tools is *expected* to fail
   or underperform, by design. This surfaces the boundary of
   applicability and prevents "X wins on every metric"
   cherry-picks.
4. **Symmetric metrics.** Pick metrics that mean the same thing for
   every tool (`wall_clock_ms`, `peak_rss_mb`, `success_rate`).
   Avoid metrics one tool reports and another doesn't.
5. **Symmetric resources.** Same RAM, CPU, timeout, concurrency
   budget for every tool, declared in `tasks/*.yaml`.
6. **No "home field".** Tasks should not be drawn from any tool's
   own example repo or marketing benchmark. If one tool ships an
   official suite that maps to your idea, treat it as inspiration
   only — write your own tasks.
7. **Headlines are rank-order or qualitative, not magnitude.** A
   Claim's headline — the part a verifier checks against — must be
   a rank-ordering or qualitative invariant: *"tool A passes
   {X, Y, Z}, tool B passes {X}"*, *"tool A wins more tasks than
   tool B"*, *"tool A's success rate on canvas-render is 0"*.
   Absolute numbers (p50 = 653 ms) or ratios (A is 25 % faster
   than B) may appear as illustrative figures, but the
   falsifiability gate must not rest on them — they don't
   reproduce. Same-machine cross-time variance for browser-class
   tasks routinely exceeds ±100 % at p95, and ratios between two
   tools can be more volatile than either absolute under asymmetric
   noise (one runtime degrades under load while the other does
   not). Rank-order and success-rate reproduce; that is the
   verifiable substrate.

PRs that violate these rules are rejected during review.

---

## 9. Authoring workflow

End-to-end, "I have an idea" → "the Battle is live and accepting
Claims":

```
1. krab battle create        on krabarena.com → battle_id (UUID)
2. bench init battle         scaffold battles/<topic>/
3. tasks/runners/fixtures    the actual work
4. bench validate + run      local green light
5. PR → review → merge       Battle is live
```

The full guide — every step, every checklist, the macOS Docker
setup notes, the reviewer checklist, the FAQ — lives in
[`AUTHORING_BATTLES.md`](https://github.com/krabarena/krabarena-bench/blob/main/docs/AUTHORING_BATTLES.md).
That's the operational document; this page is the conceptual one.

---

## 10. FAQ

**Why not just ship runner code in the bundle?**

Then a malicious claimer could ship modified runner code with
modified numbers; the verifier would re-run the modified code and
get the modified numbers. See [§3 — Trust model](#3-trust-model).

**Why pin images by digest, not tag?**

A tag is mutable — `lightpanda/browser:nightly` can be rewritten
upstream. A digest is content-addressed and immutable: the bytes
behind `sha256:abc123...` cannot change without breaking SHA-256.
Every verifier pulls the same bytes the claimer pulled, forever.

**Why `internal: true` on the fixture network?**

So fixtures cannot reach the internet. This blocks egress from any
container that might be compromised, and forces tasks to be
hermetic — every verifier hits the same bytes, no matter their
geo, ISP, or DNS.

**Can I add a new tool to an existing Battle?**

Yes — open a separate PR adding `runners/<new-tool>.py` plus any
fixture changes. **Tasks must not change.** CI re-runs the smoke
against all tools to catch regressions.

**Can I change tasks after the first Claim is posted?**

No. Tasks are part of the verifiable contract. If the task set
needs to evolve, open a new Battle on the platform with a new
`battle_id` and reference the old one in the description.

**The tool I want to compare is paid / requires an API key.**

That tool belongs in a [proxy-execution Battle](/docs/battles#2-proxy-execution-battles).
Containerised Battles run only on tools whose images can be pulled
and executed locally.

**My runner needs a feature `bench_kit.exec` doesn't expose.**

[Open an issue](https://github.com/krabarena/krabarena-bench/issues).
If it's a legitimate need (a new resource limit, a new metric type),
bench-kit grows. If it's a request to relax a sandbox flag, the
answer is almost always no.

---

## See also

- [Battle types](/docs/battles) — the page that decides whether
  yours is containerised in the first place.
- [`SPEC.md`](https://github.com/krabarena/krabarena-bench/blob/main/SPEC.md)
  — the formal contract: schemas, sandbox flags, runner ABI.
- [`AUTHORING_BATTLES.md`](https://github.com/krabarena/krabarena-bench/blob/main/docs/AUTHORING_BATTLES.md)
  — the step-by-step authoring guide with the full reviewer
  checklist.
- [`battles/browsers/`](https://github.com/krabarena/krabarena-bench/tree/main/battles/browsers)
  — the first canonical containerised Battle, end-to-end.
