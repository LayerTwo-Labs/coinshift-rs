#!/bin/bash
# Simple swap testing script
# Requires: jq, curl, bitcoin-cli (regtest), thunder_app running

set -e

RPC_URL="http://127.0.0.1:8332"
BITCOIN_CLI="bitcoin-cli -regtest"

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

echo_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

echo_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

# RPC call helper
rpc_call() {
    local method=$1
    shift
    local params="$@"
    
    curl -s -X POST "$RPC_URL" \
        -H "Content-Type: application/json" \
        -d "{
            \"jsonrpc\": \"2.0\",
            \"id\": 1,
            \"method\": \"$method\",
            \"params\": $params
        }" | jq -r '.result'
}

# Check if services are running
check_services() {
    echo_info "Checking services..."
    
    if ! curl -s "$RPC_URL" > /dev/null 2>&1; then
        echo_error "Thunder RPC server not accessible at $RPC_URL"
        echo_info "Start with: cargo run --bin thunder_app -- --headless"
        exit 1
    fi
    
    if ! $BITCOIN_CLI getblockchaininfo > /dev/null 2>&1; then
        echo_error "Bitcoin regtest node not accessible"
        echo_info "Start with: bitcoind -regtest -daemon"
        exit 1
    fi
    
    echo_info "Services are running ✓"
}

# Generate addresses
setup_addresses() {
    echo_info "Setting up addresses..."
    
    ALICE_L1_ADDR=$($BITCOIN_CLI getnewaddress "alice")
    BOB_L1_ADDR=$($BITCOIN_CLI getnewaddress "bob")
    
    ALICE_L2_ADDR=$(rpc_call "get_new_address")
    BOB_L2_ADDR=$(rpc_call "get_new_address")
    
    echo_info "Alice L1: $ALICE_L1_ADDR"
    echo_info "Alice L2: $ALICE_L2_ADDR"
    echo_info "Bob L1: $BOB_L1_ADDR"
    echo_info "Bob L2: $BOB_L2_ADDR"
}

# Fund Alice's L1 address
fund_alice_l1() {
    echo_info "Funding Alice's L1 address..."
    $BITCOIN_CLI generatetoaddress 101 "$ALICE_L1_ADDR" > /dev/null
    $BITCOIN_CLI sendtoaddress "$ALICE_L1_ADDR" 1.0 > /dev/null
    $BITCOIN_CLI -generate 1 > /dev/null
    echo_info "Alice's L1 address funded ✓"
}

# Create a swap
create_swap() {
    echo_info "Creating swap..."
    
    local result=$(rpc_call "create_swap" "{
        \"parent_chain\": \"BTC\",
        \"l1_recipient_address\": \"$ALICE_L1_ADDR\",
        \"l1_amount_sats\": 100000,
        \"l2_recipient\": \"$BOB_L2_ADDR\",
        \"l2_amount_sats\": 50000,
        \"required_confirmations\": 1,
        \"fee_sats\": 1000
    }")
    
    SWAP_ID=$(echo "$result" | jq -r '.[0]')
    TXID=$(echo "$result" | jq -r '.[1]')
    
    if [ "$SWAP_ID" = "null" ] || [ -z "$SWAP_ID" ]; then
        echo_error "Failed to create swap"
        exit 1
    fi
    
    echo_info "Swap created: $SWAP_ID"
    echo_info "Transaction ID: $TXID"
}

# Check swap status
check_swap_status() {
    local swap_id=$1
    echo_info "Checking swap status: $swap_id"
    
    local status=$(rpc_call "get_swap_status" "{\"swap_id\": \"$swap_id\"}")
    local state=$(echo "$status" | jq -r '.state')
    
    echo_info "Swap state: $state"
    echo "$status" | jq '.'
}

# Simulate Bob sending L1 transaction
send_l1_transaction() {
    echo_info "Bob sending L1 transaction to Alice..."
    
    L1_TXID=$($BITCOIN_CLI sendtoaddress "$ALICE_L1_ADDR" 0.001)
    $BITCOIN_CLI -generate 1 > /dev/null
    
    echo_info "L1 Transaction ID: $L1_TXID"
    
    # Get confirmations
    local tx_info=$($BITCOIN_CLI gettransaction "$L1_TXID")
    CONFIRMATIONS=$(echo "$tx_info" | jq -r '.confirmations')
    
    echo_info "Confirmations: $CONFIRMATIONS"
}

# Update swap with L1 transaction
update_swap() {
    local swap_id=$1
    local l1_txid=$2
    local confirmations=$3
    
    echo_info "Updating swap $swap_id with L1 transaction $l1_txid..."
    
    rpc_call "update_swap_l1_txid" "{
        \"swap_id\": \"$swap_id\",
        \"l1_txid_hex\": \"$l1_txid\",
        \"confirmations\": $confirmations
    }" > /dev/null
    
    echo_info "Swap updated ✓"
}

# Claim swap
claim_swap() {
    local swap_id=$1
    
    echo_info "Claiming swap: $swap_id"
    
    local claim_txid=$(rpc_call "claim_swap" "{\"swap_id\": \"$swap_id\"}")
    
    if [ "$claim_txid" = "null" ] || [ -z "$claim_txid" ]; then
        echo_error "Failed to claim swap"
        exit 1
    fi
    
    echo_info "Claim transaction ID: $claim_txid"
}

# List all swaps
list_swaps() {
    echo_info "Listing all swaps..."
    rpc_call "list_swaps" "[]" | jq '.'
}

# Main test flow
main() {
    echo_info "Starting swap test..."
    
    check_services
    setup_addresses
    fund_alice_l1
    
    # Note: In a real scenario, Alice would need L2 coins first
    # This is a simplified test
    echo_warn "Note: Alice needs L2 coins to create a swap"
    echo_warn "This test assumes Alice already has L2 coins"
    
    create_swap
    check_swap_status "$SWAP_ID"
    
    echo_info "Waiting for swap to be included in a block..."
    sleep 2
    
    send_l1_transaction
    update_swap "$SWAP_ID" "$L1_TXID" "$CONFIRMATIONS"
    
    echo_info "Waiting for swap state to update..."
    sleep 2
    
    check_swap_status "$SWAP_ID"
    
    # Check if ready to claim
    local status=$(rpc_call "get_swap_status" "{\"swap_id\": \"$SWAP_ID\"}")
    local state=$(echo "$status" | jq -r '.state')
    
    if [ "$state" = "ReadyToClaim" ]; then
        claim_swap "$SWAP_ID"
        echo_info "Waiting for claim to be included..."
        sleep 2
        check_swap_status "$SWAP_ID"
    else
        echo_warn "Swap not ready to claim yet (state: $state)"
    fi
    
    list_swaps
    
    echo_info "Test completed!"
}

# Run main function
main

