#!/bin/bash
#
# Deposit mainchain coins to all 3 Coinshift users (User1, User2, User3).
# Ensures each has a seed, gets deposit address, sends mainchain coins, creates deposit, mines blocks.
# Requires: mainchain, enforcer, and all 3 Coinshift instances running.
#

set -e

PROJECT_ROOT="${PROJECT_ROOT:-/home/parallels/Projects}"
BITCOIN_CLI="${BITCOIN_CLI:-$PROJECT_ROOT/bitcoin-patched/build/bin/bitcoin-cli}"
RPC_USER="${RPC_USER:-user}"
RPC_PASSWORD="${RPC_PASSWORD:-passwordDC}"
MAINCHAIN_RPC_PORT="${MAINCHAIN_RPC_PORT:-18443}"
MAINCHAIN_DATADIR="${MAINCHAIN_DATADIR:-$PROJECT_ROOT/coinshift-mainchain-data}"
MAINCHAIN_WALLET="${MAINCHAIN_WALLET:-mainchainwallet}"

# Coinshift RPC URLs (must match 5_start_coinshift_users.sh)
declare -a RPC_URLS=("http://127.0.0.1:6255" "http://127.0.0.1:6256" "http://127.0.0.1:6257")
declare -a LABELS=("User1" "User2" "User3")

# Per-user deposit amount (BTC) and fee (sats)
DEPOSIT_BTC="${1:-0.1}"
DEPOSIT_SATS="${DEPOSIT_SATS:-10000000}"   # 0.1 BTC
DEPOSIT_FEE_SATS="${DEPOSIT_FEE_SATS:-1000}"

rpc() {
    local url="$1"
    local method="$2"
    local params="$3"
    curl -s -X POST "$url" -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}"
}

# Get .result from JSON (prefer jq)
get_result() {
    local json="$1"
    if command -v jq >/dev/null 2>&1; then
        echo "$json" | jq -r '.result // empty'
    else
        echo "$json" | sed -n 's/.*"result"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
    fi
}

echo "=========================================="
echo "Deposit to all 3 Coinshift users"
echo "=========================================="
echo "Deposit: $DEPOSIT_BTC BTC ($DEPOSIT_SATS sats), fee: $DEPOSIT_FEE_SATS sats"
echo ""

# Check mainchain
if ! "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" getblockchaininfo >/dev/null 2>&1; then
    echo "ERROR: Mainchain not running. Run ./1_start_mainchain.sh first."
    exit 1
fi

MAIN_ADDR=$("$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
    -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
    -rpcwallet="$MAINCHAIN_WALLET" getnewaddress 2>/dev/null || true)
if [ -z "$MAIN_ADDR" ]; then
    echo "ERROR: Could not get mainchain wallet address."
    exit 1
fi

for i in 0 1 2; do
    url="${RPC_URLS[$i]}"
    label="${LABELS[$i]}"
    echo "--- $label ($url) ---"

    # Check RPC
    resp=$(rpc "$url" "getblockcount" "[]" 2>/dev/null || true)
    if ! echo "$resp" | grep -q '"result"'; then
        echo "  WARNING: $label RPC not reachable. Start Coinshift with ./5_start_coinshift_users.sh"
        continue
    fi

    # Get L2 address (or set seed first if wallet has none)
    l2_addr=$(get_result "$(rpc "$url" "get_new_address" "[]")")
    if [ -z "$l2_addr" ] || echo "$l2_addr" | grep -qi "error\|seed\|mnemonic"; then
        mnemonic=$(get_result "$(rpc "$url" "generate_mnemonic" "[]")")
        if [ -n "$mnemonic" ]; then
            mnemonic_escaped=$(echo "$mnemonic" | sed 's/"/\\"/g')
            rpc "$url" "set_seed_from_mnemonic" "[\"$mnemonic_escaped\"]" >/dev/null 2>&1 || true
            l2_addr=$(get_result "$(rpc "$url" "get_new_address" "[]")")
        fi
    fi
    if [ -z "$l2_addr" ]; then
        echo "  ERROR: Could not get new address from $label"
        continue
    fi
    deposit_addr=$(get_result "$(rpc "$url" "format_deposit_address" "[\"$l2_addr\"]")")
    if [ -z "$deposit_addr" ]; then
        echo "  ERROR: Could not get deposit address from $label"
        continue
    fi
    echo "  Deposit address: $deposit_addr"

    # Send mainchain coins to deposit address
    txid=$("$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
        -rpcwallet="$MAINCHAIN_WALLET" sendtoaddress "$deposit_addr" "$DEPOSIT_BTC" 2>/dev/null || true)
    if [ -z "$txid" ]; then
        echo "  ERROR: Failed to send $DEPOSIT_BTC BTC to $label. Mine mainchain: ./4_mine_blocks.sh mainchain 101"
        continue
    fi
    echo "  Sent $DEPOSIT_BTC BTC (txid: $txid)"

    # Mine 1 block to confirm
    "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
        generatetoaddress 1 "$MAIN_ADDR" >/dev/null 2>&1 || true

    # Create deposit (address, value_sats, fee_sats)
    rpc "$url" "create_deposit" "[\"$l2_addr\",$DEPOSIT_SATS,$DEPOSIT_FEE_SATS]" >/dev/null 2>&1 || true
    echo "  Create deposit called for $l2_addr"
    echo ""
done

# Mine several blocks so BIP300 processes deposits (e.g. 6)
echo "Mining 6 blocks on mainchain to process deposits..."
for _ in 1 2 3 4 5 6; do
    "$BITCOIN_CLI" -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" \
        -rpcport="$MAINCHAIN_RPC_PORT" -datadir="$MAINCHAIN_DATADIR" \
        generatetoaddress 1 "$MAIN_ADDR" >/dev/null 2>&1 || true
done

echo "=========================================="
echo "Done. Open client with ./7_open_client.sh [1|2|3] to see balances and swaps."
echo "=========================================="
