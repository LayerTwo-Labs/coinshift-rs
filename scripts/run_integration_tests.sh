#!/usr/bin/env bash
#
# run_integration_tests.sh
#
# Local runner for the `integration-test` job from
# .github/workflows/check_lint_build.yaml, adapted for macOS.
#
# It downloads / builds everything the CI job needs:
#   * bitcoin-patched  (bitcoind, bitcoin-cli)  -- prebuilt
#   * bip300301-enforcer                         -- prebuilt
#   * electrs                                    -- cloned + built from source
#   * coinshift_app                              -- built from this repo
# then runs the integration test example with the right env vars.
#
# Notes for Apple Silicon:
#   bitcoin-patched ships ONLY an x86_64-apple-darwin build, so it runs under
#   Rosetta 2. The enforcer has a native arm64 build, which we prefer.
#
# Usage:
#   scripts/run_integration_tests.sh [options] [-- <extra args for test runner>]
#
# Options:
#   --deps-dir <path>    Where to put downloaded/built deps
#                        (default: <repo>/../coinshift-integration-deps)
#   --force-download     Re-download the prebuilt binaries even if present
#   --rebuild-electrs    git pull + rebuild electrs even if already built
#   --skip-build         Skip building coinshift_app (assume it's already built)
#   --tests <a,b,c>      Run only the named tests (forwarded to the runner)
#   -h, --help           Show this help
#
# Anything after `--` is passed straight through to the test runner, e.g.:
#   scripts/run_integration_tests.sh -- --tests swap_creation
#
set -euo pipefail

# ---------------------------------------------------------------------------
# Resolve paths
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DEPS_DIR="$(cd "$REPO_DIR/.." && pwd)/coinshift-integration-deps"

ELECTRS_REPO="https://github.com/mempool/electrs.git"
RELEASES_BASE="https://releases.drivechain.info"

FORCE_DOWNLOAD=0
REBUILD_ELECTRS=0
SKIP_BUILD=0
RUNNER_ARGS=()

# ---------------------------------------------------------------------------
# Pretty logging
# ---------------------------------------------------------------------------
if [[ -t 1 ]]; then
  C_BLUE=$'\033[1;34m'; C_GREEN=$'\033[1;32m'; C_YELLOW=$'\033[1;33m'
  C_RED=$'\033[1;31m'; C_RESET=$'\033[0m'
else
  C_BLUE=''; C_GREEN=''; C_YELLOW=''; C_RED=''; C_RESET=''
fi
step() { echo "${C_BLUE}==>${C_RESET} $*"; }
ok()   { echo "${C_GREEN}  ok${C_RESET} $*"; }
warn() { echo "${C_YELLOW}  warn${C_RESET} $*"; }
die()  { echo "${C_RED}error:${C_RESET} $*" >&2; exit 1; }

usage() { awk 'NR>1 && /^#/ {sub(/^# ?/,""); print; next} NR>1 {exit}' "${BASH_SOURCE[0]}"; }

# ---------------------------------------------------------------------------
# Parse args
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case "$1" in
    --deps-dir)        DEPS_DIR="$2"; shift 2 ;;
    --force-download)  FORCE_DOWNLOAD=1; shift ;;
    --rebuild-electrs) REBUILD_ELECTRS=1; shift ;;
    --skip-build)      SKIP_BUILD=1; shift ;;
    --tests)           RUNNER_ARGS+=(--tests "$2"); shift 2 ;;
    -h|--help)         usage; exit 0 ;;
    --)                shift; RUNNER_ARGS+=("$@"); break ;;
    *)                 die "unknown argument: $1 (use --help)" ;;
  esac
done

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------
[[ "$(uname -s)" == "Darwin" ]] || die "this script targets macOS; on Linux use the CI steps directly"

HOST_ARCH="$(uname -m)"   # arm64 or x86_64

# bitcoin-patched: only an x86_64-apple-darwin build is published.
BTC_TARGET="x86_64-apple-darwin"

# enforcer: prefer a native build matching the host.
case "$HOST_ARCH" in
  arm64)  ENF_TARGET="aarch64-apple-darwin" ;;
  x86_64) ENF_TARGET="x86_64-apple-darwin" ;;
  *)      die "unsupported arch: $HOST_ARCH" ;;
esac

# ---------------------------------------------------------------------------
# Preflight: required tools + Rosetta (needed to run x86_64 bitcoind on arm64)
# ---------------------------------------------------------------------------
step "Checking prerequisites"
for tool in curl unzip git cargo; do
  command -v "$tool" >/dev/null 2>&1 || die "missing required tool: $tool"
done
ok "curl, unzip, git, cargo present"

if [[ "$HOST_ARCH" == "arm64" ]]; then
  if /usr/bin/pgrep -q oahd && arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
    ok "Rosetta 2 available (needed for x86_64 bitcoind)"
  else
    die "Rosetta 2 is required to run the x86_64 bitcoind. Install it with:
       softwareupdate --install-rosetta --agree-to-license"
  fi
fi

mkdir -p "$DEPS_DIR"
step "Dependency dir: $DEPS_DIR"

# ---------------------------------------------------------------------------
# Helper: download + unzip a release artifact into DEPS_DIR
#   args: <zip-filename>
# ---------------------------------------------------------------------------
fetch_zip() {
  local zip="$1"
  step "Downloading $zip"
  curl -fL --retry 3 --retry-delay 2 -o "$DEPS_DIR/$zip" "$RELEASES_BASE/$zip"
  ( cd "$DEPS_DIR" && unzip -o -q "$zip" && rm -f "$zip" )
}

# ---------------------------------------------------------------------------
# 1. bitcoin-patched (bitcoind, bitcoin-cli)
# ---------------------------------------------------------------------------
BTC_BIN_DIR="$DEPS_DIR/bitcoin-patched-bins"
if [[ $FORCE_DOWNLOAD -eq 1 || ! -x "$BTC_BIN_DIR/bitcoind" ]]; then
  rm -rf "$BTC_BIN_DIR" "$DEPS_DIR/L1-bitcoin-patched-latest-$BTC_TARGET"
  fetch_zip "L1-bitcoin-patched-latest-$BTC_TARGET.zip"
  mv "$DEPS_DIR/L1-bitcoin-patched-latest-$BTC_TARGET" "$BTC_BIN_DIR"
  chmod +x "$BTC_BIN_DIR/bitcoind" "$BTC_BIN_DIR/bitcoin-cli"
  ok "bitcoind + bitcoin-cli ready ($BTC_TARGET)"
else
  ok "bitcoin-patched already present (use --force-download to refresh)"
fi

# ---------------------------------------------------------------------------
# 2. bip300301-enforcer (built from the rev this workspace pins)
#
# NOT the published "latest" build. The integration tests drive the enforcer
# *binary* using the harness from `bip300301_enforcer_integration_tests`, which
# is pinned in Cargo.toml, and the two have to agree on the CLI. They already
# don't: the pinned rev takes `--serve-json-rpc-addr`, while latest renamed it
# to `--serve-rpc-addr`, so a downloaded binary fails at startup with
# "unexpected argument". Building the pinned rev keeps them in lockstep.
# ---------------------------------------------------------------------------
ENFORCER_REPO="https://github.com/LayerTwo-Labs/bip300301_enforcer"
ENFORCER_REV="$(awk -F'"' '
  /^\[workspace\.dependencies\.bip300301_enforcer_lib\]/ { f = 1 }
  f && /^rev[[:space:]]*=/ { print $2; exit }
' "$REPO_DIR/Cargo.toml")"
[[ -n "$ENFORCER_REV" ]] || die "could not read the pinned enforcer rev from Cargo.toml"

ENF_SRC="$DEPS_DIR/enforcer-src"
ENF_BIN="$ENF_SRC/target/release/bip300301_enforcer"
if [[ ! -d "$ENF_SRC/.git" ]]; then
  step "Cloning bip300301-enforcer"
  git clone "$ENFORCER_REPO" "$ENF_SRC"
fi
ENF_HAVE="$(cd "$ENF_SRC" && git rev-parse HEAD 2>/dev/null || echo none)"
if [[ "$ENF_HAVE" != "$ENFORCER_REV" || ! -x "$ENF_BIN" ]]; then
  step "Building bip300301-enforcer at ${ENFORCER_REV:0:12} — this can take a few minutes"
  ( cd "$ENF_SRC" && git fetch --quiet origin "$ENFORCER_REV" 2>/dev/null || true )
  ( cd "$ENF_SRC" && git checkout --quiet "$ENFORCER_REV" )
  ( cd "$ENF_SRC" && cargo build --release --bin bip300301_enforcer )
  ok "bip300301-enforcer built at ${ENFORCER_REV:0:12}"
else
  ok "bip300301-enforcer already built at ${ENFORCER_REV:0:12}"
fi
[[ -x "$ENF_BIN" ]] || die "enforcer binary not found at $ENF_BIN"

# Clear Gatekeeper quarantine on downloaded binaries (defensive; curl usually
# doesn't set it, but unsigned binaries can otherwise be blocked).
xattr -dr com.apple.quarantine "$BTC_BIN_DIR" "$ENF_BIN" 2>/dev/null || true

# ---------------------------------------------------------------------------
# 3. electrs (clone + build from source)
# ---------------------------------------------------------------------------
ELECTRS_DIR="$DEPS_DIR/electrs"
ELECTRS_BIN="$ELECTRS_DIR/target/release/electrs"
if [[ ! -d "$ELECTRS_DIR/.git" ]]; then
  step "Cloning electrs"
  git clone "$ELECTRS_REPO" "$ELECTRS_DIR"
fi
if [[ $REBUILD_ELECTRS -eq 1 || ! -x "$ELECTRS_BIN" ]]; then
  step "Building electrs (release) — this can take a few minutes"
  ( cd "$ELECTRS_DIR" && [[ $REBUILD_ELECTRS -eq 1 ]] && git pull --ff-only || true )
  ( cd "$ELECTRS_DIR" && cargo build --locked --release )
  ok "electrs built"
else
  ok "electrs already built (use --rebuild-electrs to refresh)"
fi
[[ -x "$ELECTRS_BIN" ]] || die "electrs binary not found at $ELECTRS_BIN"

# ---------------------------------------------------------------------------
# 4. Build coinshift_app
# ---------------------------------------------------------------------------
COINSHIFT_APP="$REPO_DIR/target/debug/coinshift_app"
if [[ $SKIP_BUILD -eq 0 ]]; then
  step "Building coinshift_app (debug)"
  ( cd "$REPO_DIR" && cargo build -p coinshift_app )
  ok "coinshift_app built"
fi
[[ -x "$COINSHIFT_APP" ]] || die "coinshift_app not found at $COINSHIFT_APP (drop --skip-build?)"

# ---------------------------------------------------------------------------
# 5. Run the integration tests
# ---------------------------------------------------------------------------
export BIP300301_ENFORCER="$ENF_BIN"
export BITCOIND="$BTC_BIN_DIR/bitcoind"
export BITCOIN_CLI="$BTC_BIN_DIR/bitcoin-cli"
export ELECTRS="$ELECTRS_BIN"
export COINSHIFT_APP="$COINSHIFT_APP"

step "Environment"
echo "  BIP300301_ENFORCER = $BIP300301_ENFORCER"
echo "  BITCOIND           = $BITCOIND"
echo "  BITCOIN_CLI        = $BITCOIN_CLI"
echo "  ELECTRS            = $ELECTRS"
echo "  COINSHIFT_APP      = $COINSHIFT_APP"

step "Running integration tests"
cd "$REPO_DIR"

# The test harness renders tracing-indicatif progress bars and PANICS if stdout
# is not a TTY (e.g. when piped or redirected to a file). When we detect that
# we're not on a terminal, run under `script` to allocate a pseudo-tty so the
# UI is happy. macOS `script` propagates the child's exit code.
#
# Note: ${arr[@]+"${arr[@]}"} guards against "unbound variable" under `set -u`
# when the array is empty (macOS ships bash 3.2, which treats it as unset).
if [[ -t 1 ]]; then
  exec cargo run -p coinshift_integration_tests --example integration_tests -- \
    ${RUNNER_ARGS[@]+"${RUNNER_ARGS[@]}"}
else
  warn "stdout is not a TTY — running under a pseudo-tty (the progress-bar UI requires one)"
  exec script -q /dev/null \
    cargo run -p coinshift_integration_tests --example integration_tests -- \
    ${RUNNER_ARGS[@]+"${RUNNER_ARGS[@]}"}
fi
