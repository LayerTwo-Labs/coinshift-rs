#!/bin/bash
#
# Send From Script
#
# Sends coins from mainchain or parentchain to a given address.
# Always shows getblockchaininfo for both chains before and after.
#
# Usage:
#   ./send_from.sh mainchain <address> <amount>
#   ./send_from.sh parentchain <address> <amount>
#
# Examples:
#   ./send_from.sh mainchain bcrt1q... 1.0
#   ./send_from.sh parentchain bcrt1q... 0.5
#

set -e

CHAIN="${1:-}"
ADDRESS="${2:-}"
AMOUNT="${3:-}"

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

# Show current configuration for both chains
echo "=========================================="
echo "Current chain configuration (before send)"
echo "=========================================="
echo ""
show_blockchain_info "Mainchain" "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR"
show_blockchain_info "Parentchain" "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR"

if [ -z "$CHAIN" ] || [ -z "$ADDRESS" ] || [ -z "$AMOUNT" ]; then
    echo "Usage: $0 mainchain <address> <amount>"
    echo "       $0 parentchain <address> <amount>"
    echo ""
    echo "Examples:"
    echo "  $0 mainchain bcrt1q... 1.0"
    echo "  $0 parentchain bcrt1q... 0.5"
    exit 1
fi

if [ "$CHAIN" != "mainchain" ] && [ "$CHAIN" != "parentchain" ]; then
    echo "ERROR: chain must be 'mainchain' or 'parentchain'"
    exit 1
fi

if [ "$CHAIN" = "mainchain" ]; then
    SRC_PORT="$MAINCHAIN_RPC_PORT"
    SRC_DATADIR="$MAINCHAIN_DATADIR"
    SRC_WALLET="$MAINCHAIN_WALLET"
else
    SRC_PORT="$PARENTCHAIN_RPC_PORT"
    SRC_DATADIR="$PARENTCHAIN_DATADIR"
    SRC_WALLET="$PARENTCHAIN_WALLET"
fi

if ! "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$SRC_PORT" -datadir="$SRC_DATADIR" getblockchaininfo >/dev/null 2>&1; then
    echo "ERROR: $CHAIN node is not running (rpcport=$SRC_PORT). Start it first."
    exit 1
fi

echo "Sending $AMOUNT BTC from $CHAIN to $ADDRESS"
TXID=$("$BITCOIN_CLI" -regtest \
    -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$SRC_PORT" -datadir="$SRC_DATADIR" \
    -rpcwallet="$SRC_WALLET" \
    sendtoaddress "$ADDRESS" "$AMOUNT" 2>&1) || true

if [[ "$TXID" == *"error"* ]] || [ -z "$TXID" ] || [ ${#TXID} -lt 32 ]; then
    echo "ERROR: Send failed: $TXID"
    exit 1
fi

echo "✓ Sent $AMOUNT BTC. Txid: $TXID"
echo ""

# Mine one block to confirm
MINER_ADDR=$("$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$SRC_PORT" -datadir="$SRC_DATADIR" -rpcwallet="$SRC_WALLET" \
    getnewaddress 2>/dev/null)
if [ -n "$MINER_ADDR" ]; then
    "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$SRC_PORT" -datadir="$SRC_DATADIR" \
        generatetoaddress 1 "$MINER_ADDR" >/dev/null 2>&1 || true
fi

echo "=========================================="
echo "Current chain configuration (after send)"
echo "=========================================="
echo ""
show_blockchain_info "Mainchain" "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR"
show_blockchain_info "Parentchain" "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR"

echo "Done."
