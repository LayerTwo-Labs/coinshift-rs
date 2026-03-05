#!/bin/bash
#
# Mine Blocks Script
# 
# Mines blocks on mainchain and/or parentchain regtest networks
# Usage:
#   ./4_mine_blocks.sh [mainchain|parentchain|both] [count]
#   Default: mines 1 block on both networks
#

# Exit on error
set -e

export BITCOIN_DIR="/home/parallels/Projects/bitcoin-patched/build/bin"
export BITCOIN_CLI="$BITCOIN_DIR/bitcoin-cli"
export RPC_USER="user"
export RPC_PASSWORD="passwordDC"

# Mainchain regtest
export MAINCHAIN_RPC_PORT="18443"
export MAINCHAIN_DATADIR="/home/parallels/Projects/coinshift-mainchain-data"
export MAINCHAIN_WALLET="mainchainwallet"

# Parentchain regtest
export PARENTCHAIN_RPC_PORT="18444"
export PARENTCHAIN_DATADIR="/home/parallels/Projects/coinshift-parentchain-data"
export PARENTCHAIN_WALLET="parentchainwallet"

# Parse arguments
TARGET="${1:-both}"
COUNT="${2:-1}"

# Function to check if a node is running
check_node_running() {
    local port=$1
    local datadir=$2
    local name=$3
    
    if "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$port" -datadir="$datadir" getblockchaininfo >/dev/null 2>&1; then
        return 0
    else
        echo "ERROR: $name node is not running on RPC port $port"
        return 1
    fi
}

# Function to mine blocks on a network
mine_blocks() {
    local port=$1
    local datadir=$2
    local wallet=$3
    local count=$4
    local name=$5
    
    echo "Mining $count block(s) on $name..."
    
    # Get an address from the wallet
    ADDR=$("$BITCOIN_CLI" -regtest -rpcwait \
      -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$port" \
      -datadir="$datadir" \
      -rpcwallet="$wallet" \
      getnewaddress 2>/dev/null || echo "")
    
    if [ -z "$ADDR" ]; then
        echo "  WARNING: Could not get address from wallet, trying without wallet..."
        ADDR=$("$BITCOIN_CLI" -regtest -rpcwait \
          -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$port" \
          -datadir="$datadir" \
          getnewaddress 2>/dev/null || echo "")
    fi
    
    if [ -z "$ADDR" ]; then
        echo "  ERROR: Could not get address. Make sure the node is running and wallet exists."
        return 1
    fi
    
    # Mine blocks
    RESULT=$("$BITCOIN_CLI" -regtest -rpcwait \
      -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$port" \
      -datadir="$datadir" \
      generatetoaddress "$count" "$ADDR" 2>&1)
    
    if [ $? -eq 0 ]; then
        echo "  ✓ Successfully mined $count block(s) on $name"
        # Get current block height
        HEIGHT=$("$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$port" -datadir="$datadir" getblockcount 2>/dev/null || echo "unknown")
        echo "  Current $name block height: $HEIGHT"
        return 0
    else
        echo "  ✗ Failed to mine blocks on $name: $RESULT"
        return 1
    fi
}

echo "=========================================="
echo "Mine Blocks Script"
echo "=========================================="
echo "Target: $TARGET"
echo "Block count: $COUNT"
echo ""

# Mine on mainchain
if [ "$TARGET" = "mainchain" ] || [ "$TARGET" = "both" ]; then
    if check_node_running "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR" "Mainchain"; then
        mine_blocks "$MAINCHAIN_RPC_PORT" "$MAINCHAIN_DATADIR" "$MAINCHAIN_WALLET" "$COUNT" "mainchain"
        echo ""
    else
        echo "Skipping mainchain (node not running)"
        echo ""
    fi
fi

# Mine on parentchain
if [ "$TARGET" = "parentchain" ] || [ "$TARGET" = "both" ]; then
    if check_node_running "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR" "Parentchain"; then
        mine_blocks "$PARENTCHAIN_RPC_PORT" "$PARENTCHAIN_DATADIR" "$PARENTCHAIN_WALLET" "$COUNT" "parentchain"
        echo ""
    else
        echo "Skipping parentchain (node not running)"
        echo ""
    fi
fi

echo "=========================================="
echo "Mining complete!"
echo "=========================================="
echo ""
echo "Usage examples:"
echo "  ./4_mine_blocks.sh              # Mine 1 block on both networks"
echo "  ./4_mine_blocks.sh both 5       # Mine 5 blocks on both networks"
echo "  ./4_mine_blocks.sh mainchain 10 # Mine 10 blocks on mainchain only"
echo "  ./4_mine_blocks.sh parentchain 3 # Mine 3 blocks on parentchain only"
echo ""

