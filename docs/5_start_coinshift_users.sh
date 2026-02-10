#!/bin/bash
#
# Start 3 Coinshift instances (User1, User2, User3).
# Each has its own datadir, RPC port, and net port.
# Requires: mainchain + enforcer running (run 1_start_mainchain.sh and 3_start_enforcer.sh first).
#

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="${PROJECT_ROOT:-/home/parallels/Projects}"
COINSHIFT_REPO="${COINSHIFT_REPO:-$PROJECT_ROOT/coinshift-rs}"
# Prefer release build, fallback to debug
if [ -x "$COINSHIFT_REPO/target/release/coinshift_app" ]; then
    COINSHIFT_BIN="${COINSHIFT_BIN:-$COINSHIFT_REPO/target/release/coinshift_app}"
else
    COINSHIFT_BIN="${COINSHIFT_BIN:-$COINSHIFT_REPO/target/debug/coinshift_app}"
fi
ENFORCER_GRPC_URL="${ENFORCER_GRPC_URL:-http://127.0.0.1:50051}"

# User config: datadir suffix, RPC port, net port
declare -a USERS=("user1:6255:4255" "user2:6256:4256" "user3:6257:4257")

# Check enforcer is reachable
if command -v grpcurl >/dev/null 2>&1; then
    if ! grpcurl -plaintext "${ENFORCER_GRPC_URL#http://}" list >/dev/null 2>&1; then
        echo "WARNING: Enforcer gRPC not reachable at $ENFORCER_GRPC_URL. Start mainchain and run ./3_start_enforcer.sh first."
    fi
fi

# Build coinshift command (no exec; we run with nohup)
CARGO_RELEASE=""
[ -x "$COINSHIFT_REPO/target/release/coinshift_app" ] && CARGO_RELEASE="--release"
build_cmd() {
    local datadir="$1"
    local rpc_port="$2"
    local net_port="$3"
    if [ -x "$COINSHIFT_BIN" ]; then
        echo "$COINSHIFT_BIN --datadir $datadir --rpc-addr 127.0.0.1:$rpc_port --net-addr 0.0.0.0:$net_port --mainchain-grpc-url $ENFORCER_GRPC_URL --network regtest --headless"
    else
        echo "cd $COINSHIFT_REPO && cargo run --bin coinshift_app $CARGO_RELEASE -- --datadir $datadir --rpc-addr 127.0.0.1:$rpc_port --net-addr 0.0.0.0:$net_port --mainchain-grpc-url $ENFORCER_GRPC_URL --network regtest --headless"
    fi
}

echo "=========================================="
echo "Starting 3 Coinshift users"
echo "=========================================="

for entry in "${USERS[@]}"; do
    IFS=: read -r suffix rpc_port net_port <<< "$entry"
    datadir="$PROJECT_ROOT/coinshift-$suffix"
    mkdir -p "$datadir"
    logfile="$datadir/coinshift.log"
    echo "Starting Coinshift $suffix (RPC $rpc_port, net $net_port)..."
    cmd=$(build_cmd "$datadir" "$rpc_port" "$net_port")
    nohup bash -c "$cmd" >> "$logfile" 2>&1 &
    sleep 2
done

echo ""
echo "All 3 Coinshift instances started in background."
echo "  User1: RPC http://127.0.0.1:6255  datadir $PROJECT_ROOT/coinshift-user1"
echo "  User2: RPC http://127.0.0.1:6256  datadir $PROJECT_ROOT/coinshift-user2"
echo "  User3: RPC http://127.0.0.1:6257  datadir $PROJECT_ROOT/coinshift-user3"
echo ""
echo "Next: ./6_deposit_all.sh to deposit to all 3, then ./7_open_client.sh [1|2|3] to open the GUI."
echo ""
