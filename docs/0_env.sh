#!/bin/bash
# Source this file to set env for regtest scripts (optional).
# Usage: source ./0_env.sh  OR  . ./0_env.sh

export PROJECT_ROOT="${PROJECT_ROOT:-/home/parallels/Projects}"
export BITCOIN_DIR="${BITCOIN_DIR:-$PROJECT_ROOT/bitcoin-patched/build/bin}"
export BITCOIND="${BITCOIND:-$BITCOIN_DIR/bitcoind}"
export BITCOIN_CLI="${BITCOIN_CLI:-$BITCOIN_DIR/bitcoin-cli}"
export ENFORCER="${ENFORCER:-$PROJECT_ROOT/bip300301_enforcer/target/debug/bip300301_enforcer}"
export RPC_USER="${RPC_USER:-user}"
export RPC_PASSWORD="${RPC_PASSWORD:-passwordDC}"

# Mainchain
export MAINCHAIN_RPC_PORT="${MAINCHAIN_RPC_PORT:-18443}"
export MAINCHAIN_P2P_PORT="${MAINCHAIN_P2P_PORT:-38333}"
export MAINCHAIN_DATADIR="${MAINCHAIN_DATADIR:-$PROJECT_ROOT/coinshift-mainchain-data}"
export MAINCHAIN_WALLET="${MAINCHAIN_WALLET:-mainchainwallet}"
export ZMQ_SEQUENCE="${ZMQ_SEQUENCE:-tcp://127.0.0.1:29000}"
export ZMQ_HASHBLOCK="${ZMQ_HASHBLOCK:-tcp://127.0.0.1:29001}"
export ZMQ_HASHTX="${ZMQ_HASHTX:-tcp://127.0.0.1:29002}"
export ZMQ_RAWBLOCK="${ZMQ_RAWBLOCK:-tcp://127.0.0.1:29003}"
export ZMQ_RAWTX="${ZMQ_RAWTX:-tcp://127.0.0.1:29004}"

# Parentchain
export PARENTCHAIN_RPC_PORT="${PARENTCHAIN_RPC_PORT:-18444}"
export PARENTCHAIN_P2P_PORT="${PARENTCHAIN_P2P_PORT:-38334}"
export PARENTCHAIN_DATADIR="${PARENTCHAIN_DATADIR:-$PROJECT_ROOT/coinshift-parentchain-data}"
export PARENTCHAIN_WALLET="${PARENTCHAIN_WALLET:-parentchainwallet}"

# Enforcer
export ENFORCER_GRPC_PORT="${ENFORCER_GRPC_PORT:-50051}"
export ENFORCER_GRPC_ADDR="${ENFORCER_GRPC_ADDR:-127.0.0.1:$ENFORCER_GRPC_PORT}"
export ENFORCER_GRPC_URL="${ENFORCER_GRPC_URL:-http://$ENFORCER_GRPC_ADDR}"

# Coinshift (3 users)
export COINSHIFT_BIN="${COINSHIFT_BIN:-$PROJECT_ROOT/coinshift-rs/target/release/coinshift_app}"
# If release not built, fallback to cargo run from repo root
export COINSHIFT_REPO="${COINSHIFT_REPO:-$PROJECT_ROOT/coinshift-rs}"
