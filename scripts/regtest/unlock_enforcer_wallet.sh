#!/bin/bash
#
# Unlock Enforcer Wallet Script
# 
# Usage:
#   ./unlock_enforcer_wallet.sh [password]
#   
# Arguments:
#   password    Wallet password (if not provided, will prompt)
#
# Example:
#   ./unlock_enforcer_wallet.sh "mypassword"
#   ./unlock_enforcer_wallet.sh    # Will prompt for password
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

# Get password from argument or prompt
if [ -n "$1" ]; then
    PASSWORD="$1"
else
    echo "Enter wallet password (or press Enter if wallet has no password):"
    read -s PASSWORD
    echo ""
fi

# Prepare the request JSON
REQUEST_JSON=$(cat <<EOF
{
  "password": "$PASSWORD"
}
EOF
)

echo "Unlocking wallet..."
RESPONSE=$(grpcurl -plaintext -d @ "$ENFORCER_GRPC_ADDR" \
  cusf.mainchain.v1.WalletService/UnlockWallet \
  <<< "$REQUEST_JSON" 2>&1)

if [ $? -eq 0 ]; then
    echo "✓ Wallet unlocked successfully!"
else
    echo "✗ Failed to unlock wallet:"
    echo "$RESPONSE"
    exit 1
fi

