#!/bin/bash
#
# Create Enforcer Wallet Script
# 
# Usage:
#   ./create_enforcer_wallet.sh [password] [mnemonic_path]
#   
# Arguments:
#   password       Wallet password (optional - if not provided, wallet will be unencrypted)
#   mnemonic_path  Path to file containing 12-word mnemonic (optional - will generate if not provided)
#
# Examples:
#   ./create_enforcer_wallet.sh "mypassword"                    # Create encrypted wallet with generated mnemonic
#   ./create_enforcer_wallet.sh ""                              # Create unencrypted wallet with generated mnemonic
#   ./create_enforcer_wallet.sh "mypassword" /path/to/mnemonic  # Create encrypted wallet with existing mnemonic
#

# Exit on error
set -e

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

# Get password (first argument)
PASSWORD="${1:-}"

# Get mnemonic path (second argument)
MNEMONIC_PATH="${2:-}"

# Build request JSON
if [ -n "$MNEMONIC_PATH" ]; then
    # Use mnemonic from file
    if [ ! -f "$MNEMONIC_PATH" ]; then
        echo "ERROR: Mnemonic file not found: $MNEMONIC_PATH"
        exit 1
    fi
    
    # Read mnemonic words from file
    MNEMONIC_WORDS=$(cat "$MNEMONIC_PATH" | tr '\n' ' ' | xargs)
    MNEMONIC_WORD_COUNT=$(echo "$MNEMONIC_WORDS" | wc -w)
    
    if [ "$MNEMONIC_WORD_COUNT" -ne 12 ]; then
        echo "ERROR: Mnemonic must be exactly 12 words, found $MNEMONIC_WORD_COUNT"
        exit 1
    fi
    
    # Convert to JSON array
    MNEMONIC_JSON=$(echo "$MNEMONIC_WORDS" | awk '{
        printf "["
        for (i=1; i<=NF; i++) {
            if (i>1) printf ","
            printf "\"" $i "\""
        }
        printf "]"
    }')
    
    if [ -n "$PASSWORD" ]; then
        REQUEST_JSON=$(cat <<EOF
{
  "mnemonic_words": $MNEMONIC_JSON,
  "password": "$PASSWORD"
}
EOF
)
    else
        REQUEST_JSON=$(cat <<EOF
{
  "mnemonic_words": $MNEMONIC_JSON
}
EOF
)
    fi
else
    # Generate new mnemonic (empty mnemonic_words means generate)
    if [ -n "$PASSWORD" ]; then
        REQUEST_JSON=$(cat <<EOF
{
  "password": "$PASSWORD"
}
EOF
)
    else
        REQUEST_JSON=$(cat <<EOF
{}
EOF
)
    fi
fi

echo "Creating wallet..."
echo "  Password: ${PASSWORD:-"(none - unencrypted)"}"
echo "  Mnemonic: ${MNEMONIC_PATH:-"(will be generated)"}"
echo ""

# Temporarily disable exit on error to handle CreateWallet response
set +e
RESPONSE=$(grpcurl -plaintext -d @ "$ENFORCER_GRPC_ADDR" \
  cusf.mainchain.v1.WalletService/CreateWallet \
  <<< "$REQUEST_JSON" 2>&1)
EXIT_CODE=$?
set -e

if [ $EXIT_CODE -eq 0 ]; then
    echo "✓ Wallet created successfully!"
    echo ""
    echo "Next steps:"
    if [ -n "$PASSWORD" ]; then
        echo "  1. Unlock the wallet: ./unlock_enforcer_wallet.sh \"$PASSWORD\""
    else
        echo "  1. Wallet is already unlocked (no password)"
    fi
    echo "  2. Get a new address: grpcurl -plaintext $ENFORCER_GRPC_ADDR cusf.mainchain.v1.WalletService/CreateNewAddress"
    echo "  3. (Optional) Send funds from mainchain wallet to enforcer wallet address if needed"
    echo "  4. Mine blocks: ./mine_with_enforcer.sh"
else
    # Check if the error is "AlreadyExists"
    if echo "$RESPONSE" | grep -qi "already exists\|AlreadyExists"; then
        echo "⚠ Wallet already exists!"
        echo ""
        echo "The enforcer wallet has already been created."
        echo "If you need to unlock it, use: ./unlock_enforcer_wallet.sh [password]"
        exit 0
    else
        echo "✗ Failed to create wallet:"
        echo "$RESPONSE"
        exit 1
    fi
fi

