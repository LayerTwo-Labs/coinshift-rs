# Step by Step Setup Script

This document provides an interactive script to set up and manage your private signet network, regtest node, and enforcer.

## Quick Start

The setup script is located at `scripts/setup.sh`. You can run it directly:

```bash
./scripts/setup.sh
```

Or if you prefer to see the full script source:

```bash
#!/bin/bash

# Configuration - Update these paths as needed
BITCOIN_DIR="/Users/rob/projects/layertwolabs/bitcoin-patched/build/bin"
BITCOIND="${BITCOIN_DIR}/bitcoind"
BITCOIN_CLI="${BITCOIN_DIR}/bitcoin-cli"
ENFORCER="/Users/rob/projects/layertwolabs/bip300301_enforcer/target/debug/bip300301_enforcer"

# Network configuration
RPC_USER="user"
RPC_PASSWORD="passwordDC"
SIGNET_RPC_PORT=18443
REGTEST_RPC_PORT=18444
SIGNET_DATADIR="/Users/rob/projects/layertwolabs/coinshift-signet-data"
REGTEST_DATADIR="/Users/rob/projects/layertwolabs/coinshift-regtest-data"
SIGNET_WALLET="signetwallet"
REGTEST_WALLET="regtestwallet"

# ZMQ ports
ZMQ_SEQUENCE="tcp://127.0.0.1:29000"
ZMQ_HASHBLOCK="tcp://127.0.0.1:29001"
ZMQ_HASHTX="tcp://127.0.0.1:29002"
ZMQ_RAWBLOCK="tcp://127.0.0.1:29003"
ZMQ_RAWTX="tcp://127.0.0.1:29004"

# Signet challenge (will be generated if not set)
SIGNET_CHALLENGE_FILE="${SIGNET_DATADIR}/.signet_challenge"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Helper functions
print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

print_info() {
    echo -e "${YELLOW}ℹ${NC} $1"
}

# Generate signet challenge script
generate_signet_challenge() {
    print_info "Generating signet challenge script..."
    
    # Start regtest temporarily if not running
    if ! pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        print_info "Starting temporary regtest node for key generation..."
        ${BITCOIND} -regtest -rpcuser=${RPC_USER} -rpcpassword=${RPC_PASSWORD} -rpcport=${REGTEST_RPC_PORT} -daemon -datadir=${REGTEST_DATADIR} > /dev/null 2>&1
        sleep 3
    fi
    
    # Create wallet if needed
    ${BITCOIN_CLI} -regtest -rpcuser=${RPC_USER} -rpcpassword=${RPC_PASSWORD} -rpcport=${REGTEST_RPC_PORT} -datadir=${REGTEST_DATADIR} createwallet "temp" > /dev/null 2>&1
    
    # Generate address and get public key
    ADDR=$(${BITCOIN_CLI} -regtest -rpcuser=${RPC_USER} -rpcpassword=${RPC_PASSWORD} -rpcport=${REGTEST_RPC_PORT} -datadir=${REGTEST_DATADIR} getnewaddress)
    PUBKEY=$(${BITCOIN_CLI} -regtest -rpcuser=${RPC_USER} -rpcpassword=${RPC_PASSWORD} -rpcport=${REGTEST_RPC_PORT} -datadir=${REGTEST_DATADIR} getaddressinfo ${ADDR} | grep -o '"pubkey": "[^"]*"' | cut -d'"' -f4)
    
    if [ -z "$PUBKEY" ]; then
        print_error "Failed to generate public key"
        return 1
    fi
    
    # Create challenge script (1-of-1 multisig: OP_1 <pubkey> OP_1 OP_CHECKMULTISIG)
    SIGNET_CHALLENGE="5121${PUBKEY}51ae"
    
    # Save to file
    mkdir -p ${SIGNET_DATADIR}
    echo ${SIGNET_CHALLENGE} > ${SIGNET_CHALLENGE_FILE}
    
    print_success "Signet challenge generated: ${SIGNET_CHALLENGE}"
    return 0
}

# Load or generate signet challenge
load_signet_challenge() {
    if [ -f "${SIGNET_CHALLENGE_FILE}" ]; then
        SIGNET_CHALLENGE=$(cat ${SIGNET_CHALLENGE_FILE})
        print_info "Loaded existing signet challenge"
    else
        generate_signet_challenge
        if [ $? -ne 0 ]; then
            return 1
        fi
        SIGNET_CHALLENGE=$(cat ${SIGNET_CHALLENGE_FILE})
    fi
}

# Start signet node
start_signet() {
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    if pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_error "Signet node is already running"
        return 1
    fi
    
    print_info "Starting signet node with challenge: ${SIGNET_CHALLENGE:0:20}..."
    
    ${BITCOIND} -signet -noconnect \
        -signetchallenge=${SIGNET_CHALLENGE} \
        -fallbackfee=0.0002 \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -server -txindex -rest \
        -zmqpubsequence=${ZMQ_SEQUENCE} \
        -zmqpubhashblock=${ZMQ_HASHBLOCK} \
        -zmqpubhashtx=${ZMQ_HASHTX} \
        -zmqpubrawblock=${ZMQ_RAWBLOCK} \
        -zmqpubrawtx=${ZMQ_RAWTX} \
        -listen -port=38333 \
        -datadir=${SIGNET_DATADIR} &
    
    sleep 3
    if pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_success "Signet node started"
    else
        print_error "Failed to start signet node"
        return 1
    fi
}

# Start regtest node
start_regtest() {
    if pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        print_error "Regtest node is already running"
        return 1
    fi
    
    print_info "Starting regtest node..."
    
    ${BITCOIND} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -server -txindex -rest \
        -zmqpubsequence=${ZMQ_SEQUENCE} \
        -zmqpubhashblock=${ZMQ_HASHBLOCK} \
        -zmqpubhashtx=${ZMQ_HASHTX} \
        -zmqpubrawblock=${ZMQ_RAWBLOCK} \
        -zmqpubrawtx=${ZMQ_RAWTX} \
        -listen \
        -datadir=${REGTEST_DATADIR} &
    
    sleep 3
    if pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        print_success "Regtest node started"
    else
        print_error "Failed to start regtest node"
        return 1
    fi
}

# Create signet wallet
create_signet_wallet() {
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    print_info "Creating signet wallet..."
    
    ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        createwallet ${SIGNET_WALLET} > /dev/null 2>&1
    
    if [ $? -eq 0 ]; then
        print_success "Signet wallet created"
    else
        print_error "Failed to create signet wallet (may already exist)"
    fi
}

# Create regtest wallet
create_regtest_wallet() {
    print_info "Creating regtest wallet..."
    
    ${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        createwallet ${REGTEST_WALLET} > /dev/null 2>&1
    
    if [ $? -eq 0 ]; then
        print_success "Regtest wallet created"
    else
        print_error "Failed to create regtest wallet (may already exist)"
    fi
}

# Mine blocks on signet
mine_signet() {
    local blocks=${1:-101}
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    print_info "Mining ${blocks} blocks on signet..."
    
    ADDR=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        getnewaddress)
    
    ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        generatetoaddress ${blocks} ${ADDR} > /dev/null
    
    if [ $? -eq 0 ]; then
        print_success "Mined ${blocks} blocks on signet"
    else
        print_error "Failed to mine blocks"
        return 1
    fi
}

# Mine blocks on regtest
mine_regtest() {
    local blocks=${1:-101}
    print_info "Mining ${blocks} blocks on regtest..."
    
    ADDR=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        getnewaddress)
    
    ${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        generatetoaddress ${blocks} ${ADDR} > /dev/null
    
    if [ $? -eq 0 ]; then
        print_success "Mined ${blocks} blocks on regtest"
    else
        print_error "Failed to mine blocks"
        return 1
    fi
}

# Start enforcer
start_enforcer() {
    if pgrep -f "bip300301_enforcer" > /dev/null; then
        print_error "Enforcer is already running"
        return 1
    fi
    
    if ! pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_error "Signet node must be running first"
        return 1
    fi
    
    print_info "Starting enforcer..."
    
    ${ENFORCER} \
        --node-rpc-addr=127.0.0.1:${SIGNET_RPC_PORT} \
        --node-rpc-user=${RPC_USER} \
        --node-rpc-pass=${RPC_PASSWORD} \
        --node-zmq-addr-sequence=${ZMQ_SEQUENCE} \
        --enable-wallet \
        --wallet-sync-source=disabled &
    
    sleep 2
    if pgrep -f "bip300301_enforcer" > /dev/null; then
        print_success "Enforcer started"
    else
        print_error "Failed to start enforcer"
        return 1
    fi
}

# Stop all services
stop_all() {
    print_info "Stopping all services..."
    
    pkill -f "bip300301_enforcer" && print_success "Enforcer stopped" || print_info "Enforcer not running"
    pkill -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" && print_success "Signet node stopped" || print_info "Signet node not running"
    pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" && print_success "Regtest node stopped" || print_info "Regtest node not running"
}

# Check status
check_status() {
    echo ""
    echo "=== Service Status ==="
    if pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_success "Signet node: Running"
    else
        print_error "Signet node: Not running"
    fi
    
    if pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        print_success "Regtest node: Running"
    else
        print_error "Regtest node: Not running"
    fi
    
    if pgrep -f "bip300301_enforcer" > /dev/null; then
        print_success "Enforcer: Running"
    else
        print_error "Enforcer: Not running"
    fi
    echo ""
}

# Main menu
show_menu() {
    echo ""
    echo "=== Coinshift Setup Menu ==="
    echo "1) Generate signet challenge"
    echo "2) Start signet node"
    echo "3) Start regtest node"
    echo "4) Create signet wallet"
    echo "5) Create regtest wallet"
    echo "6) Mine blocks on signet (default: 101)"
    echo "7) Mine blocks on regtest (default: 101)"
    echo "8) Start enforcer"
    echo "9) Stop all services"
    echo "10) Check status"
    echo "11) Full setup (signet + regtest + wallets + mining)"
    echo "0) Exit"
    echo ""
    read -p "Select option: " choice
}

# Full setup
full_setup() {
    print_info "Starting full setup..."
    
    # Generate challenge
    generate_signet_challenge
    
    # Start nodes
    start_signet
    start_regtest
    
    # Wait for nodes to be ready
    sleep 5
    
    # Create wallets
    create_signet_wallet
    create_regtest_wallet
    
    # Mine initial blocks
    mine_signet 101
    mine_regtest 101
    
    print_success "Full setup complete!"
    check_status
}

# Main loop
main() {
    while true; do
        show_menu
        case $choice in
            1)
                generate_signet_challenge
                ;;
            2)
                start_signet
                ;;
            3)
                start_regtest
                ;;
            4)
                create_signet_wallet
                ;;
            5)
                create_regtest_wallet
                ;;
            6)
                read -p "Number of blocks to mine (default 101): " blocks
                mine_signet ${blocks:-101}
                ;;
            7)
                read -p "Number of blocks to mine (default 101): " blocks
                mine_regtest ${blocks:-101}
                ;;
            8)
                start_enforcer
                ;;
            9)
                stop_all
                ;;
            10)
                check_status
                ;;
            11)
                full_setup
                ;;
            0)
                echo "Exiting..."
                exit 0
                ;;
            *)
                print_error "Invalid option"
                ;;
        esac
        echo ""
        read -p "Press Enter to continue..."
    done
}

# Run main if script is executed directly
if [ "${BASH_SOURCE[0]}" == "${0}" ]; then
    main
fi
```

## Usage

1. **Run the script:**
   ```bash
   ./scripts/setup.sh
   ```

2. **Select options from the menu:**
   - **Option 11**: Performs a full setup (generates challenge, starts both nodes, creates wallets, and mines initial blocks) - recommended for first-time setup
   - **Individual options**: Allow you to control each step separately for fine-grained control
   - **Option 10**: Check status of all services at any time
   - **Option 9**: Stop all services cleanly

## Notes

- The signet challenge is automatically generated and saved to `${SIGNET_DATADIR}/.signet_challenge`
- All paths are configurable at the top of the script
- The script checks if services are already running before starting them
- Use option 10 to check the status of all services at any time
- Use option 9 to stop all services cleanly
