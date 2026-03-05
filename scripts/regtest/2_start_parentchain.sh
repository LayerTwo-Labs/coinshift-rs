#!/bin/bash
#
# Parentchain Regtest Setup Script
# 
# Ports used:
#   RPC: 18444
#   P2P: 38334
#
# Data directory: /home/parallels/Projects/coinshift-parentchain-data
#
# NOTE: These ports are DIFFERENT from mainchain to avoid conflicts
#

# Exit on error
set -e

export BITCOIN_DIR="/home/parallels/Projects/bitcoin-patched/build/bin"
export BITCOIND="$BITCOIN_DIR/bitcoind"
export BITCOIN_CLI="$BITCOIN_DIR/bitcoin-cli"
export RPC_USER="user"
export RPC_PASSWORD="passwordDC"

# Parentchain regtest (for swap transactions)
export PARENTCHAIN_RPC_PORT="18444"
export PARENTCHAIN_P2P_PORT="38334"
export PARENTCHAIN_DATADIR="/home/parallels/Projects/coinshift-parentchain-data"
export PARENTCHAIN_WALLET="parentchainwallet"

echo "=========================================="
echo "Starting Parentchain Regtest"
echo "=========================================="

# Clean up data directory
echo "Cleaning up data directory..."
rm -rf "$PARENTCHAIN_DATADIR"
mkdir -p "$PARENTCHAIN_DATADIR"
sleep 1

# Verify ports are free before starting
check_port() {
    local port=$1
    local name=$2
    if command -v lsof >/dev/null 2>&1; then
        if lsof -Pi :$port -sTCP:LISTEN -t >/dev/null 2>&1; then
            echo "ERROR: $name port $port is still in use!"
            echo "Please manually kill the process using: lsof -ti :$port | xargs kill -9"
            return 1
        fi
    elif command -v ss >/dev/null 2>&1; then
        if ss -lptn "sport = :$port" 2>/dev/null | grep -q LISTEN; then
            echo "ERROR: $name port $port is still in use!"
            echo "Please manually kill the process using: ss -lptn 'sport = :$port'"
            return 1
        fi
    fi
    return 0
}

if ! check_port "$PARENTCHAIN_P2P_PORT" "Parentchain P2P"; then
    exit 1
fi

if ! check_port "$PARENTCHAIN_RPC_PORT" "Parentchain RPC"; then
    exit 1
fi

# Start parentchain bitcoind
echo "Starting parentchain bitcoind..."
echo "  RPC Port: $PARENTCHAIN_RPC_PORT"
echo "  P2P Port: $PARENTCHAIN_P2P_PORT"
echo "  Data Dir: $PARENTCHAIN_DATADIR"

if ! "$BITCOIND" -regtest \
  -fallbackfee=0.0002 \
  -rpcuser="$RPC_USER" \
  -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$PARENTCHAIN_RPC_PORT" \
  -server -txindex -rest \
  -bind=0.0.0.0:"$PARENTCHAIN_P2P_PORT" \
  -port="$PARENTCHAIN_P2P_PORT" \
  -datadir="$PARENTCHAIN_DATADIR" \
  -daemon; then
    echo "ERROR: Failed to start parentchain bitcoind"
    echo "Check the logs in $PARENTCHAIN_DATADIR/debug.log"
    exit 1
fi

# Wait a moment for bitcoind to start
sleep 2

# Verify the port it's actually using
echo "Verifying parentchain bitcoind is using the correct ports..."
if command -v lsof >/dev/null 2>&1; then
    echo "  Checking port $PARENTCHAIN_P2P_PORT (P2P):"
    lsof -Pi :$PARENTCHAIN_P2P_PORT -sTCP:LISTEN 2>/dev/null | head -3 || echo "    Port $PARENTCHAIN_P2P_PORT not in use (unexpected)"
    echo "  Checking port $PARENTCHAIN_RPC_PORT (RPC):"
    lsof -Pi :$PARENTCHAIN_RPC_PORT -sTCP:LISTEN 2>/dev/null | head -3 || echo "    Port $PARENTCHAIN_RPC_PORT not in use (unexpected)"
fi

echo "Parentchain bitcoind started, waiting for it to initialize..."
sleep 3

# Wait for RPC to be ready
echo "Waiting for parentchain RPC to be ready..."
max_attempts=60
attempt=0
while [ $attempt -lt $max_attempts ]; do
    if "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$PARENTCHAIN_RPC_PORT" -datadir="$PARENTCHAIN_DATADIR" getblockchaininfo >/dev/null 2>&1; then
        echo "Parentchain RPC is ready!"
        break
    fi
    attempt=$((attempt + 1))
    if [ $((attempt % 5)) -eq 0 ]; then
        echo "  Still waiting... (attempt $attempt/$max_attempts)"
        # Check if process is still running
        if ! pgrep -f "bitcoind.*$PARENTCHAIN_RPC_PORT" >/dev/null; then
            echo "ERROR: Parentchain bitcoind process died!"
            echo "Check the logs in $PARENTCHAIN_DATADIR/debug.log"
            exit 1
        fi
    fi
    sleep 1
done

if [ $attempt -eq $max_attempts ]; then
    echo "ERROR: Parentchain RPC failed to start after $max_attempts attempts"
    echo "Check the logs in $PARENTCHAIN_DATADIR/debug.log"
    echo "Trying to get error info..."
    "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$PARENTCHAIN_RPC_PORT" -datadir="$PARENTCHAIN_DATADIR" getblockchaininfo 2>&1 || true
    exit 1
fi

# Create wallet
echo "Creating parentchain wallet..."
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$PARENTCHAIN_RPC_PORT" \
  -datadir="$PARENTCHAIN_DATADIR" \
  createwallet "$PARENTCHAIN_WALLET" || true

# Get address and mine initial blocks
ADDR=$("$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$PARENTCHAIN_RPC_PORT" \
  -datadir="$PARENTCHAIN_DATADIR" \
  -rpcwallet="$PARENTCHAIN_WALLET" \
  getnewaddress)

echo "Mining 101 blocks on parentchain..."
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$PARENTCHAIN_RPC_PORT" \
  -datadir="$PARENTCHAIN_DATADIR" \
  generatetoaddress 101 "$ADDR"

echo ""
echo "=========================================="
echo "Parentchain is ready!"
echo "=========================================="
echo "RPC Port: $PARENTCHAIN_RPC_PORT"
echo "P2P Port: $PARENTCHAIN_P2P_PORT"
echo "Data Dir: $PARENTCHAIN_DATADIR"
echo ""
echo "You can now run: ./3_start_enforcer.sh"
echo ""

