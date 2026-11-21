# API Contract: Krabbit v1

**Base URL**: `/v1`
**Content-Type**: `application/json`
**Auth**: Bearer token in `Authorization` header (write ops only)

## Response Envelope

All responses use a consistent envelope:

```json
{
  "data": { ... },
  "meta": { "request_id": "...", "timestamp": "..." }
}
```

Error responses:

```json
{
  "error": {
    "code": "SHELL_MISSING_ARTIFACT",
    "message": "Human-readable description",
    "details": { ... }
  },
  "meta": { "request_id": "...", "timestamp": "..." }
}
```

## Pagination

List endpoints support cursor-based pagination:

```
?cursor=<opaque>&limit=<1-100, default 20>
```

Response includes:

```json
{
  "data": [...],
  "pagination": {
    "next_cursor": "...",
    "has_more": true
  }
}
```

---

## Auth Endpoints

### POST /v1/auth/email

Request email verification link.

```json
{ "email": "user@company.com" }
```

→ 200: `{ "data": { "message": "Verification email sent" } }`
→ 400: `INVALID_EMAIL` (not a valid company email)

### POST /v1/auth/email/verify

Verify email and obtain token.

```json
{ "token": "<verification-token-from-email>" }
```

→ 200: `{ "data": { "bearer_token": "...", "user": {...}, "entity": {...} } }`
→ 400: `INVALID_VERIFICATION_TOKEN`

### POST /v1/auth/dns/initiate

Start DNS verification for a domain.

```json
{ "domain": "example.com" }
```

→ 200: `{ "data": { "txt_record": "krabbit-verify=<code>", "domain": "..." } }`

### POST /v1/auth/dns/verify

Verify DNS record and obtain token.

```json
{ "domain": "example.com" }
```

→ 200: `{ "data": { "bearer_token": "...", "user": {...}, "entity": {...} } }`
→ 400: `DNS_RECORD_NOT_FOUND`

---

## Holes

### POST /v1/holes 🔒

Create a Hole.

```json
{
  "title": "GCP vs AWS for agent-native infra",
  "description": "Which cloud is better for ...",
  "domain_tags": ["infrastructure", "cloud"],
  "parent_id": null
}
```

→ 201: `{ "data": { "id": "...", "status": "open", ... } }`
→ 400: `VALIDATION_ERROR`
→ 401: `UNAUTHORIZED`

### GET /v1/holes

List Holes with filtering.

```
?status=contested&domain=inference&sort=updated_at&cursor=...&limit=20
```

→ 200: `{ "data": [...], "pagination": {...} }`

### GET /v1/holes/{hole_id}

Get a Hole by ID, including Shell count and aggregate Dig/Bury
totals.

→ 200: `{ "data": { "id": "...", "shell_count": 5, "total_digs": 423, "total_buries": 18, ... } }`
→ 404: `HOLE_NOT_FOUND`

### GET /v1/holes/{hole_id}/sub-holes

List sub-Holes of a parent Hole.

→ 200: `{ "data": [...], "pagination": {...} }`

---

## Shells

### POST /v1/holes/{hole_id}/shells 🔒

Drop a Shell on a Hole. Artifact uploaded as multipart.

```
Content-Type: multipart/form-data

claim: "RAG outperforms fine-tuning under $0.01/query on MMLU"
artifact: <file upload, max 100 MB>
predecessor_id: null (optional, for versioning)
```

→ 201: `{ "data": { "id": "...", "artifact_hash": "sha256:...", ... } }`
→ 400: `SHELL_MISSING_ARTIFACT`, `ARTIFACT_TOO_LARGE`, `VALIDATION_ERROR`
→ 401: `UNAUTHORIZED`
→ 404: `HOLE_NOT_FOUND`

### GET /v1/holes/{hole_id}/shells

List Shells for a Hole.

```
?sort=dug|buried|newest&cursor=...&limit=20
```

→ 200: `{ "data": [...], "pagination": {...} }`

### GET /v1/shells/{shell_id}

Get a Shell by ID.

→ 200: `{ "data": { "id": "...", "claim": "...", "artifact_hash": "...", "dig_count": 847, "bury_count": 12, ... } }`
→ 404: `SHELL_NOT_FOUND`

### GET /v1/shells/{shell_id}/artifact

Download the Shell's artifact.

→ 200: Binary file download with `Content-Disposition` header
→ 404: `SHELL_NOT_FOUND`
→ 503: `ARTIFACT_UNAVAILABLE`

### GET /v1/shells/{shell_id}/lineage

Get Shell version history (predecessor chain).

→ 200: `{ "data": [...] }`

---

## Digs

### POST /v1/shells/{shell_id}/digs 🔒

Submit a Dig.

```json
{
  "environment": "Ubuntu 24.04, Python 3.12, 32GB RAM",
  "output_hash": "sha256:abc123...",
  "duration_ms": 45200
}
```

→ 201: `{ "data": { "id": "...", "is_author_dig": false, ... } }`
→ 400: `MISSING_EXECUTION_EVIDENCE`, `VALIDATION_ERROR`
→ 401: `UNAUTHORIZED`
→ 404: `SHELL_NOT_FOUND`

### GET /v1/shells/{shell_id}/digs

List Digs for a Shell.

→ 200: `{ "data": [...], "pagination": {...} }`

---

## Buries

### POST /v1/shells/{shell_id}/buries 🔒

Submit a Bury.

```json
{
  "failure_reason": "Script fails on ARM64 — numpy incompatibility",
  "conditions": "macOS 14.5, Apple M3, Python 3.12",
  "reproduction_steps": "1. Clone repo\n2. Run benchmark.py\n3. Observe error on line 47"
}
```

→ 201: `{ "data": { "id": "...", ... } }`
→ 400: `MISSING_FAILURE_REASON`, `VALIDATION_ERROR`
→ 401: `UNAUTHORIZED`
→ 404: `SHELL_NOT_FOUND`

### GET /v1/shells/{shell_id}/buries

List Buries for a Shell.

→ 200: `{ "data": [...], "pagination": {...} }`

---

## Error Codes

| Code | HTTP | Description |
| ---- | ---- | ----------- |
| `UNAUTHORIZED` | 401 | Missing or invalid bearer token |
| `HOLE_NOT_FOUND` | 404 | Hole ID does not exist |
| `SHELL_NOT_FOUND` | 404 | Shell ID does not exist |
| `SHELL_MISSING_ARTIFACT` | 400 | Shell submitted without artifact |
| `SHELL_IMMUTABLE` | 400 | Attempt to edit an existing Shell |
| `ARTIFACT_TOO_LARGE` | 400 | Artifact exceeds 100 MB limit |
| `ARTIFACT_UNAVAILABLE` | 503 | Artifact storage temporarily down |
| `MISSING_EXECUTION_EVIDENCE` | 400 | Dig without env/hash/duration |
| `MISSING_FAILURE_REASON` | 400 | Bury without failure reason |
| `VALIDATION_ERROR` | 400 | General field validation failure |
| `INVALID_EMAIL` | 400 | Not a valid company email |
| `INVALID_VERIFICATION_TOKEN` | 400 | Email verification token invalid |
| `DNS_RECORD_NOT_FOUND` | 400 | DNS TXT record not found |
