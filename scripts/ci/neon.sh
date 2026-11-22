#!/usr/bin/env bash
set -euo pipefail

: "${NEON_PROJECT_ID:?}" "${NEON_API_KEY:?}"
API="https://console.neon.tech/api/v2/projects/${NEON_PROJECT_ID}"

api() {
  local method=$1 path=$2
  shift 2
  curl -fsS -X "$method" -H "Authorization: Bearer ${NEON_API_KEY}" \
    -H "Accept: application/json" -H "Content-Type: application/json" "$API$path" "$@"
}

branch_id() {
  api GET /branches | jq -r --arg n "$1" '.branches[] | select(.name == $n) | .id' | head -1
}

main_id() {
  api GET /branches | jq -r '.branches[] | select(.default == true) | .id'
}

wait_idle() {
  local id=$1
  for _ in $(seq 1 90); do
    local busy state
    busy=$(api GET "/operations?limit=50" | jq --arg b "$id" \
      '[.operations[] | select(.branch_id == $b and (.status == "running" or .status == "scheduling"))] | length')
    state=$(api GET "/branches/$id" | jq -r .branch.current_state)
    if [ "$busy" = 0 ] && [ "$state" = ready ]; then return 0; fi
    sleep 5
  done
  echo "Neon branch $id did not become ready" >&2
  return 1
}

wait_gone() {
  for _ in $(seq 1 60); do
    [ -z "$(branch_id "$1")" ] && return 0
    sleep 5
  done
  echo "Neon branch $1 was not deleted" >&2
  return 1
}

delete_branch() {
  local id
  id=$(branch_id "$1")
  if [ -z "$id" ]; then
    echo "No Neon branch named $1" >&2
    return 0
  fi
  api DELETE "/branches/$id" >/dev/null
  wait_gone "$1"
  echo "Deleted Neon branch $1" >&2
}

create_branch() {
  local body
  body=$(jq -n --arg n "$1" --arg p "$(main_id)" \
    '{branch: {name: $n, parent_id: $p}, endpoints: [{type: "read_write"}]}')
  api POST /branches -d "$body" | jq -r .branch.id
}

connection_uri() {
  api GET "/connection_uri?branch_id=$1&database_name=neondb&role_name=neondb_owner&pooled=$2" | jq -r .uri
}

usage() {
  cat >&2 <<'USAGE'
Usage:
  neon.sh ensure <branch-name> [fresh=true|false] [pooled=true|false]
      Create <branch-name> from the protected prod branch if missing (or
      re-create it when fresh=true) and print its connection URI on stdout.
  neon.sh delete <branch-name>
  neon.sh list
USAGE
  exit 2
}

case "${1:-}" in
  ensure)
    name=${2:?branch name}
    fresh=${3:-false}
    pooled=${4:-true}
    id=$(branch_id "$name")
    if [ -n "$id" ] && [ "$fresh" = true ]; then
      delete_branch "$name"
      id=""
    fi
    if [ -z "$id" ]; then
      id=$(create_branch "$name")
      echo "Created Neon branch $name ($id) from prod" >&2
    fi
    wait_idle "$id"
    connection_uri "$id" "$pooled"
    ;;
  delete)
    delete_branch "${2:?branch name}"
    ;;
  list)
    api GET /branches | jq -r '.branches[] | "\(.name)\t\(.created_at)"'
    ;;
  *)
    usage
    ;;
esac
