#!/bin/bash
#
# Start Enforcer Script
# 
# Usage:
#   ./3_start_enforcer.sh [--skip-proposal]
#   
# Options:
#   --skip-proposal    Skip creating sidechain proposal (useful if it already exists)
#

# Exit on error
set -e

# Parse command line arguments
SKIP_PROPOSAL=0
for arg in "$@"; do
    case $arg in
        --skip-proposal)
            SKIP_PROPOSAL=1
            shift
            ;;
        *)
            # Unknown option
            ;;
    esac
done

export BITCOIN_DIR="/home/parallels/Projects/bitcoin-patched/build/bin"
export BITCOIN_CLI="$BITCOIN_DIR/bitcoin-cli"
export ENFORCER="/home/parallels/Projects/bip300301_enforcer/target/debug/bip300301_enforcer"
export RPC_USER="user"
export RPC_PASSWORD="passwordDC"

# Mainchain regtest (for sidechain activation)
export MAINCHAIN_RPC_PORT="18443"
export MAINCHAIN_DATADIR="/home/parallels/Projects/coinshift-mainchain-data"
export MAINCHAIN_WALLET="mainchainwallet"

# Enforcer
export ENFORCER_GRPC_PORT="50051"
export ENFORCER_GRPC_ADDR="127.0.0.1:$ENFORCER_GRPC_PORT"
export ENFORCER_GRPC_URL="http://$ENFORCER_GRPC_ADDR"

# ZMQ (for mainchain)
export ZMQ_SEQUENCE="tcp://127.0.0.1:29000"

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=========================================="
echo "Starting Enforcer"
echo "=========================================="

# Check if mainchain is running
echo "Checking if mainchain is running..."
if ! "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" getblockchaininfo >/dev/null 2>&1; then
    echo "ERROR: Mainchain is not running!"
    echo "Please run ./1_start_mainchain.sh first"
    exit 1
fi
echo "Mainchain is running ✓"

# Kill existing enforcer
echo "Stopping any existing enforcer..."
pkill -f bip300301_enforcer || true
sleep 2

# Start enforcer
echo "Starting enforcer..."
"$ENFORCER" \
  --node-rpc-addr=127.0.0.1:"$MAINCHAIN_RPC_PORT" \
  --node-rpc-user="$RPC_USER" \
  --node-rpc-pass="$RPC_PASSWORD" \
  --node-zmq-addr-sequence="$ZMQ_SEQUENCE" \
  --serve-grpc-addr "$ENFORCER_GRPC_ADDR" \
  --enable-wallet \
  --wallet-sync-source=disabled &

ENFORCER_PID=$!
echo "Enforcer started with PID: $ENFORCER_PID"

# Wait for enforcer gRPC to be ready
echo "Waiting for enforcer gRPC to be ready..."
max_attempts=30
attempt=0
while [ $attempt -lt $max_attempts ]; do
    if grpcurl -plaintext "$ENFORCER_GRPC_ADDR" list >/dev/null 2>&1; then
        echo "Enforcer gRPC is ready!"
        break
    fi
    attempt=$((attempt + 1))
    sleep 1
done

if [ $attempt -eq $max_attempts ]; then
    echo "WARNING: Enforcer gRPC may not be ready yet"
    echo "You can check manually with: grpcurl -plaintext $ENFORCER_GRPC_ADDR list"
fi

# Check if sidechain proposal already exists (unless skipped)
if [ "$SKIP_PROPOSAL" -eq 1 ]; then
    echo "Skipping sidechain proposal creation (--skip-proposal flag set)"
else
    echo "Checking for existing sidechain proposals..."
    PROPOSALS_OUTPUT=$(grpcurl -plaintext "$ENFORCER_GRPC_ADDR" \
      cusf.mainchain.v1.ValidatorService/GetSidechainProposals \
      2>&1)

    PROPOSAL_EXISTS=0
    PROPOSAL_APPROVED=0
    
    # Check if any proposals exist (approved or pending)
    if echo "$PROPOSALS_OUTPUT" | grep -q "sidechain_proposals"; then
        PROPOSAL_EXISTS=1
        # Check if proposal is already approved (has sidechain_number)
        if echo "$PROPOSALS_OUTPUT" | grep -q "sidechain_number"; then
            PROPOSAL_APPROVED=1
            echo "Sidechain proposal(s) already exist and are approved ✓"
            echo "Proposal details:"
            echo "$PROPOSALS_OUTPUT" | head -20
        else
            echo "Sidechain proposal(s) exist but not yet approved"
            echo "Proposal details:"
            echo "$PROPOSALS_OUTPUT" | head -20
        fi
    fi

    if [ "$PROPOSAL_EXISTS" -eq 0 ]; then
        echo "No existing sidechain proposals found"
        # Create sidechain proposal
        # Note: This requires a wallet to be created first (run create_enforcer_wallet.sh)
        echo "Creating sidechain proposal..."
        if [ -f "$SCRIPT_DIR/create_sidechain_proposal.json" ]; then
            CREATE_OUTPUT=$(grpcurl -plaintext -d @ "$ENFORCER_GRPC_ADDR" \
              cusf.mainchain.v1.WalletService/CreateSidechainProposal \
              < "$SCRIPT_DIR/create_sidechain_proposal.json" 2>&1)
            CREATE_EXIT_CODE=$?
            
            if [ $CREATE_EXIT_CODE -eq 0 ]; then
                echo "Sidechain proposal created ✓"
            else
                # Check if error is due to missing wallet
                if echo "$CREATE_OUTPUT" | grep -qi "wallet.*not.*exist\|no.*wallet\|wallet.*required"; then
                    echo "WARNING: Cannot create proposal - wallet does not exist yet"
                    echo "Please run ./create_enforcer_wallet.sh first, then re-run this script with --skip-proposal"
                    echo "Or create the wallet and proposal manually"
                    echo ""
                    echo "Error details: $CREATE_OUTPUT"
                    exit 1
                else
                    echo "WARNING: Failed to create proposal: $CREATE_OUTPUT"
                    exit 1
                fi
            fi
        else
            echo "WARNING: create_sidechain_proposal.json not found at $SCRIPT_DIR/create_sidechain_proposal.json"
            exit 1
        fi
    fi

    # Wait for proposal to be approved (check for sidechain_number)
    if [ "$PROPOSAL_APPROVED" -eq 0 ]; then
        echo ""
        echo "Waiting for sidechain proposal to be approved..."
        echo "A proposal is approved when it gets a sidechain_number assigned."
        echo ""
        
        max_wait_attempts=60
        wait_attempt=0
        
        while [ $wait_attempt -lt $max_wait_attempts ]; do
            # Check proposal status
            PROPOSALS_OUTPUT=$(grpcurl -plaintext "$ENFORCER_GRPC_ADDR" \
              cusf.mainchain.v1.ValidatorService/GetSidechainProposals \
              2>&1)
            
            # Check if proposal has a sidechain_number (indicates approval)
            if echo "$PROPOSALS_OUTPUT" | grep -q "sidechain_number"; then
                PROPOSAL_APPROVED=1
                echo "Sidechain proposal approved! ✓"
                echo "Proposal details:"
                echo "$PROPOSALS_OUTPUT" | head -30
                break
            fi
        
            wait_attempt=$((wait_attempt + 1))
            
            if [ $((wait_attempt % 5)) -eq 0 ]; then
                echo "  Still waiting... (attempt $wait_attempt/$max_wait_attempts)"
                echo "  Mining blocks to help with approval..."
                # Mine a few blocks to help with approval
                ADDR=$("$BITCOIN_CLI" -regtest -rpcwait \
                  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" \
                  -datadir="$MAINCHAIN_DATADIR" \
                  -rpcwallet="$MAINCHAIN_WALLET" \
                  getnewaddress 2>/dev/null || echo "")
                
                if [ -n "$ADDR" ]; then
                    "$BITCOIN_CLI" -regtest -rpcwait \
                      -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$MAINCHAIN_RPC_PORT" \
                      -datadir="$MAINCHAIN_DATADIR" \
                      generatetoaddress 3 "$ADDR" >/dev/null 2>&1 || true
                fi
            else
                sleep 2
            fi
        done
        
        if [ "$PROPOSAL_APPROVED" -eq 0 ]; then
            echo ""
            echo "WARNING: Sidechain proposal was not approved after $max_wait_attempts attempts"
            echo "Current proposal status:"
            echo "$PROPOSALS_OUTPUT"
            echo ""
            echo "You may need to:"
            echo "  1. Mine more blocks manually"
            echo "  2. Check enforcer logs for errors"
            echo "  3. Verify the proposal was created correctly"
            echo ""
            echo "To check proposal status manually:"
            echo "  grpcurl -plaintext $ENFORCER_GRPC_ADDR cusf.mainchain.v1.ValidatorService/GetSidechainProposals"
            echo ""
        fi
    fi
fi

# Find enforcer data directory
echo ""
echo "Finding enforcer data directory..."
# Try common locations
ENFORCER_DATADIR=""
if [ -d "$HOME/.local/share/bip300301_enforcer" ]; then
    ENFORCER_DATADIR="$HOME/.local/share/bip300301_enforcer"
elif [ -d "$HOME/Library/Application Support/bip300301_enforcer" ]; then
    ENFORCER_DATADIR="$HOME/Library/Application Support/bip300301_enforcer"
elif [ -d "/var/lib/bip300301_enforcer" ]; then
    ENFORCER_DATADIR="/var/lib/bip300301_enforcer"
else
    # Try to find it from the process
    ENFORCER_CMD=$(ps -p $ENFORCER_PID -o args= 2>/dev/null || echo "")
    if [ -n "$ENFORCER_CMD" ]; then
        # Look for --datadir or similar in the command
        if echo "$ENFORCER_CMD" | grep -q "datadir"; then
            ENFORCER_DATADIR=$(echo "$ENFORCER_CMD" | grep -oP 'datadir=\K[^\s]+' || echo "")
        fi
    fi
    # If still not found, check default location
    if [ -z "$ENFORCER_DATADIR" ] && [ -d "$HOME/.bip300301_enforcer" ]; then
        ENFORCER_DATADIR="$HOME/.bip300301_enforcer"
    fi
fi

if [ -z "$ENFORCER_DATADIR" ]; then
    ENFORCER_DATADIR="(default location - check enforcer logs or process)"
fi

echo ""
echo "=========================================="
echo "Enforcer is ready!"
echo "=========================================="
echo "gRPC Address: $ENFORCER_GRPC_ADDR"
echo "Enforcer PID: $ENFORCER_PID"
echo "Enforcer Data Directory: $ENFORCER_DATADIR"
echo ""
echo "All services are now running!"
echo ""
echo "To stop all services:"
echo "  pkill -f bitcoind"
echo "  pkill -f bip300301_enforcer"
echo ""

