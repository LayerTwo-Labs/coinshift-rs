#!/bin/bash
#
# Generate Addresses Script
#
# Generates new receive addresses from mainchain and/or parentchain wallets.
# Always shows current getblockchaininfo for both chains.
#
# Usage:
#   ./generate_addresses.sh [mainchain|parentchain|both]
#
# Arguments:
#   mainchain   Generate address from mainchain only (default)
#   parentchain Generate address from parentchain only
#   both        Generate address from both chains
#
# Examples:
#   ./generate_addresses.sh              # Mainchain only
#   ./generate_addresses.sh parentchain  # Parentchain only
#   ./generate_addresses.sh both        # Both chains
#

set -e

TARGET="${1:-mainchain}"

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

generate_address() {
    local name="$1"
    local port="$2"
    local datadir="$3"
    local wallet="$4"
    echo "--- $name: getnewaddress ---"
    local addr
    if addr=$("$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$port" -datadir="$datadir" -rpcwallet="$wallet" getnewaddress 2>&1); then
        echo "$name address: $addr"
    else
        echo "Failed to get $name address: $addr"
    fi
    echo ""
}

# Show current configuration (getblockchaininfo) for both chains first
echo "=========================================="
echo "Current chain configuration"
echo "=========================================="
echo ""
show_blockchain_info "Mainchain" "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR"
show_blockchain_info "Parentchain" "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR"

# Validate target
case "$TARGET" in
    mainchain)
        generate_address "Mainchain" "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR" "$MAINCHAIN_WALLET"
        ;;
    parentchain)
        generate_address "Parentchain" "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR" "$PARENTCHAIN_WALLET"
        ;;
    both)
        generate_address "Mainchain" "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR" "$MAINCHAIN_WALLET"
        generate_address "Parentchain" "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR" "$PARENTCHAIN_WALLET"
        ;;
    *)
        echo "Usage: $0 [mainchain|parentchain|both]"
        echo "  mainchain   - Generate address from mainchain only (default)"
        echo "  parentchain - Generate address from parentchain only"
        echo "  both        - Generate address from both chains"
        exit 1
        ;;
esac

echo "Done."
