# Battle Proxy Spec

## What is this, in one paragraph

A Battle that wants to test a commercial API (Exa, OpenAI, Anthropic,
…) hits a chicken-and-egg problem: whoever runs the experiment needs
a real API key, and we don't want Claimers pasting their own keys
into a public Battle just to participate. The **proxy spec** is the
way around that. The Battle's *sponsor* (you) stores a set of
upstream API configurations — including the API keys — on the
Battle. Claimers then send their requests to KrabArena; KrabArena
injects the real key and forwards to the real API. Claimers never
see the key, KrabArena logs every request, and the Battle's budget
gets debited per call — at a flat per-request rate, or at the exact
price the upstream reports for usage-priced APIs (see
[Actual-cost billing](#actual-cost-billing)).

## The request flow

A Claimer running a benchmark makes a request like:

```
POST https://krabarena.com/v1/battles/09ab7.../proxy/keenable/search
Authorization: Bearer <their KrabArena JWT>
Content-Type: application/json

{"query": "how tall is mount kilimanjaro"}
```

What KrabArena does with that:

1. Auths the KrabArena bearer (verifies it's a real user).
2. Looks at the Battle's `proxy_spec.services["keenable"]`.
3. Checks the method and path against the service's `allowed_methods` /
   `allowed_paths` (if set) — anything else is a `403`.
4. Checks the budget and **reserves** the most this call can cost (the
   per-request cap, else the flat rate) under a per-Battle lock, before
   anything is sent upstream.
5. Takes the `upstream` from that config — say `https://api.keenable.ai` —
   and makes the actual request to `https://api.keenable.ai/search` with
   the Claimer's body, but with the sponsor's `headers` from the spec
   glued on (e.g. `x-api-key: kbl_live_…`).
6. Returns the response to the Claimer unmodified (responses over 20 MB
   are refused with `502`).
7. Settles the reservation to `cost_per_1k_cents / 1000` for this
   service — or, if the service has a `billing` block, the actual cost
   the upstream reported in its response — and logs the request for
   audit. A call that was sent upstream but never completed (the
   Claimer hung up, the upstream timed out, the body was too large) is
   still charged: at `max_per_request_cents` if set, else the flat rate.

So: **Claimer's request → KrabArena → real API**, with KrabArena
owning the secrets.

## Field glossary (plain English)

| Field | What it means | Example |
|---|---|---|
| `services` | Dictionary from a name you invent (e.g. `"keenable"`) to a config for that API. The name you pick here is what Claimers put in their URL path. | `{"keenable": {...}, "openai": {...}}` |
| `services.<name>.upstream` | The **real** base URL of the API KrabArena should forward to. Must start with `https://` and name a public host — private, loopback, link-local and metadata addresses (`10.x`, `127.0.0.1`, `169.254.169.254`, `*.internal`, `localhost`) are rejected. The proxy does not follow upstream redirects (3xx is passed back to the caller). No trailing path — KrabArena appends whatever the Claimer sent after `/proxy/<name>/`. | `"https://api.keenable.ai"` |
| `services.<name>.docs_url` | **REQUIRED** — a link to the upstream provider's public API docs so Claimers know how to call it (what paths, what body shape, what response to expect). Shown in `proxy-info` next to the service name. Must be an absolute URL. | `"https://docs.keenable.ai/search"` |
| `services.<name>.headers` | A dictionary of HTTP headers that KrabArena attaches to every forwarded request to this service. This is where API keys live. There is **no separate `api_key` field** — whatever header the upstream expects goes here. | `{"x-api-key": "kbl_live_…"}` or `{"authorization": "Bearer sk-…"}` |
| `services.<name>.cost_per_1k_cents` | How many cents the sponsor pays per **1000 requests** to this service. Per-K instead of per-request because every modern search/retrieval API charges sub-cent per call (e.g. Exa $7/1K, BrightData $1.50/1K) — integer per-K cents lets the spec hit those prices exactly. `0` = free/untracked. Old specs that used `cost_per_request_cents` still parse via a serde alias, but the numeric value is reinterpreted in the new unit. | `100` (= $1 per 1000 requests = $0.001/req) |
| `services.<name>.billing` | Optional. Bill this service at the **actual** price the upstream reports per call (Monid, OpenRouter, Exa all return it in the response) instead of the flat `cost_per_1k_cents`. See [Actual-cost billing](#actual-cost-billing). Absent = flat billing. | `{"type": "response_json", "cost_path": "cost.value", "dedupe_key_path": "runId", "max_per_request_cents": 10}` |
| `services.<name>.aws_sigv4` | Optional. For AWS APIs that take IAM credentials instead of an API key (e.g. Amazon Bedrock AgentCore Web Search): KrabArena signs every forwarded request with AWS Signature V4. Fields: `access_key_id`, `secret_access_key`, `region`, `service` (the SigV4 signing name), optional `session_token`. See [AWS SigV4 signing](#aws-sigv4-signing). | `{"access_key_id": "AKIA…", "secret_access_key": "…", "region": "us-east-1", "service": "bedrock-agentcore"}` |
| `services.<name>.allowed_methods` | Optional. HTTP methods Claimers may use on this service, e.g. `["GET", "POST"]`. Anything else returns `403` listing what's allowed. Absent / empty = every method (legacy). Strongly recommended: without it a Claimer can call **any** method — `DELETE`, `PUT` — with your key. Valid values: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS`. | `["POST"]` |
| `services.<name>.allowed_paths` | Optional. Paths Claimers may call (the part after `/proxy/<name>`, query string excluded). Each entry starts with `/` and is either a **prefix** matched on a segment boundary (`/v1/search` allows `/v1/search` and `/v1/search/x`, not `/v1/searchx`) or a **glob** where `*` matches any characters including `/` (`/v3/*`, `/v1/*/search`). Anything else returns `403`. Absent / empty = every path (legacy). Strongly recommended: without it a Claimer can reach your account, team, billing or key-management endpoints with your key. | `["/v3/*"]`, `["/web-search/web-search.v1/invoke"]` |
| `services.<name>.pricing_note` | A short human-readable string shown in the public `proxy-info` endpoint. Cosmetic only — the server doesn't read it. | `"$0.007 per search"` |
| `budget_cents` | The **total** amount (in cents) the sponsor is willing to pay across *all* services and *all* Claimers for this Battle. `10000` = $100. Default `0` = unlimited. Exhausting the budget returns 402 on further proxied calls. | `10000` |
| `per_claim_cents` | A per-Claim cap inside the total budget. Prevents one Claimer from burning the whole budget. `0` = no per-Claim cap. | `500` (= $5 per Claimer) |
| `rate_limit_rpm` | Rate limit in requests per minute, enforced **per Claimer (user), per Battle**, summed across all of this Battle's services, in fixed 60-second windows. Proxy calls are exempt from KrabArena's platform-wide request limit (120/min per user), so this value is the **effective** limit — `600` really means 600/min. Over the limit returns `429` with `Retry-After` and `RateLimit-Limit` / `RateLimit-Remaining` / `RateLimit-Reset` headers (also stamped on every successful proxied response). `0` falls back to the platform default (120/min). Default `60`. Battles with a budget also cap how many of their calls are checked against the budget at once; when too many arrive together the extra ones get `429` with code `PROXY_BUSY` after a few seconds — retry with backoff. | `60` |

### Why the field names look the way they do

- **`upstream`** not "url" or "base_url": it's the network term for
  "the server we're forwarding to". Matches common proxy / load-
  balancer vocabulary.
- **`headers`** not "api_key": different APIs put the secret in
  different header names (`authorization` for Bearer, `x-api-key`
  for most B2B, sometimes `api-key` with no prefix). We take the
  whole header dictionary so the sponsor just copies whatever the
  provider's docs show.
- **`_cents`** everywhere: integer cents avoids float rounding. `100
  = $1.00`. Every money field is `i32` cents, always non-negative.

## Actual-cost billing

Usage-priced APIs don't have one price per request: a Monid run costs
whatever the underlying endpoint costs, an OpenRouter call costs its
tokens, an Exa call costs more with `text` / `summary`. All three
report the real price in the response body. Add a `billing` block to
the service and KrabArena charges that reported amount (with
micro-dollar precision — $0.004 is recorded as $0.004, not rounded to
0¢ or 1¢) against `budget_cents` and `per_claim_cents`.

| Field | What it means | Example |
|---|---|---|
| `type` | **REQUIRED.** `response_json` — cost is a number (or numeric string) in the JSON response body; for a streamed body (only when the upstream's `Content-Type` is `text/event-stream`), the last `data:` chunk that contains it. `response_header` — cost is the value of a response header. | `"response_json"` |
| `cost_path` | **REQUIRED for `response_json`.** Dot-separated path to the cost; numeric segments index arrays. A list means "first path that resolves wins". An entry can be `{"path": …, "unit": …}` to override `unit` for that path (for APIs that report the same cost in different units on different endpoints). | `"cost.value"`, `["usage.cost", "usage.total_cost"]`, `"costDollars.total"`, `["cost.value", {"path": "billing.reportedCost.value", "unit": "micro_usd"}]` |
| `header` | **REQUIRED for `response_header`.** Response header carrying the cost. | `"x-cost-usd"` |
| `unit` | Unit of the reported number: `usd` (default), `cents`, `micro_usd`. | `"usd"` |
| `currency` | Only `USD` is supported (budgets are USD cents). Default `USD`. | `"USD"` |
| `currency_path` | Optional path to the response's own currency code. If it says something other than `currency`, the reported cost is ignored and the flat rate is charged. | `"cost.currency"` |
| `dedupe_key_path` | Optional path (or list of paths) to an id for the billable unit. Each id is charged **once per Battle + service** at the highest cost reported for it: polling a run that repeats its cost bills `0`, and a poll reporting a larger final cost bills only the increase. The key is claimed only by a response that reports a cost > 0, so a still-running run (no cost yet) doesn't block its completed poll from being billed. | `"runId"`, `["runId", "id"]` |
| `max_per_request_cents` | Optional hard ceiling for one request's charge, in cents (fractions allowed, at most `10000` = $100). Higher reported costs are clamped to it and logged. It is also the amount held against the budget while the call is in flight, and what a call that is sent but never completes is charged. Strongly recommended. | `10` |

How a request with a `billing` block is charged:

1. Before forwarding, KrabArena checks `budget_cents` / `per_claim_cents`
   and reserves `max_per_request_cents` (or the flat rate if unset)
   under a per-Battle lock, so concurrent calls can't all slip under the
   cap at once. (Flat-rate services reserve their flat rate the same
   way.)
2. After the response arrives, the full body is parsed (before the
   100 KB log truncation; the bytes returned to the Claimer are not
   modified) and the reservation is settled to:
   - the reported cost (clamped to `max_per_request_cents`), or
   - only the increase over what was already billed if
     `dedupe_key_path` names an id already billed (`0` for a poll
     repeating the same cost), or
   - the flat `cost_per_1k_cents / 1000` if the cost is missing,
     unparseable, negative, above the $100/request sanity ceiling, or
     in the wrong currency (errors, discovery endpoints, a run that
     hasn't finished yet).
3. Requests made before the `billing` block was added stay billed at
   the flat rate. A call that was sent upstream but never completed
   (the Claimer hung up, the upstream timed out, the response exceeded
   20 MB) is settled at `max_per_request_cents` (else the flat rate). A
   reservation orphaned by a server crash keeps counting at that same
   amount and is swept to `abandoned` after 5 minutes.

### Examples

**Monid** — one balance for hundreds of tool endpoints. `POST /v1/run`
returns `runId`; when the run is `COMPLETED` inline it reports the price
as `"billing": {"reportedCost": {"value": 4000, "unit": "MICRO_DOLLAR"}}`,
while `GET /v1/runs/{runId}` polls report
`"cost": {"value": 0.004, "currency": "USD"}`. Read both (each with its
unit) and dedupe on the run id so a run that is polled after completing
inline is billed once (`price.amount.value` is the list price, not
what was charged — don't bill from it):

```json
"monid": {
  "upstream": "https://api.monid.ai",
  "docs_url": "https://monid.ai/SKILL.md",
  "headers": { "Authorization": "Bearer monid_live_…" },
  "cost_per_1k_cents": 0,
  "billing": {
    "type": "response_json",
    "cost_path": [
      "cost.value",
      { "path": "billing.reportedCost.value", "unit": "micro_usd" }
    ],
    "currency_path": "cost.currency",
    "dedupe_key_path": ["runId", "id"],
    "max_per_request_cents": 10
  }
}
```

`/v1/discover`, `/v1/inspect` and pending runs carry no cost and fall
back to `cost_per_1k_cents` (here `0`).

**OpenRouter** — every chat completion includes `usage.cost` (USD) in
the response body; for `stream: true` it arrives in the final SSE chunk,
which KrabArena reads too. (The old `usage: {"include": true}` request
flag is no longer needed — OpenRouter now always returns usage.) Keep a
flat `cost_per_1k_cents` as the fallback for responses without usage
(errors):

```json
"openrouter": {
  "upstream": "https://openrouter.ai",
  "docs_url": "https://openrouter.ai/docs/api-reference/overview",
  "headers": { "Authorization": "Bearer sk-or-…" },
  "cost_per_1k_cents": 100,
  "billing": {
    "type": "response_json",
    "cost_path": ["usage.cost", "usage.total_cost"],
    "max_per_request_cents": 25
  }
}
```

**Microsoft Web IQ** — flat-rate, `x-apikey` header, every search
endpoint under `/v3`. Lock the key to `POST /v3/*` so Claimers can't
reach anything else on the account:

```json
"webiq": {
  "upstream": "https://api.microsoft.ai",
  "docs_url": "https://learn.microsoft.com/en-us/web-iq/reference/web-search",
  "headers": { "x-apikey": "…" },
  "cost_per_1k_cents": 700,
  "allowed_methods": ["POST"],
  "allowed_paths": ["/v3/*"]
}
```

**Exa** — every response carries `costDollars.total`:

```json
"exa": {
  "upstream": "https://api.exa.ai",
  "docs_url": "https://docs.exa.ai/reference/search",
  "headers": { "x-api-key": "…" },
  "cost_per_1k_cents": 700,
  "billing": {
    "type": "response_json",
    "cost_path": "costDollars.total",
    "max_per_request_cents": 10
  }
}
```

## AWS SigV4 signing

AWS APIs don't accept a static key in a header: each request is signed
over its method, path, query, body and a timestamp. Add an `aws_sigv4`
block and KrabArena signs every forwarded request with the sponsor's IAM
credentials (`host`, `x-amz-content-sha256`, `x-amz-date`, plus
`x-amz-security-token` for temporary credentials). Claimers call the
proxy exactly like any other service.

Use a dedicated IAM user whose only permission is the API being
sponsored, e.g. for Web Search:

```json
{
  "Effect": "Allow",
  "Action": "bedrock-agentcore:InvokeWebSearch",
  "Resource": "arn:aws:bedrock-agentcore:us-east-1:aws:tool/web-search.v1"
}
```

**Amazon Bedrock AgentCore Web Search** — `POST /web-search/web-search.v1/invoke`
with `{"searchQuery": "…", "maxResults": 10}`:

```json
"amazon_web_search": {
  "upstream": "https://bedrock-agentcore.us-east-1.amazonaws.com",
  "docs_url": "https://docs.aws.amazon.com/bedrock-agentcore/latest/APIReference/API_InvokeWebSearch.html",
  "aws_sigv4": {
    "access_key_id": "AKIA…",
    "secret_access_key": "…",
    "region": "us-east-1",
    "service": "bedrock-agentcore"
  },
  "cost_per_1k_cents": 700,
  "pricing_note": "$7 per 1,000 queries",
  "allowed_methods": ["POST"],
  "allowed_paths": ["/web-search/web-search.v1/invoke"]
}
```

The `region` must match the region in `upstream`. The IAM policy above
limits what the key can do on AWS; `allowed_methods` / `allowed_paths`
additionally keep Claimers on the one endpoint you are sponsoring, even
if the IAM user is later granted more actions.

## Canonical shape (schema)

```jsonc
{
  // REQUIRED — OBJECT (map), not an array.
  "services": {
    "keenable": {
      "upstream": "https://api.keenable.ai",              // REQUIRED
      "docs_url": "https://docs.keenable.ai/search",      // REQUIRED
      "headers": { "x-api-key": "kbl_live_…" },           // optional
      "cost_per_1k_cents": 100,                           // optional ($1/1K)
      "pricing_note": "~$0.001 per search",               // optional
      "allowed_methods": ["POST"],                        // optional (default: any)
      "allowed_paths": ["/v1/search", "/v1/fetch"]        // optional (default: any)
    },
    "exa": {
      "upstream": "https://api.exa.ai",
      "docs_url": "https://docs.exa.ai/reference/search",
      "headers": { "x-api-key": "…" },
      "cost_per_1k_cents": 700,                           // fallback rate
      "billing": {                                        // optional
        "type": "response_json",
        "cost_path": "costDollars.total",
        "max_per_request_cents": 10
      }
    },
    "openai": {
      "upstream": "https://api.openai.com",
      "docs_url": "https://platform.openai.com/docs/api-reference",
      "headers": { "authorization": "Bearer sk-…" }
    }
  },
  "budget_cents": 10000,      // optional, default 0 (unlimited)
  "per_claim_cents": 500,     // optional, default 0 (unlimited)
  "rate_limit_rpm": 60        // optional, default 60
}
```

## Minimum valid spec

The smallest thing the server will accept:

```json
{
  "services": {
    "keenable": {
      "upstream": "https://api.keenable.ai",
      "docs_url": "https://docs.keenable.ai/search"
    }
  }
}
```

An empty `services: {}` is rejected with 400 (`proxy_spec.services
must have at least one entry`) because a spec with no services can't
proxy anything. Missing `docs_url` is rejected with
`docs_url is required — set it to the provider's public API docs
URL`, because Claimers need to know how to call the thing you're
handing them.

## Common mistakes

| Mistake | Error you see | Fix |
|---|---|---|
| `services: [ … ]` (array) | `invalid type: sequence, expected a map` | Use an object keyed by service name: `{"keenable": {…}}`. |
| Missing `upstream` | `missing field upstream` | Add `upstream` to every service. |
| `upstream: "api.keenable.ai"` (no scheme) | `upstream must be an absolute URL with scheme` | Prefix `https://`. |
| Missing `docs_url` | `docs_url is required — set it to the provider's public API docs URL` | Add a link to the provider's API docs. |
| `docs_url: "docs.keenable.ai"` (no scheme) | `docs_url must be an absolute URL` | Prefix `https://`. |
| API key at the service top level (`{"api_key": "…"}`) | `unknown field api_key` | Move it into `headers` as whatever header name the upstream documents. |
| `billing` without `cost_path` | `billing.cost_path is required for type 'response_json'` | Add the JSON path to the cost, e.g. `"cost.value"`. |
| `billing.currency: "EUR"` | `currency 'EUR' is not supported — only 'USD'` | Budgets are USD; use a USD cost field. |
| Typo inside `billing` (`"costpath"`) | `unknown field costpath` | `billing` rejects unknown fields so typos don't silently disable it. |
| `pricing_note` as a number | `invalid type: integer, expected a string` | Wrap in quotes. |
| `http://` or private-host `upstream` | `upstream must use https` / `must be a public host` | Use the provider's public `https://` base URL. |
| Trailing slash on `upstream` | silent, usually harmless | Either form works; KrabArena concatenates the request path. |
| `allowed_paths: ["v3/*"]` (no leading slash) | `allowed_paths entry 'v3/*' is invalid` | Paths start with `/`. |
| `allowed_methods: ["FETCH"]` | `allowed_methods has unknown method 'FETCH'` | Use a standard HTTP method. |
| Claimer calls a path outside `allowed_paths` | `403 PROXY_ENDPOINT_NOT_ALLOWED` listing the allowed methods/paths | Call an allowed endpoint, or ask the sponsor to widen the list. |

## Uploading a spec

Use the CLI — it accepts inline JSON or a file path:

```sh
# From a file (keeps keys out of your shell history)
krab battle edit <battle_id> --proxy-spec @proxy-spec.json

# Inline
krab battle edit <battle_id> --proxy-spec \
  '{"services":{"keenable":{"upstream":"https://api.keenable.ai","headers":{"x-api-key":"…"}}},"budget_cents":5000}'
```

On create:

```sh
krab battle create --title "..." --description "..." --tags "..." \
  --proxy-spec @proxy-spec.json
```

The server round-trip-validates on write: it parses your JSON into
the `ProxySpec` Rust struct, checks every service's `upstream` is
absolute, and fails with a clear 400 if anything's off. The Battle
is never left in a half-valid state where the spec looks saved but
the first read explodes.

## What Claimers see (public projection)

Claimers don't see the raw spec — secrets would leak. `GET
/v1/battles/{id}/proxy-info` returns only:

- `services` — the list of service names (so Claimers know what's
  available to them)
- `service_details` — one entry per service with `name`, `docs_url`,
  `pricing_note`, `cost_per_1k_cents`, plus live `request_count`,
  `spent_cents` and `spent_usd` (sub-cent precision) so Claimers can
  jump to the provider's API docs, see the rate, and see how much of
  the sponsored budget this service has already burned. Services with
  a `billing` block also show `billing` (`"response_json"` /
  `"response_header"`) and `actual_cost_requests` (calls charged at an
  upstream-reported price)
- `budget_cents`, `per_claim_cents`
- `rate_limit_rpm` — the effective per-user, per-Battle limit (the
  platform default when the spec sets `0`) and `rate_limit_scope`, a
  one-line description of how it is enforced
- `spent_cents`, `remaining_cents`, `total_requests` (live budget
  tracking; `spent_cents` is floored to whole cents) and `spent_usd`
  (precise total in dollars, including actual costs and in-flight
  reservations)

`upstream` and `headers` are server-side only — these carry the
credentials that let KrabArena forward on the Claimer's behalf.

## Rust definitions (for server contributors)

- Types: `ProxySpec` / `ServiceConfig` —
  `crates/krabarena-api/src/models/proxy.rs`
- Validator: `ProxySpec::validate()` (same file) — the exact check
  `create_battle` and `update_battle` run. If you add a new field,
  extend the validator and this doc together.
- Runtime dispatch: `crates/krabarena-api/src/routes/proxy.rs`
  reads `upstream`, `headers`, `cost_per_1k_cents`, `billing`, and the
  budget fields. Cost extraction / cap / fallback decisions:
  `services/proxy_billing.rs` (pure). Reservation, settlement and
  dedupe: `services/proxy_service.rs` over the `proxy_charges` side
  table (migration 072; micro-dollar `BIGINT`, unique
  `(battle_id, service_name, dedupe_key)`). Authoritative spend per
  service = `SUM(proxy_charges)` + flat rate × `proxy_logs` rows with no
  charge (see `services/proxy_service.rs::get_proxy_info`); the legacy
  `proxy_budgets.total_spent_cents` cache is still written by
  `log_request` for back-compat but no longer read.
- Since migration 079 every proxied call — flat-rate too — gets a
  `proxy_charges` row reserved before the upstream call (`source`
  `flat` / `abandoned` added). A drop guard in `routes/proxy.rs`
  releases it if the request was never sent, or settles it as
  `abandoned` (cap, else flat) and writes the `proxy_logs` row if the
  handler is dropped / times out after sending. Request and response
  bodies in `proxy_logs` are truncated at 100 KB; upstream responses
  are capped at 20 MB.
- Proxy calls whose battle / spec / service doesn't resolve count
  against a per-user `rl:proxymiss:` bucket (`web::rate_limit`), sized
  like the global tier, checked before any DB lookup.
