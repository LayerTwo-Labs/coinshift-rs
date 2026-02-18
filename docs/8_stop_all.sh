#!/bin/bash
#
# Stop mainchain, parentchain, enforcer, and Coinshift user processes.
# Usage: ./8_stop_all.sh
#

PROJECT_ROOT="${PROJECT_ROOT:-/home/parallels/Projects}"
BITCOIN_CLI="${BITCOIN_CLI:-$PROJECT_ROOT/bitcoin-patched/build/bin/bitcoin-cli}"
RPC_USER="${RPC_USER:-user}"
RPC_PASSWORD="${RPC_PASSWORD:-passwordDC}"
MAINCHAIN_RPC_PORT="${MAINCHAIN_RPC_PORT:-18443}"
MAINCHAIN_DATADIR="${MAINCHAIN_DATADIR:-$PROJECT_ROOT/coinshift-mainchain-data}"
PARENTCHAIN_RPC_PORT="${PARENTCHAIN_RPC_PORT:-18444}"
PARENTCHAIN_DATADIR="${PARENTCHAIN_DATADIR:-$PROJECT_ROOT/coinshift-parentchain-data}"

echo "Stopping Coinshift user processes..."
pkill -f "coinshift_app.*coinshift-user" || true
sleep 1

echo "Stopping enforcer..."
pkill -f bip300301_enforcer || true
sleep 1

echo "Stopping mainchain..."
"$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" stop 2>/dev/null || true

echo "Stopping parentchain..."
"$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$PARENTCHAIN_RPC_PORT" -datadir="$PARENTCHAIN_DATADIR" stop 2>/dev/null || true

echo "Done."
