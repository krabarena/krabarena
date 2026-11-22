#!/usr/bin/env bash
set -euo pipefail

db=${1:?database url}
script=${2:?script path}
name=${3:?backfill name}
mode=${4:?dry-run|apply}

sha=$(sha256sum "$script" | cut -d' ' -f1)
run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-local}/actions/runs/${GITHUB_RUN_ID:-0}"
actor=${GITHUB_ACTOR:-unknown}

psql_q() { psql "$db" -v ON_ERROR_STOP=1 -X -q "$@"; }

psql_q -c "CREATE TABLE IF NOT EXISTS _backfills (
  name text PRIMARY KEY,
  sha256 text NOT NULL,
  applied_at timestamptz NOT NULL DEFAULT now(),
  actor text NOT NULL,
  run_url text NOT NULL
)"

applied=$(psql_q -At -v name="$name" <<'SQL'
SELECT count(*) FROM _backfills WHERE name = :'name';
SQL
)
if [ "$applied" != 0 ]; then
  echo "Backfill $name was already applied to this database; refusing to run it again." >&2
  exit 1
fi

echo "== $mode: $script (sha256 $sha)"
wrapper=$(mktemp)
trap 'rm -f "$wrapper"' EXIT
cat > "$wrapper" <<SQL
\\i $(cd "$(dirname "$script")" && pwd)/$(basename "$script")
INSERT INTO _backfills (name, sha256, actor, run_url) VALUES (:'name', :'sha', :'actor', :'run_url');
SQL
psql "$db" -v ON_ERROR_STOP=1 -X --single-transaction --echo-errors \
  -v name="$name" -v sha="$sha" -v actor="$actor" -v run_url="$run_url" \
  -f "$wrapper"
echo "== $mode finished"
