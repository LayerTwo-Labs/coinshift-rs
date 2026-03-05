#!/bin/bash
#
# Get Transactions From Address Script
#
# Shows UTXOs and (if address is in wallet) received history for an address
# on mainchain or parentchain. Always shows getblockchaininfo for both chains.
#
# Usage:
#   ./get_txs_from_address.sh [mainchain|parentchain] <address>
#
# Examples:
#   ./get_txs_from_address.sh mainchain bcrt1q9z447588v4ua9nna7ff83zqfrcqlj8xklf4nl5
#   ./get_txs_from_address.sh parentchain bcrt1q...
#

set -e

CHAIN="${1:-}"
ADDRESS="${2:-}"

# If first arg looks like an address (starts with bcrt1, bc1, tb1, etc.), use it as address and default chain
if [ -n "$CHAIN" ] && [[ "$CHAIN" =~ ^(bcrt1|bc1|tb1|2|3|m|n) ]]; then
    ADDRESS="$CHAIN"
    CHAIN="mainchain"
fi

# Bitcoin Core settings (match 1_start_mainchain.sh and 2_start_parentchain.sh)
export BITCOIN_DIR="/home/parallels/Projects/bitcoin-patched/build/bin"
export BITCOIN_CLI="$BITCOIN_DIR/bitcoin-cli"
export RPC_USER="user"
export RPC_PASSWORD="passwordDC"

export MAINCHAIN_RPC_PORT="18443"
export MAINCHAIN_DATADIR="/home/parallels/Projects/coinshift-mainchain-data"
export MAINCHAIN_WALLET="mainchainwallet"

export PARENTCHAIN_RPC_PORT="18444"
export PARENTCHAIN_DATADIR="/home/parallels/Projects/coinshift-parentchain-data"
export PARENTCHAIN_WALLET="parentchainwallet"

show_blockchain_info() {
    local name="$1"
    local port="$2"
    local datadir="$3"
    echo "-------------------------------------------"
    echo "=== $name — getblockchaininfo ==="
    echo "-------------------------------------------"
    if "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$port" -datadir="$datadir" getblockchaininfo 2>&1; then
        :
    else
        echo "(node not running or RPC failed)"
    fi
    echo ""
}

cli() {
    local port="$1"
    local datadir="$2"
    shift 2
    "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$port" -datadir="$datadir" "$@" 2>&1
}

# Show current configuration for both chains
echo "=========================================="
echo "Current chain configuration"
echo "=========================================="
echo ""
show_blockchain_info "Mainchain" "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR"
show_blockchain_info "Parentchain" "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR"

if [ -z "$ADDRESS" ]; then
    echo "Usage: $0 [mainchain|parentchain] <address>"
    echo ""
    echo "  address  Bitcoin address (e.g. bcrt1q...)"
    echo "  chain    mainchain (default) or parentchain"
    echo ""
    echo "Examples:"
    echo "  $0 mainchain bcrt1q9z447588v4ua9nna7ff83zqfrcqlj8xklf4nl5"
    echo "  $0 parentchain bcrt1q..."
    echo "  $0 bcrt1q9z447588v4ua9nna7ff83zqfrcqlj8xklf4nl5   # mainchain by default"
    exit 1
fi

[ -z "$CHAIN" ] && CHAIN="mainchain"
if [ "$CHAIN" != "mainchain" ] && [ "$CHAIN" != "parentchain" ]; then
    echo "ERROR: chain must be 'mainchain' or 'parentchain'"
    exit 1
fi

if [ "$CHAIN" = "mainchain" ]; then
    RPC_PORT="$MAINCHAIN_RPC_PORT"
    DATADIR="$MAINCHAIN_DATADIR"
    WALLET="$MAINCHAIN_WALLET"
else
    RPC_PORT="$PARENTCHAIN_RPC_PORT"
    DATADIR="$PARENTCHAIN_DATADIR"
    WALLET="$PARENTCHAIN_WALLET"
fi

if ! "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$RPC_PORT" -datadir="$DATADIR" getblockchaininfo >/dev/null 2>&1; then
    echo "ERROR: $CHAIN node is not running (rpcport=$RPC_PORT). Start it first."
    exit 1
fi

echo "=========================================="
echo "Address: $ADDRESS (on $CHAIN)"
echo "=========================================="
echo ""

# 1) UTXOs at this address (scantxoutset – works for any address)
echo "--- Unspent outputs (scantxoutset) ---"
SCAN_RESULT=$(cli "$RPC_PORT" "$DATADIR" scantxoutset "start" "[\"addr($ADDRESS)\"]" 2>&1) || true
if echo "$SCAN_RESULT" | grep -q '"unspents"'; then
    if command -v jq >/dev/null 2>&1; then
        echo "$SCAN_RESULT" | jq '.'
        TOTAL=$(echo "$SCAN_RESULT" | jq -r '.total_amount // 0')
        echo ""
        echo "Total unspent: $TOTAL BTC"
    else
        echo "$SCAN_RESULT"
    fi
else
    echo "$SCAN_RESULT"
fi
echo ""

# 2) Received-by-address (only if address is in wallet)
echo "--- Received by address (wallet; if address is in $WALLET) ---"
RECV=$(cli "$RPC_PORT" "$DATADIR" -rpcwallet="$WALLET" listreceivedbyaddress 0 true true "$ADDRESS" 2>&1) || true
if echo "$RECV" | grep -q 'txids\|address'; then
    if command -v jq >/dev/null 2>&1; then
        echo "$RECV" | jq '.'
    else
        echo "$RECV"
    fi
else
    echo "$RECV"
    echo "(Address not in wallet or no received transactions)"
fi
echo ""

echo "Done."
