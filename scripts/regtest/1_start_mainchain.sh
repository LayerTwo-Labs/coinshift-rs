#!/bin/bash
#
# Mainchain Regtest Setup Script
# 
# Ports used:
#   RPC: 18443
#   P2P: 38333
#   ZMQ: 29000-29004
#
# Data directory: /home/parallels/Projects/coinshift-mainchain-data
#

# Exit on error
set -e

export BITCOIN_DIR="/home/parallels/Projects/bitcoin-patched/build/bin"
export BITCOIND="$BITCOIN_DIR/bitcoind"
export BITCOIN_CLI="$BITCOIN_DIR/bitcoin-cli"
export RPC_USER="user"
export RPC_PASSWORD="passwordDC"

# Mainchain regtest (for sidechain activation)
export MAINCHAIN_RPC_PORT="18443"
export MAINCHAIN_P2P_PORT="38333"
export MAINCHAIN_DATADIR="/home/parallels/Projects/coinshift-mainchain-data"
export MAINCHAIN_WALLET="mainchainwallet"

# ZMQ (for mainchain)
export ZMQ_SEQUENCE="tcp://127.0.0.1:29000"
export ZMQ_HASHBLOCK="tcp://127.0.0.1:29001"
export ZMQ_HASHTX="tcp://127.0.0.1:29002"
export ZMQ_RAWBLOCK="tcp://127.0.0.1:29003"
export ZMQ_RAWTX="tcp://127.0.0.1:29004"

echo "=========================================="
echo "Starting Mainchain Regtest"
echo "=========================================="


# Clean up data directory
echo "Cleaning up data directory..."
rm -rf "$MAINCHAIN_DATADIR"
mkdir -p "$MAINCHAIN_DATADIR"
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

if ! check_port "$MAINCHAIN_P2P_PORT" "Mainchain P2P"; then
    exit 1
fi

if ! check_port "$MAINCHAIN_RPC_PORT" "Mainchain RPC"; then
    exit 1
fi

# Start mainchain bitcoind
echo "Starting mainchain bitcoind..."
echo "  RPC Port: $MAINCHAIN_RPC_PORT"
echo "  P2P Port: $MAINCHAIN_P2P_PORT"
echo "  Data Dir: $MAINCHAIN_DATADIR"

if ! "$BITCOIND" -regtest \
  -fallbackfee=0.0002 \
  -rpcuser="$RPC_USER" \
  -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$MAINCHAIN_RPC_PORT" \
  -server -txindex -rest \
  -zmqpubsequence="$ZMQ_SEQUENCE" \
  -zmqpubhashblock="$ZMQ_HASHBLOCK" \
  -zmqpubhashtx="$ZMQ_HASHTX" \
  -zmqpubrawblock="$ZMQ_RAWBLOCK" \
  -zmqpubrawtx="$ZMQ_RAWTX" \
  -bind=0.0.0.0:"$MAINCHAIN_P2P_PORT" \
  -port="$MAINCHAIN_P2P_PORT" \
  -datadir="$MAINCHAIN_DATADIR" \
  -daemon; then
    echo "ERROR: Failed to start mainchain bitcoind"
    echo "Check the logs in $MAINCHAIN_DATADIR/debug.log"
    exit 1
fi

# Wait a moment for bitcoind to start
sleep 2

# Verify the port it's actually using
echo "Verifying mainchain bitcoind is using the correct ports..."
if command -v lsof >/dev/null 2>&1; then
    echo "  Checking port $MAINCHAIN_P2P_PORT (P2P):"
    lsof -Pi :$MAINCHAIN_P2P_PORT -sTCP:LISTEN 2>/dev/null | head -3 || echo "    Port $MAINCHAIN_P2P_PORT not in use (unexpected)"
    echo "  Checking port $MAINCHAIN_RPC_PORT (RPC):"
    lsof -Pi :$MAINCHAIN_RPC_PORT -sTCP:LISTEN 2>/dev/null | head -3 || echo "    Port $MAINCHAIN_RPC_PORT not in use (unexpected)"
fi

echo "Mainchain bitcoind started, waiting for it to initialize..."
sleep 3

# Wait for RPC to be ready
echo "Waiting for mainchain RPC to be ready..."
max_attempts=60
attempt=0
while [ $attempt -lt $max_attempts ]; do
    if "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" getblockchaininfo >/dev/null 2>&1; then
        echo "Mainchain RPC is ready!"
        break
    fi
    attempt=$((attempt + 1))
    if [ $((attempt % 5)) -eq 0 ]; then
        echo "  Still waiting... (attempt $attempt/$max_attempts)"
        # Check if process is still running
        if ! pgrep -f "bitcoind.*$MAINCHAIN_RPC_PORT" >/dev/null; then
            echo "ERROR: Mainchain bitcoind process died!"
            echo "Check the logs in $MAINCHAIN_DATADIR/debug.log"
            exit 1
        fi
    fi
    sleep 1
done

if [ $attempt -eq $max_attempts ]; then
    echo "ERROR: Mainchain RPC failed to start after $max_attempts attempts"
    echo "Check the logs in $MAINCHAIN_DATADIR/debug.log"
    echo "Trying to get error info..."
    "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" getblockchaininfo 2>&1 || true
    exit 1
fi

# Create wallet
echo "Creating mainchain wallet..."
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" \
  -datadir="$MAINCHAIN_DATADIR" \
  createwallet "$MAINCHAIN_WALLET" || true

# Get address and mine initial blocks
ADDR=$("$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" \
  -datadir="$MAINCHAIN_DATADIR" \
  -rpcwallet="$MAINCHAIN_WALLET" \
  getnewaddress)

echo "Mining 101 blocks on mainchain..."
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" \
  -datadir="$MAINCHAIN_DATADIR" \
  generatetoaddress 101 "$ADDR"

echo ""
echo "=========================================="
echo "Mainchain is ready!"
echo "=========================================="
echo "RPC Port: $MAINCHAIN_RPC_PORT"
echo "P2P Port: $MAINCHAIN_P2P_PORT"
echo "Data Dir: $MAINCHAIN_DATADIR"
echo ""
echo "You can now run: ./2_start_parentchain.sh"
echo ""

