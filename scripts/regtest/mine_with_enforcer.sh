#!/bin/bash
#
# Mine regtest blocks using the enforcer's GenerateBlocks gRPC method
# 
# Usage:
#   ./mine_with_enforcer.sh [count] [ack_all_proposals]
#   
# Arguments:
#   count              Number of blocks to mine (default: 1)
#   ack_all_proposals  Whether to ACK all sidechain proposals (default: false)
#
# Example:
#   ./mine_with_enforcer.sh 5 true    # Mine 5 blocks and ACK all proposals
#   ./mine_with_enforcer.sh 1         # Mine 1 block (default)
#

# Exit on error
set -e

# Default values
COUNT="${1:-1}"
ACK_ALL="${2:-false}"

# Enforcer gRPC settings (should match 3_start_enforcer.sh)
export ENFORCER_GRPC_PORT="50051"
export ENFORCER_GRPC_ADDR="127.0.0.1:$ENFORCER_GRPC_PORT"

# Check if grpcurl is available
if ! command -v grpcurl &> /dev/null; then
    echo "ERROR: grpcurl is not installed"
    echo "Install it with: go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest"
    exit 1
fi

# Check if enforcer gRPC is accessible
echo "Checking if enforcer gRPC is accessible..."
if ! grpcurl -plaintext "$ENFORCER_GRPC_ADDR" list >/dev/null 2>&1; then
    echo "ERROR: Enforcer gRPC is not accessible at $ENFORCER_GRPC_ADDR"
    echo "Make sure the enforcer is running (run ./3_start_enforcer.sh first)"
    exit 1
fi
echo "Enforcer gRPC is accessible ✓"

# Try to get wallet info to check if wallet is unlocked
# This is a simple check - if wallet is not unlocked, GetInfo will fail
echo "Checking if wallet is unlocked..."
WALLET_INFO=$(grpcurl -plaintext "$ENFORCER_GRPC_ADDR" \
  cusf.mainchain.v1.WalletService/GetInfo 2>&1)
WALLET_CHECK_EXIT=$?

if echo "$WALLET_INFO" | grep -qi "not.*unlock\|wallet.*not.*unlock"; then
    echo ""
    echo "ERROR: Enforcer wallet is not unlocked!"
    echo ""
    echo "To unlock the wallet, run:"
    echo "  ./unlock_enforcer_wallet.sh [password]"
    echo ""
    echo "If the wallet was created without a password, use:"
    echo "  ./unlock_enforcer_wallet.sh \"\""
    echo ""
    exit 1
elif [ $WALLET_CHECK_EXIT -eq 0 ] && echo "$WALLET_INFO" | grep -q "network"; then
    echo "Wallet is unlocked ✓"
else
    # If GetInfo failed for another reason, warn but continue
    echo "  WARNING: Could not verify wallet status (will attempt mining anyway)"
fi

# Prepare the request JSON
# Note: blocks is optional, but we'll include it for clarity
REQUEST_JSON=$(cat <<EOF
{
  "blocks": $COUNT,
  "ack_all_proposals": $ACK_ALL
}
EOF
)

echo "=========================================="
echo "Mining blocks with enforcer"
echo "=========================================="
echo "Number of blocks: $COUNT"
echo "ACK all proposals: $ACK_ALL"
echo ""

# Call GenerateBlocks gRPC method
# This returns a stream, so we'll process each response
echo "Mining blocks..."
BLOCK_NUM=1

# Use grpcurl to call the streaming RPC
# Capture stderr to check for errors
ERROR_OUTPUT=$(mktemp)
trap "rm -f $ERROR_OUTPUT" EXIT

grpcurl -plaintext -d @ "$ENFORCER_GRPC_ADDR" \
  cusf.mainchain.v1.WalletService/GenerateBlocks \
  <<< "$REQUEST_JSON" 2>"$ERROR_OUTPUT" | while IFS= read -r line; do
    # Parse JSON response - look for block_hash field
    if echo "$line" | grep -q "block_hash"; then
        # Extract block hash (handles both quoted and unquoted formats)
        BLOCK_HASH=$(echo "$line" | sed -n 's/.*"block_hash"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
        if [ -z "$BLOCK_HASH" ]; then
            # Try alternative format
            BLOCK_HASH=$(echo "$line" | sed -n 's/.*block_hash.*"\([a-fA-F0-9]\{64\}\)".*/\1/p')
        fi
        if [ -n "$BLOCK_HASH" ]; then
            echo "  ✓ Block $BLOCK_NUM mined: $BLOCK_HASH"
            BLOCK_NUM=$((BLOCK_NUM + 1))
        else
            # If parsing failed, show the raw line for debugging
            echo "  Response: $line"
        fi
    fi
done

# Check for errors in stderr
if [ -s "$ERROR_OUTPUT" ]; then
    ERROR_CONTENT=$(cat "$ERROR_OUTPUT")
    if echo "$ERROR_CONTENT" | grep -qi "not.*unlock\|wallet.*not.*unlock"; then
        echo ""
        echo "ERROR: Enforcer wallet is not unlocked!"
        echo ""
        echo "To unlock the wallet, run:"
        echo "  ./unlock_enforcer_wallet.sh [password]"
        echo ""
        echo "If the wallet was created without a password, use:"
        echo "  ./unlock_enforcer_wallet.sh \"\""
        echo ""
        exit 1
    else
        echo ""
        echo "Error occurred while mining:"
        echo "$ERROR_CONTENT"
        exit 1
    fi
fi

# Check if we mined any blocks
if [ $BLOCK_NUM -eq 1 ]; then
    echo "  WARNING: No blocks were reported. Check enforcer logs."
fi

echo ""
echo "=========================================="
echo "Mining complete!"
echo "=========================================="

