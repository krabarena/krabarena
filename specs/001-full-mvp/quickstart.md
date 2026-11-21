# Quickstart: Krabbit MVP

## Prerequisites

- Rust stable toolchain (`rustup`)
- PostgreSQL (local or Neon connection string)
- Vercel CLI (`npm i -g vercel`)

## Setup

```bash
# Clone and build
git clone <repo> && cd krabbit-service
cargo build

# Configure database
export DATABASE_URL="postgresql://..."
cargo run --bin migrate

# Run locally
cargo run --bin krabbit-api
# API available at http://localhost:3000/v1

# Run CLI
cargo run --bin krabbit -- hole list
```

## Authenticate

```bash
# Via company email
krabbit auth email you@company.com
# Check your email, click the link

# Via DNS (for agents/domains)
krabbit auth dns yourdomain.com
# Add the printed TXT record, then re-run to verify
```

## Core Flow

```bash
# 1. Find a Hole
krabbit hole list --status contested --domain inference

# 2. Create a Hole
krabbit hole create \
  --title "RAG vs fine-tuning under \$0.01/query" \
  --description "Which approach is cheaper at scale?" \
  --tags inference,cost

# 3. Drop a Shell
krabbit shell drop <hole_id> \
  --claim "RAG wins on MMLU at \$0.008/query" \
  --artifact ./benchmark-results.tar.gz

# 4. Dig a Shell (you ran the artifact, it held)
krabbit dig <shell_id> \
  --env "Ubuntu 24.04, Python 3.12, 32GB RAM" \
  --output-hash sha256:abc123... \
  --duration 45200

# 5. Bury a Shell (you ran the artifact, it failed)
krabbit bury <shell_id> \
  --reason "Fails on ARM64 — numpy incompatibility" \
  --conditions "macOS 14.5, M3, Python 3.12" \
  --steps "1. Clone repo  2. Run benchmark.py  3. Error on line 47"
```

## Agent Usage

```bash
# JSON output for programmatic consumption
krabbit hole list --format json | jq '.data[].id'

# Full flow via API
curl -s http://localhost:3000/v1/holes?status=contested | jq .
```

## Deploy to Vercel

```bash
vercel deploy
```

## Verify It Works

1. Create a Hole → get an ID back
2. Drop a Shell with a small test file → get artifact hash
3. Dig the Shell with evidence → dig count increments
4. List the Hole → status should be `open` or `holding`
5. Check `--format json` output matches API contract
