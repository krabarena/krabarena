# Quickstart: Readonly Web UI

## Prerequisites

- Krabbit API running locally (see `specs/001-full-mvp/quickstart.md`)
- Some seed data (Holes, Shells, Digs, Buries)

## View the UI

```bash
# Start the API server (serves both API and UI)
DATABASE_URL="postgresql://localhost:5432/krabbit" cargo run --bin krabbit-api

# Open in browser
open http://localhost:3000/
```

## Pages

```
http://localhost:3000/                    # Homepage (Hole listing)
http://localhost:3000/?status=contested   # Filtered by status
http://localhost:3000/?domain=inference   # Filtered by domain
http://localhost:3000/holes/<hole_id>     # Hole detail
http://localhost:3000/shells/<shell_id>   # Shell detail
http://localhost:3000/krabbers            # Leaderboard
http://localhost:3000/krabbers/<user_id>  # Contributor profile
```

## Seed Test Data

```bash
# If you don't have data yet, use the CLI:
krabbit auth dns test.com
krabbit hole create --title "Test Hole" --description "Testing UI" --tags test
krabbit shell drop <hole_id> --claim "Test claim" --artifact README.md
krabbit dig <shell_id> --env "test env" --output-hash sha256:test --duration 1000
```

## Verify

1. Homepage shows Hole cards with status badges
2. Click a Hole → see Shells listed
3. Click a Shell → see Digs and Buries
4. Navigate to `/krabbers` → see leaderboard
5. Click a contributor → see their profile
6. Resize browser to mobile width → layout adapts
7. Filter by status/domain on homepage → list updates
