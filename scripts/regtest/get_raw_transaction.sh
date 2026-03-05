#!/bin/bash
#
# Get Raw Transaction Script
# 
# Retrieves raw transaction data from the mainchain or parentchain Bitcoin Core RPC
# 
# Usage:
#   ./get_raw_transaction.sh [chain] [txid] [verbose]
#   
# Arguments:
#   chain     Chain to query: "mainchain" or "parentchain" (default: "mainchain")
#   txid      Transaction ID (required)
#   verbose   If set to "true" or "1", returns decoded transaction (default: false)
#
# Example:
#   ./get_raw_transaction.sh mainchain abc123...    # Get raw hex from mainchain
#   ./get_raw_transaction.sh parentchain abc123... true  # Get decoded from parentchain
#   ./get_raw_transaction.sh abc123...              # Get raw hex from mainchain (default)
#

# Exit on error
set -e

# Parse arguments
echo "=========================================="
echo "Get Raw Transaction Script"
echo "=========================================="
echo "[DEBUG] Parsing arguments..."
echo "[DEBUG] Argument 1: '${1:-<empty>}'"
echo "[DEBUG] Argument 2: '${2:-<empty>}'"
echo "[DEBUG] Argument 3: '${3:-<empty>}'"
echo ""

CHAIN="${1:-mainchain}"
TXID=""
VERBOSE=""

# Determine which argument is the txid (skip chain if it's "mainchain" or "parentchain")
if [ "$CHAIN" != "mainchain" ] && [ "$CHAIN" != "parentchain" ]; then
    # First argument is actually the txid, not the chain
    TXID="$CHAIN"
    CHAIN="mainchain"
    VERBOSE="${2:-false}"
    echo "[DEBUG] First argument is TXID (not a chain name)"
else
    # First argument is the chain
    TXID="$2"
    VERBOSE="${3:-false}"
    echo "[DEBUG] First argument is chain name"
fi

echo "[DEBUG] Parsed values:"
echo "[DEBUG]   CHAIN: '$CHAIN'"
echo "[DEBUG]   TXID: '$TXID'"
echo "[DEBUG]   VERBOSE: '$VERBOSE'"
echo ""

# Check if txid is provided
if [ -z "$TXID" ]; then
    echo "ERROR: Transaction ID is required"
    echo ""
    echo "Usage: ./get_raw_transaction.sh [chain] [txid] [verbose]"
    echo ""
    echo "Arguments:"
    echo "  chain     Chain to query: 'mainchain' or 'parentchain' (default: 'mainchain')"
    echo "  txid      Transaction ID (required)"
    echo "  verbose   If set to 'true' or '1', returns decoded transaction (default: false)"
    echo ""
    echo "Examples:"
    echo "  ./get_raw_transaction.sh mainchain abc123...    # Get raw hex from mainchain"
    echo "  ./get_raw_transaction.sh parentchain abc123... true  # Get decoded from parentchain"
    echo "  ./get_raw_transaction.sh abc123...              # Get raw hex from mainchain (default)"
    exit 1
fi

# Validate chain argument
if [ "$CHAIN" != "mainchain" ] && [ "$CHAIN" != "parentchain" ]; then
    echo "ERROR: Invalid chain '$CHAIN'. Must be 'mainchain' or 'parentchain'"
    exit 1
fi

# Bitcoin Core settings
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

# Set chain-specific variables
echo "[DEBUG] Setting chain-specific variables..."
if [ "$CHAIN" = "mainchain" ]; then
    RPC_PORT="$MAINCHAIN_RPC_PORT"
    DATADIR="$MAINCHAIN_DATADIR"
    CHAIN_NAME="Mainchain"
    START_SCRIPT="./1_start_mainchain.sh"
    echo "[DEBUG] Selected: Mainchain"
else
    RPC_PORT="$PARENTCHAIN_RPC_PORT"
    DATADIR="$PARENTCHAIN_DATADIR"
    CHAIN_NAME="Parentchain"
    START_SCRIPT="./2_start_parentchain.sh"
    echo "[DEBUG] Selected: Parentchain"
fi
echo "[DEBUG]   RPC_PORT: $RPC_PORT"
echo "[DEBUG]   DATADIR: $DATADIR"
echo "[DEBUG]   BITCOIN_CLI: $BITCOIN_CLI"
echo ""

# Check if chain is running
echo "[DEBUG] Checking if $CHAIN_NAME is running..."
echo "[DEBUG] Executing: $BITCOIN_CLI -regtest -rpcuser=$RPC_USER -rpcpassword=*** -rpcport=$RPC_PORT -datadir=$DATADIR getblockchaininfo"
CHECK_RESULT=$("$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$RPC_PORT" -datadir="$DATADIR" getblockchaininfo 2>&1)
CHECK_EXIT=$?
echo "[DEBUG] Check exit code: $CHECK_EXIT"
if [ $CHECK_EXIT -ne 0 ]; then
    echo "[DEBUG] Check output:"
    echo "$CHECK_RESULT"
    echo ""
    echo "ERROR: $CHAIN_NAME is not running!"
    echo "Please run $START_SCRIPT first"
    exit 1
fi
echo "[DEBUG] $CHAIN_NAME is running ✓"
echo ""

# Determine if verbose mode
VERBOSE_FLAG=""
if [ "$VERBOSE" = "true" ] || [ "$VERBOSE" = "1" ]; then
    VERBOSE_FLAG="1"
    echo "[DEBUG] Mode: Decoded transaction (verbose)"
else
    echo "[DEBUG] Mode: Raw hex transaction"
fi
echo ""

# Get raw transaction
echo "[DEBUG] Fetching transaction from $CHAIN_NAME: $TXID"
if [ -n "$VERBOSE_FLAG" ]; then
    # Get decoded transaction
    echo "[DEBUG] Executing command (decoded mode):"
    echo "[DEBUG]   $BITCOIN_CLI -regtest -rpcuser=$RPC_USER -rpcpassword=*** -rpcport=$RPC_PORT -datadir=$DATADIR getrawtransaction \"$TXID\" \"$VERBOSE_FLAG\""
    RESULT=$("$BITCOIN_CLI" -regtest \
      -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
      -rpcport="$RPC_PORT" -datadir="$DATADIR" \
      getrawtransaction "$TXID" "$VERBOSE_FLAG" 2>&1)
    EXIT_CODE=$?
else
    # Get raw hex transaction
    echo "[DEBUG] Executing command (raw hex mode):"
    echo "[DEBUG]   $BITCOIN_CLI -regtest -rpcuser=$RPC_USER -rpcpassword=*** -rpcport=$RPC_PORT -datadir=$DATADIR getrawtransaction \"$TXID\""
    RESULT=$("$BITCOIN_CLI" -regtest \
      -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
      -rpcport="$RPC_PORT" -datadir="$DATADIR" \
      getrawtransaction "$TXID" 2>&1)
    EXIT_CODE=$?
fi

echo "[DEBUG] Command exit code: $EXIT_CODE"
echo "[DEBUG] Raw result length: ${#RESULT} characters"
echo "[DEBUG] Raw result (first 200 chars): ${RESULT:0:200}"
if [ ${#RESULT} -gt 200 ]; then
    echo "[DEBUG] ... (truncated, full result below)"
fi
echo ""

# Check if command failed (non-zero exit code)
echo "[DEBUG] Checking exit code..."
if [ $EXIT_CODE -ne 0 ]; then
    echo ""
    echo "✗ ERROR: Failed to get transaction"
    echo "  Exit code: $EXIT_CODE"
    echo "  Transaction ID: $TXID"
    echo "  Chain: $CHAIN_NAME (RPC port: $RPC_PORT)"
    echo ""
    echo "[DEBUG] Full error output:"
    echo "---"
    echo "$RESULT"
    echo "---"
    exit 1
fi
echo "[DEBUG] Exit code check passed (0)"

# Check if the result contains an error message (even if exit code was 0)
# Bitcoin RPC sometimes returns errors in JSON format with exit code 0
echo "[DEBUG] Checking for JSON error response..."
# Check for JSON error response pattern: {"error": {...}}
if echo "$RESULT" | grep -qi "\"error\""; then
    echo "[DEBUG] Found JSON error pattern in response"
    echo ""
    echo "✗ ERROR: RPC returned an error response"
    echo "  Transaction ID: $TXID"
    echo "  Chain: $CHAIN_NAME (RPC port: $RPC_PORT)"
    echo ""
    echo "Error details:"
    echo "---"
    # Try to extract and pretty print the error if jq is available
    if command -v jq &> /dev/null; then
        # Try to extract error message and code
        ERROR_MSG=$(echo "$RESULT" | jq -r '.error.message // .error // empty' 2>/dev/null)
        ERROR_CODE=$(echo "$RESULT" | jq -r '.error.code // empty' 2>/dev/null)
        if [ -n "$ERROR_MSG" ]; then
            if [ -n "$ERROR_CODE" ]; then
                echo "Error code: $ERROR_CODE"
            fi
            echo "Error message: $ERROR_MSG"
            echo ""
            echo "Full response:"
            echo "$RESULT" | jq . 2>/dev/null || echo "$RESULT"
        else
            echo "$RESULT" | jq . 2>/dev/null || echo "$RESULT"
        fi
    else
        echo "$RESULT"
    fi
    echo "---"
    exit 1
fi

echo "[DEBUG] JSON error check passed"
echo "[DEBUG] Checking for plain text error patterns..."
# Also check for common error patterns in non-JSON responses
if echo "$RESULT" | grep -qiE "^(error|Error|ERROR|failed|Failed|FAILED|not found|Not found|NOT FOUND|invalid|Invalid|INVALID)"; then
    echo "[DEBUG] Found plain text error pattern in response"
    echo ""
    echo "✗ ERROR: Command returned an error"
    echo "  Transaction ID: $TXID"
    echo "  Chain: $CHAIN_NAME (RPC port: $RPC_PORT)"
    echo ""
    echo "Error details:"
    echo "---"
    echo "$RESULT"
    echo "---"
    exit 1
fi
echo "[DEBUG] Plain text error check passed"
echo ""

# Output the result
echo "[DEBUG] Preparing to output transaction data..."
echo ""
echo "Transaction data:"
echo "---"
if [ -n "$VERBOSE_FLAG" ]; then
    # Pretty print JSON if jq is available
    if command -v jq &> /dev/null; then
        echo "$RESULT" | jq .
    else
        echo "$RESULT"
    fi
else
    echo "$RESULT"
fi
echo "---"
echo ""
echo "[DEBUG] Output complete"
echo "✓ Transaction retrieved successfully!"
echo ""

