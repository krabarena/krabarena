#!/usr/bin/env bash
# Isolation test for the public installer (`/install.sh`).
#
# Six phases, each exits non-zero on any failure:
#
#   1. Static syntax — POSIX `sh -n` + `bash -n` against the on-disk
#      install.sh so we never ship a broken script. Runs everywhere.
#
#   2. Local skill bundle packaging — build the skill tarballs the same
#      way release.yml does and assert runtime-critical files are present.
#
#   3. Server-served identity — fetch `/install.sh` from a running
#      `krabarena-api` (defaults to http://127.0.0.1:3100, override
#      with KRAB_TEST_BASE) and confirm it byte-matches the on-disk
#      file. Catches `include_str!` skew or accidental rebuild gaps.
#      Skipped automatically when no server is listening.
#
#   4. macOS install — on Darwin runners, run the installer locally with
#      an isolated HOME and verify the macOS archive installs and runs.
#
#   4b. Hermes install — run `install.sh --hermes` with an isolated HOME
#      against the phase-2 Hermes bundle (via KRAB_SKILLS_BASE_URL) and
#      verify the skills land in the `krabarena` category dir, honour
#      HERMES_HOME, migrate the legacy flat layout, and set the
#      `Hermes Agent` identity. Runs on macOS and glibc Linux hosts.
#
#   5. Ubuntu/glibc install — spin up a clean throwaway container,
#      mount the install.sh read-only, set HOME=/tmp/krabhome (so the
#      host filesystem is untouched), and run:
#
#          sh /install.sh --silent --skills-scope none
#
#      Then verify the binary lands at $HOME/.krab/bin/krab, the
#      config has the expected api_url, and `krab --version` runs.
#
#   6. Alpine/musl rejection — run the same script in Alpine and assert
#      it fails early with a clear glibc requirement message instead of
#      downloading an incompatible Linux binary.
#
#      Container phases are skipped when Docker isn't reachable.
#
# Current platform coverage:
#
#   - macOS runner         full local install success
#   - ubuntu:24.04        full install success, glibc
#   - alpine:latest       expected failure, musl unsupported until a musl artifact exists
#
# Usage:
#   scripts/test-install.sh                # run all phases that are reachable
#   scripts/test-install.sh --syntax-only  # phase 1 only (fast pre-commit hook)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_SH="${REPO_ROOT}/install.sh"
KRAB_TEST_BASE="${KRAB_TEST_BASE:-http://127.0.0.1:3100}"

SYNTAX_ONLY=false
for arg in "$@"; do
  case "$arg" in
    --syntax-only) SYNTAX_ONLY=true ;;
  esac
done

# ── helpers ──────────────────────────────────────────────────────────
say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '  \033[1;32m✓\033[0m %s\n' "$*"; }
skip() { printf '  \033[1;33m·\033[0m %s\n' "$*"; }
die()  { printf '  \033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

# ── phase 1: syntax ──────────────────────────────────────────────────
say "Phase 1: static syntax"

[ -f "$INSTALL_SH" ] || die "install.sh not found at $INSTALL_SH"

if /bin/sh -n "$INSTALL_SH"; then
  ok "POSIX sh -n passes"
else
  die "POSIX sh syntax errors"
fi

if /bin/bash -n "$INSTALL_SH"; then
  ok "bash -n passes"
else
  die "bash syntax errors"
fi

# Reject the most common bashisms that would silently break under
# BusyBox / dash. Not exhaustive (a real shellcheck run is better)
# but cheap and catches the foot-guns.
if grep -nE '\[\[ |==~|\+= ' "$INSTALL_SH" >/dev/null; then
  die "bashisms detected ([[ , =~, +=)"
fi
ok "no obvious bashisms"

# Look for references to `krab` subcommands that don't exist. We don't
# parse clap here; just sanity-check against a known whitelist of
# subcommands the binary actually exposes today.
known_cmds="auth identity doctor manifest-refresh battle claim notifications agents config arena profile"
unknown=""
for cmd in $(grep -oE 'krab [a-z][a-z-]+' "$INSTALL_SH" \
              | awk '{print $2}' | sort -u); do
  case " $known_cmds " in
    *" $cmd "*) ;;
    *) unknown="$unknown $cmd" ;;
  esac
done
if [ -n "$unknown" ]; then
  die "install.sh references unknown krab subcommands:$unknown"
fi
ok "all referenced 'krab <cmd>' invocations exist"

if [ "$SYNTAX_ONLY" = true ]; then
  say "syntax-only run requested — done"
  exit 0
fi

# ── phase 2: local skill bundle packaging ───────────────────────────
say "Phase 2: local skill bundle packaging"

skill_tmp=$(mktemp -d)
trap 'rm -rf "$skill_tmp"' EXIT
for platform in claude-code openclaw codex github-copilot hermes; do
  staging="${skill_tmp}/staging-${platform}"
  mkdir -p "$staging"
  for skill in "${REPO_ROOT}/skills/${platform}/krabarena" "${REPO_ROOT}/skills/${platform}"/krabarena-*; do
    [ -d "$skill" ] && cp -r "$skill" "$staging/"
  done
  if [ "$platform" = hermes ]; then
    for skill in "$staging"/*/; do
      mkdir -p "${skill}references"
      cp "${REPO_ROOT}/docs/claim-structure.md" "${REPO_ROOT}/docs/battle-structure.md" \
        "${REPO_ROOT}/docs/proxy-spec.md" "${skill}references/"
    done
  else
    cp "${REPO_ROOT}/docs/claim-structure.md" "$staging/"
    cp "${REPO_ROOT}/docs/battle-structure.md" "$staging/"
    cp "${REPO_ROOT}/docs/proxy-spec.md" "$staging/"
  fi
  archive="${skill_tmp}/krabarena-skills-${platform}.tar.gz"
  tar czf "$archive" -C "$staging" .
  listing="${archive}.list"
  tar tzf "$archive" > "$listing"
  case "$platform" in
    github-copilot)
      grep -q '^./krabarena/copilot-instructions.md$' "$listing" \
        || die "${platform} bundle missing ./krabarena/copilot-instructions.md"
      grep -q '^./krabarena/krabarena-join-battle.prompt.md$' "$listing" \
        || die "${platform} bundle missing ./krabarena/krabarena-join-battle.prompt.md"
      ;;
    hermes)
      for f in krabarena/SKILL.md krabarena-heartbeat/SKILL.md \
               krabarena/references/claim-structure.md \
               krabarena-heartbeat/references/proxy-spec.md; do
        grep -q "^./${f}\$" "$listing" || die "${platform} bundle missing ./${f}"
      done
      ;;
    *)
      grep -q '^./krabarena/SKILL.md$' "$listing" \
        || die "${platform} bundle missing ./krabarena/SKILL.md"
      ;;
  esac
  ok "${platform} skill bundle contains required files"
done

# ── phase 3: server-served install.sh matches on-disk ────────────────
say "Phase 3: served install.sh matches on-disk"

sha256_file() {
  file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    die "no SHA-256 tool found; install sha256sum or shasum"
  fi
}

if curl -fsS -o /dev/null -m 3 "${KRAB_TEST_BASE}/health" 2>/dev/null; then
  served_install=$(mktemp)
  curl -fsSL "${KRAB_TEST_BASE}/install.sh" -o "$served_install"
  served_sum=$(sha256_file "$served_install")
  disk_sum=$(sha256_file "$INSTALL_SH")
  rm -f "$served_install"
  if [ "$served_sum" = "$disk_sum" ]; then
    ok "served /install.sh sha256 matches on-disk ($disk_sum)"
  else
    die "served /install.sh sha256 ($served_sum) != on-disk ($disk_sum) — rebuild the binary"
  fi
else
  skip "no server listening on ${KRAB_TEST_BASE} — skipping served-content check"
fi

# ── phase 4: macOS local install ─────────────────────────────────────
say "Phase 4: local install (macOS)"

if [ "$(uname -s)" = Darwin ]; then
  macos_home=$(mktemp -d)
  macos_log=$(mktemp)
  if HOME="$macos_home" sh "$INSTALL_SH" --silent --skills-scope none >"$macos_log" 2>&1; then
    test -x "$macos_home/.krab/bin/krab" || {
      cat "$macos_log"
      rm -rf "$macos_home"
      rm -f "$macos_log"
      die "macOS install did not create an executable krab binary"
    }
    grep -q 'api_url' "$macos_home/.krab/config.toml" || {
      cat "$macos_log"
      rm -rf "$macos_home"
      rm -f "$macos_log"
      die "macOS install did not create config.toml with api_url"
    }
    version=$("$macos_home/.krab/bin/krab" --version)
    ok "macOS install OK — $version"
  else
    echo
    echo "--- macOS install output ---"
    cat "$macos_log"
    echo "----------------------------"
    echo
    rm -rf "$macos_home"
    rm -f "$macos_log"
    die "macOS install FAILED"
  fi
  rm -rf "$macos_home"
  rm -f "$macos_log"
else
  skip "not running on macOS — skipping local macOS install"
fi

# ── phase 4b: Hermes skills install ─────────────────────────────────
say "Phase 4b: Hermes skills install (--hermes)"

hermes_install() {
  _home="$1"
  shift
  _log=$(mktemp)
  if ! env HOME="$_home" KRAB_SKILLS_BASE_URL="file://${skill_tmp}" "$@" \
       sh "$INSTALL_SH" --silent --hermes >"$_log" 2>&1; then
    cat "$_log"
    rm -f "$_log"
    die "install.sh --hermes FAILED"
  fi
  rm -f "$_log"
}

assert_hermes_skills() {
  _category="$1"
  for f in krabarena/SKILL.md krabarena-heartbeat/SKILL.md \
           krabarena/references/claim-structure.md \
           krabarena-heartbeat/references/battle-structure.md; do
    [ -f "${_category}/${f}" ] || die "Hermes skill file missing: ${_category}/${f}"
  done
}

host_glibc=true
if [ "$(uname -s)" = Linux ] && ! getconf GNU_LIBC_VERSION >/dev/null 2>&1; then
  host_glibc=false
fi

if [ "$host_glibc" = true ]; then
  hermes_home=$(mktemp -d)
  mkdir -p "$hermes_home/.hermes/skills/krabarena" "$hermes_home/.hermes/skills/krabarena-heartbeat"
  printf -- '---\nname: krabarena\n---\n' > "$hermes_home/.hermes/skills/krabarena/SKILL.md"
  printf -- '---\nname: krabarena-heartbeat\n---\n' > "$hermes_home/.hermes/skills/krabarena-heartbeat/SKILL.md"
  hermes_install "$hermes_home"
  assert_hermes_skills "$hermes_home/.hermes/skills/krabarena"
  [ ! -f "$hermes_home/.hermes/skills/krabarena/SKILL.md" ] \
    || die "legacy flat ~/.hermes/skills/krabarena/SKILL.md was not migrated"
  [ ! -d "$hermes_home/.hermes/skills/krabarena-heartbeat" ] \
    || die "legacy ~/.hermes/skills/krabarena-heartbeat was not migrated"
  grep -q '^agent_backend = "Hermes Agent"' "$hermes_home/.krab/config.toml" \
    || die "install.sh --hermes did not set the Hermes Agent identity"
  ok "HOME/.hermes/skills/krabarena/{krabarena,krabarena-heartbeat} installed, legacy layout migrated"
  rm -rf "$hermes_home"

  hermes_home=$(mktemp -d)
  hermes_install "$hermes_home" HERMES_HOME="$hermes_home/custom-hermes"
  assert_hermes_skills "$hermes_home/custom-hermes/skills/krabarena"
  [ ! -d "$hermes_home/.hermes" ] || die "HERMES_HOME was ignored"
  ok "HERMES_HOME respected"
  rm -rf "$hermes_home"

  hermes_home=$(mktemp -d)
  hermes_project=$(mktemp -d)
  ( cd "$hermes_project" \
      && env HOME="$hermes_home" KRAB_SKILLS_BASE_URL="file://${skill_tmp}" \
           sh "$INSTALL_SH" --silent --hermes --skills-scope project >/dev/null 2>&1 ) \
    || die "install.sh --hermes --skills-scope project FAILED"
  assert_hermes_skills "$hermes_project/.hermes/skills/krabarena"
  ok "--skills-scope project installs into ./.hermes/skills/krabarena"
  rm -rf "$hermes_home" "$hermes_project"
else
  skip "host is not glibc — skipping Hermes install"
fi

# ── phase 5: container install ───────────────────────────────────────
say "Phase 5: container install (Ubuntu)"

if ! command -v docker >/dev/null 2>&1; then
  skip "docker CLI not installed — skipping container phase"
  exit 0
fi
if ! docker info >/dev/null 2>&1; then
  skip "docker daemon not reachable — skipping container phase"
  exit 0
fi

run_install_success_container() {
  image="$1"
  setup="$2"
  label="$3"

  printf '  \033[1;34m▸\033[0m %s\n' "$label"

  # Mount the install.sh read-only, set HOME=/tmp/krabhome so nothing
  # leaks into the developer's actual home (important on macOS where
  # docker mounts the user's home by default), and run the script with
  # --silent --skills-scope none so we don't block on the 5-minute auth poll
  # or try to fetch skill tarballs.
  if docker run --rm \
       -v "${INSTALL_SH}:/install.sh:ro" \
       -e HOME=/tmp/krabhome \
       --entrypoint /bin/sh \
       "$image" -c "
         set -e
         $setup
         mkdir -p /tmp/krabhome
         sh /install.sh --silent --skills-scope none
         test -x /tmp/krabhome/.krab/bin/krab || { echo 'binary missing'; exit 1; }
         grep -q 'api_url' /tmp/krabhome/.krab/config.toml || { echo 'config missing api_url'; exit 1; }
         /tmp/krabhome/.krab/bin/krab --version
       " >/tmp/krab-install-test.log 2>&1; then
    version=$(grep -E '^krab ' /tmp/krab-install-test.log | head -1)
    ok "$label install OK — ${version:-(version line not captured)}"
  else
    echo
    echo "--- container output ---"
    cat /tmp/krab-install-test.log
    echo "------------------------"
    echo
    die "$label install FAILED"
  fi
}

run_expected_failure_container() {
  image="$1"
  setup="$2"
  label="$3"
  expected="$4"

  printf '  \033[1;34m▸\033[0m %s\n' "$label"

  if docker run --rm \
       -v "${INSTALL_SH}:/install.sh:ro" \
       -e HOME=/tmp/krabhome \
       --entrypoint /bin/sh \
       "$image" -c "
         set -e
         $setup
         mkdir -p /tmp/krabhome
         sh /install.sh --silent --skills-scope none
       " >/tmp/krab-install-test.log 2>&1; then
    echo
    echo "--- container output ---"
    cat /tmp/krab-install-test.log
    echo "------------------------"
    echo
    die "$label unexpectedly succeeded"
  fi

  if grep -qi "$expected" /tmp/krab-install-test.log; then
    ok "$label failed with expected message"
  else
    echo
    echo "--- container output ---"
    cat /tmp/krab-install-test.log
    echo "------------------------"
    echo
    die "$label failed without expected message: $expected"
  fi
}

run_install_success_container \
  "ubuntu:24.04" \
  "apt-get update -qq >/dev/null && apt-get install -y -qq curl ca-certificates >/dev/null" \
  "ubuntu:24.04 (bash / glibc)"

say "Phase 6: unsupported Linux libc rejection (Alpine/musl)"

run_expected_failure_container \
  "alpine:latest" \
  ":" \
  "alpine:latest (BusyBox / musl)" \
  "require glibc"

say "all phases passed"
