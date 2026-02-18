#!/bin/bash
#
# Open Coinshift GUI for one of the 3 users (1, 2, or 3).
# Usage: ./7_open_client.sh [1|2|3]
# Default: 1
#

set -e

USER_NUM="${1:-1}"
PROJECT_ROOT="${PROJECT_ROOT:-/home/parallels/Projects}"
COINSHIFT_REPO="${COINSHIFT_REPO:-$PROJECT_ROOT/coinshift-rs}"
if [ -x "$COINSHIFT_REPO/target/release/coinshift_app" ]; then
    COINSHIFT_BIN="${COINSHIFT_BIN:-$COINSHIFT_REPO/target/release/coinshift_app}"
else
    COINSHIFT_BIN="${COINSHIFT_BIN:-$COINSHIFT_REPO/target/debug/coinshift_app}"
fi
ENFORCER_GRPC_URL="${ENFORCER_GRPC_URL:-http://127.0.0.1:50051}"

case "$USER_NUM" in
    1) suffix="user1"; rpc_port=6255; net_port=4255 ;;
    2) suffix="user2"; rpc_port=6256; net_port=4256 ;;
    3) suffix="user3"; rpc_port=6257; net_port=4257 ;;
    *)
        echo "Usage: $0 [1|2|3]"
        echo "  User 1: RPC 6255, datadir coinshift-user1"
        echo "  User 2: RPC 6256, datadir coinshift-user2"
        echo "  User 3: RPC 6257, datadir coinshift-user3"
        exit 1
        ;;
esac

datadir="$PROJECT_ROOT/coinshift-$suffix"
mkdir -p "$datadir"

# Stop headless instance for this user (GUI runs its own RPC server)
pkill -f "coinshift_app.*coinshift-$suffix" 2>/dev/null || true
pkill -f "coinshift_app.*--datadir.*$datadir" 2>/dev/null || true
sleep 1

echo "Opening Coinshift GUI for User $USER_NUM (RPC $rpc_port, datadir $datadir)"
echo "In the GUI: L1 Config -> Regtest -> URL http://127.0.0.1:18444, user/password user/passwordDC"
echo ""

if [ -x "$COINSHIFT_BIN" ]; then
    exec "$COINSHIFT_BIN" \
        --datadir "$datadir" \
        --rpc-addr "127.0.0.1:$rpc_port" \
        --net-addr "0.0.0.0:$net_port" \
        --mainchain-grpc-url "$ENFORCER_GRPC_URL" \
        --network regtest
else
    exec env COINSHIFT_REPO="$COINSHIFT_REPO" bash -c "cd \"$COINSHIFT_REPO\" && cargo run --bin coinshift_app --release -- \
        --datadir \"$datadir\" \
        --rpc-addr 127.0.0.1:$rpc_port \
        --net-addr 0.0.0.0:$net_port \
        --mainchain-grpc-url \"$ENFORCER_GRPC_URL\" \
        --network regtest"
fi
