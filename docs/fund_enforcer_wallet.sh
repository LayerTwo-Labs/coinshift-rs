#!/bin/bash
#
# Fund Enforcer Wallet Script
# 
# Sends funds from the mainchain Bitcoin Core wallet to the enforcer wallet
# 
# Usage:
#   ./fund_enforcer_wallet.sh [amount]
#   
# Arguments:
#   amount    Amount in BTC to send (default: 1.0)
#
# Example:
#   ./fund_enforcer_wallet.sh 1.0    # Send 1 BTC
#   ./fund_enforcer_wallet.sh         # Send 1 BTC (default)
#

# Exit on error
set -e

# Default amount
AMOUNT="${1:-1.0}"

# Bitcoin Core settings (should match 1_start_mainchain.sh)
export BITCOIN_DIR="/home/parallels/Projects/bitcoin-patched/build/bin"
export BITCOIN_CLI="$BITCOIN_DIR/bitcoin-cli"
export RPC_USER="user"
export RPC_PASSWORD="passwordDC"
export MAINCHAIN_RPC_PORT="18443"
export MAINCHAIN_DATADIR="/home/parallels/Projects/coinshift-mainchain-data"
export MAINCHAIN_WALLET="mainchainwallet"

# Enforcer gRPC settings (should match 3_start_enforcer.sh)
export ENFORCER_GRPC_PORT="50051"
export ENFORCER_GRPC_ADDR="127.0.0.1:$ENFORCER_GRPC_PORT"

# Check if grpcurl is available
if ! command -v grpcurl &> /dev/null; then
    echo "ERROR: grpcurl is not installed"
    echo "Install it with: go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest"
    exit 1
fi

# Check if mainchain is running
echo "Checking if mainchain is running..."
if ! "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" getblockchaininfo >/dev/null 2>&1; then
    echo "ERROR: Mainchain is not running!"
    echo "Please run ./1_start_mainchain.sh first"
    exit 1
fi
echo "Mainchain is running ✓"

# Check if enforcer gRPC is accessible
echo "Checking if enforcer gRPC is accessible..."
if ! grpcurl -plaintext "$ENFORCER_GRPC_ADDR" list >/dev/null 2>&1; then
    echo "ERROR: Enforcer gRPC is not accessible at $ENFORCER_GRPC_ADDR"
    echo "Make sure the enforcer is running (run ./3_start_enforcer.sh first)"
    exit 1
fi
echo "Enforcer gRPC is accessible ✓"

# Check if enforcer wallet is unlocked
echo "Checking if enforcer wallet is unlocked..."
WALLET_INFO=$(grpcurl -plaintext "$ENFORCER_GRPC_ADDR" \
  cusf.mainchain.v1.WalletService/GetInfo 2>&1)
if echo "$WALLET_INFO" | grep -qi "not.*unlock\|wallet.*not.*unlock"; then
    echo "ERROR: Enforcer wallet is not unlocked!"
    echo "Please unlock it first: ./unlock_enforcer_wallet.sh [password]"
    exit 1
fi
echo "Enforcer wallet is unlocked ✓"

# Get enforcer wallet address
echo "Getting enforcer wallet address..."
ENFORCER_ADDR_RESPONSE=$(grpcurl -plaintext "$ENFORCER_GRPC_ADDR" \
  cusf.mainchain.v1.WalletService/CreateNewAddress 2>&1)

if [ $? -ne 0 ]; then
    echo "ERROR: Failed to get enforcer wallet address:"
    echo "$ENFORCER_ADDR_RESPONSE"
    exit 1
fi

# Try to parse with jq first (more reliable), fallback to grep/sed
if command -v jq &> /dev/null; then
    ENFORCER_ADDR=$(echo "$ENFORCER_ADDR_RESPONSE" | jq -r '.address // empty' 2>/dev/null)
else
    # Fallback: extract address value from JSON (handles both single-line and multi-line JSON)
    # First try: match "address": "value" pattern (handles pretty-printed JSON)
    ENFORCER_ADDR=$(echo "$ENFORCER_ADDR_RESPONSE" | grep -A1 '"address"' | grep -o '"[^"]*"' | tail -1 | tr -d '"')
    # If that didn't work, try matching on same line
    if [ -z "$ENFORCER_ADDR" ]; then
        ENFORCER_ADDR=$(echo "$ENFORCER_ADDR_RESPONSE" | grep -o '"address"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"address"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')
    fi
fi

if [ -z "$ENFORCER_ADDR" ]; then
    echo "ERROR: Could not parse address from response:"
    echo "$ENFORCER_ADDR_RESPONSE"
    exit 1
fi

echo "Enforcer wallet address: $ENFORCER_ADDR"

# Check mainchain wallet balance
echo "Checking mainchain wallet balance..."
MAINCHAIN_BALANCE=$("$BITCOIN_CLI" -regtest \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
  -rpcwallet="$MAINCHAIN_WALLET" \
  getbalance 2>/dev/null || echo "0")

echo "Mainchain wallet balance: $MAINCHAIN_BALANCE BTC"

# Check if we have enough funds
if (( $(echo "$MAINCHAIN_BALANCE < $AMOUNT" | bc -l) )); then
    echo "ERROR: Insufficient funds in mainchain wallet"
    echo "  Required: $AMOUNT BTC"
    echo "  Available: $MAINCHAIN_BALANCE BTC"
    echo ""
    echo "Mine some blocks first: ./4_mine_blocks.sh mainchain 101"
    exit 1
fi

# Send funds
echo ""
echo "Sending $AMOUNT BTC from mainchain wallet to enforcer wallet..."
TXID=$("$BITCOIN_CLI" -regtest \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
  -rpcwallet="$MAINCHAIN_WALLET" \
  sendtoaddress "$ENFORCER_ADDR" "$AMOUNT" 2>&1)

if [ $? -eq 0 ]; then
    echo "✓ Transaction sent successfully!"
    echo "  Transaction ID: $TXID"
    echo ""
    echo "Waiting for confirmation..."
    "$BITCOIN_CLI" -regtest \
      -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
      -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
      generatetoaddress 1 "$("$BITCOIN_CLI" -regtest \
        -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
        -rpcwallet="$MAINCHAIN_WALLET" \
        getnewaddress)" >/dev/null 2>&1
    
    echo "✓ Transaction confirmed!"
    echo ""
    echo "Check enforcer wallet balance:"
    echo "  grpcurl -plaintext $ENFORCER_GRPC_ADDR cusf.mainchain.v1.WalletService/GetBalance"
else
    echo "✗ Failed to send transaction:"
    echo "$TXID"
    exit 1
fi

