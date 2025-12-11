#!/bin/bash
PROJECT_ROOT="/home/parallels/Projects"
# Configuration - Update these paths as needed
BITCOIN_DIR="${PROJECT_ROOT}/bitcoin-patched/build/bin"
SIGNET_DATADIR="${PROJECT_ROOT}/coinshift-signet-data"
REGTEST_DATADIR="${PROJECT_ROOT}/coinshift-regtest-data"
ENFORCER="${PROJECT_ROOT}/bip300301_enforcer/target/debug/bip300301_enforcer"

BITCOIND="${BITCOIN_DIR}/bitcoind"
BITCOIN_CLI="${BITCOIN_DIR}/bitcoin-cli"

# Create the data directories if they do not exist
if [ ! -d "$SIGNET_DATADIR" ]; then
    mkdir -p "$SIGNET_DATADIR"
    print_info "Created directory: $SIGNET_DATADIR"
fi

if [ ! -d "$REGTEST_DATADIR" ]; then
    mkdir -p "$REGTEST_DATADIR"
    print_info "Created directory: $REGTEST_DATADIR"
fi

#!/bin/bash

# Network configuration
RPC_USER="user"
RPC_PASSWORD="passwordDC"
SIGNET_RPC_PORT=18443
REGTEST_RPC_PORT=18444
REGTEST_P2P_PORT=18445
SIGNET_DATADIR="${PROJECT_ROOT}/coinshift-signet-data"
REGTEST_DATADIR="${PROJECT_ROOT}/coinshift-regtest-data"
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
    
    local temp_node_started=false
    
    # Start regtest temporarily if not running
    if ! pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        # Check if port is in use but node not responding
        if check_port_in_use ${REGTEST_RPC_PORT}; then
            print_info "Port ${REGTEST_RPC_PORT} is in use, checking if node is responding..."
            if ${BITCOIN_CLI} -regtest \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${REGTEST_RPC_PORT} \
                -datadir=${REGTEST_DATADIR} \
                getblockchaininfo > /dev/null 2>&1; then
                print_info "Regtest node is already running and responding"
            else
                print_error "Port ${REGTEST_RPC_PORT} is in use but node is not responding"
                return 1
            fi
        else
            print_info "Starting temporary regtest node for key generation..."
            ${BITCOIND} -regtest \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${REGTEST_RPC_PORT} \
                -port=${REGTEST_P2P_PORT} \
                -noconnect \
                -datadir=${REGTEST_DATADIR} \
                -daemon > /dev/null 2>&1
            temp_node_started=true
            
            # Wait for RPC to be ready
            if ! wait_for_rpc_ready "regtest" ${REGTEST_RPC_PORT} ${REGTEST_DATADIR}; then
                print_error "Failed to start regtest node for key generation"
                return 1
            fi
        fi
    else
        # Node is running, wait for RPC to be ready
        if ! wait_for_rpc_ready "regtest" ${REGTEST_RPC_PORT} ${REGTEST_DATADIR}; then
            print_error "Regtest node is running but RPC is not responding"
            return 1
        fi
    fi
    
    # Create wallet if needed
    print_info "Creating temporary wallet for key generation..."
    ${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        createwallet "temp" > /dev/null 2>&1
    
    # Generate address and get public key
    print_info "Generating address and extracting public key..."
    ADDR=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        getnewaddress 2>/dev/null)
    
    if [ -z "$ADDR" ]; then
        print_error "Failed to generate address"
        if [ "$temp_node_started" = true ]; then
            print_info "Stopping temporary regtest node..."
            pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
        fi
        return 1
    fi
    
    PUBKEY=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        getaddressinfo ${ADDR} 2>/dev/null | grep -o '"pubkey": "[^"]*"' | cut -d'"' -f4)
    
    if [ -z "$PUBKEY" ]; then
        print_error "Failed to extract public key from address"
        if [ "$temp_node_started" = true ]; then
            print_info "Stopping temporary regtest node..."
            pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
        fi
        return 1
    fi
    
    # Create challenge script (1-of-1 multisig: OP_1 <pubkey> OP_1 OP_CHECKMULTISIG)
    SIGNET_CHALLENGE="5121${PUBKEY}51ae"
    
    # Save to file
    mkdir -p ${SIGNET_DATADIR}
    echo ${SIGNET_CHALLENGE} > ${SIGNET_CHALLENGE_FILE}
    
    # Stop temporary node if we started it
    if [ "$temp_node_started" = true ]; then
        print_info "Stopping temporary regtest node..."
        ${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            stop > /dev/null 2>&1 || true
        sleep 3
        # Force kill if still running
        pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
        sleep 1
    fi
    
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

# Check if port is in use
check_port_in_use() {
    local port=$1
    if command -v lsof > /dev/null 2>&1; then
        lsof -i :${port} > /dev/null 2>&1
        return $?
    elif command -v netstat > /dev/null 2>&1; then
        netstat -tuln 2>/dev/null | grep -q ":${port} "
        return $?
    elif command -v ss > /dev/null 2>&1; then
        ss -tuln 2>/dev/null | grep -q ":${port} "
        return $?
    fi
    return 1
}

# Wait for Bitcoin RPC to be ready
wait_for_rpc_ready() {
    local network=$1  # "signet" or "regtest"
    local port=$2
    local datadir=$3
    local challenge=${4:-""}  # Optional signet challenge
    local max_attempts=30
    local attempt=0
    local network_arg=""
    
    if [ "$network" = "signet" ]; then
        if [ -n "$challenge" ]; then
            network_arg="-signet -signetchallenge=${challenge}"
        elif [ -f "${SIGNET_CHALLENGE_FILE}" ]; then
            local challenge=$(cat ${SIGNET_CHALLENGE_FILE})
            network_arg="-signet -signetchallenge=${challenge}"
        else
            network_arg="-signet"
        fi
    else
        network_arg="-regtest"
    fi
    
    while [ $attempt -lt $max_attempts ]; do
        if ${BITCOIN_CLI} ${network_arg} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${port} \
            -datadir=${datadir} \
            getblockchaininfo > /dev/null 2>&1; then
            return 0
        fi
        attempt=$((attempt + 1))
        if [ $((attempt % 5)) -eq 0 ]; then
            print_info "Still waiting for ${network} RPC... (${attempt}/${max_attempts})"
        fi
        sleep 1
    done
    
    print_error "RPC did not become ready after ${max_attempts} seconds"
    return 1
}

# Start signet node
start_signet() {
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    # Check if process is running
    if pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_error "Signet node is already running (process found)"
        return 1
    fi
    
    # Check if port is in use
    if check_port_in_use ${SIGNET_RPC_PORT}; then
        print_error "Port ${SIGNET_RPC_PORT} is already in use"
        print_info "Trying to connect to existing node..."
        ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            getblockchaininfo > /dev/null 2>&1
        if [ $? -eq 0 ]; then
            print_info "Signet node is already running and responding on port ${SIGNET_RPC_PORT}"
            print_success "Signet node is available"
            return 0
        else
            print_error "Port ${SIGNET_RPC_PORT} is in use but node is not responding"
            print_info "You may need to stop the process using this port first"
            return 1
        fi
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
        -datadir=${SIGNET_DATADIR} > /dev/null 2>&1 &
    
    sleep 3
    
    # Verify it started successfully
    if pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        # Wait for RPC to be ready
        if wait_for_rpc_ready "signet" ${SIGNET_RPC_PORT} ${SIGNET_DATADIR} "${SIGNET_CHALLENGE}"; then
            print_success "Signet node started and is responding"
        else
            print_success "Signet node started (RPC may not be fully ready yet)"
        fi
    else
        print_error "Failed to start signet node"
        print_info "Check the logs or try stopping any conflicting processes"
        return 1
    fi
}

# Start regtest node
start_regtest() {
    # First, forcefully stop any existing regtest nodes
    print_info "Checking for existing regtest nodes..."
    if pgrep -f "bitcoind.*regtest" > /dev/null; then
        print_info "Found existing regtest process, attempting graceful shutdown..."
        ${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            stop > /dev/null 2>&1 || true
        sleep 3
        
        # Force kill if still running
        if pgrep -f "bitcoind.*regtest" > /dev/null; then
            print_info "Forcefully stopping regtest processes..."
            pkill -9 -f "bitcoind.*regtest" > /dev/null 2>&1 || true
            sleep 2
        fi
    fi
    
    # Check if RPC port is in use
    if check_port_in_use ${REGTEST_RPC_PORT}; then
        print_info "RPC port ${REGTEST_RPC_PORT} is in use, checking if node is responding..."
        if ${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            getblockchaininfo > /dev/null 2>&1; then
            print_info "Regtest node is already running and responding on port ${REGTEST_RPC_PORT}"
            print_success "Regtest node is available"
            return 0
        else
            print_error "Port ${REGTEST_RPC_PORT} is in use but node is not responding"
            print_info "Attempting to free the port..."
            pkill -9 -f "bitcoind.*regtest" > /dev/null 2>&1 || true
            sleep 2
            # Check again
            if check_port_in_use ${REGTEST_RPC_PORT}; then
                print_error "Port ${REGTEST_RPC_PORT} is still in use. Please manually stop the process using this port."
                return 1
            fi
        fi
    fi
    
    # Check if P2P port is in use
    if check_port_in_use ${REGTEST_P2P_PORT}; then
        print_info "P2P port ${REGTEST_P2P_PORT} is in use, attempting to free it..."
        pkill -9 -f "bitcoind.*regtest" > /dev/null 2>&1 || true
        sleep 2
        if check_port_in_use ${REGTEST_P2P_PORT}; then
            print_error "P2P port ${REGTEST_P2P_PORT} is still in use. Please manually stop the process using this port."
            return 1
        fi
    fi
    
    print_info "Starting regtest node..."
    
    # Check if bitcoind exists
    if [ ! -f "${BITCOIND}" ] || [ ! -x "${BITCOIND}" ]; then
        print_error "bitcoind not found or not executable at: ${BITCOIND}"
        return 1
    fi
    
    # Ensure datadir exists and is writable
    if [ ! -d "${REGTEST_DATADIR}" ]; then
        mkdir -p "${REGTEST_DATADIR}"
        print_info "Created regtest datadir: ${REGTEST_DATADIR}"
    fi
    if [ ! -w "${REGTEST_DATADIR}" ]; then
        print_error "Regtest datadir is not writable: ${REGTEST_DATADIR}"
        return 1
    fi
    
    # Create a temporary log file to capture startup errors
    TEMP_LOG=$(mktemp)
    
    print_info "Executing: ${BITCOIND} -regtest -rpcuser=${RPC_USER} -rpcpassword=*** -rpcport=${REGTEST_RPC_PORT} -port=${REGTEST_P2P_PORT} -server -txindex -rest -datadir=${REGTEST_DATADIR} ..."
    
    # Start bitcoind in background and capture any immediate errors
    ${BITCOIND} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -port=${REGTEST_P2P_PORT} \
        -server -txindex -rest \
        -zmqpubsequence=${ZMQ_SEQUENCE} \
        -zmqpubhashblock=${ZMQ_HASHBLOCK} \
        -zmqpubhashtx=${ZMQ_HASHTX} \
        -zmqpubrawblock=${ZMQ_RAWBLOCK} \
        -zmqpubrawtx=${ZMQ_RAWTX} \
        -listen \
        -datadir=${REGTEST_DATADIR} > "${TEMP_LOG}" 2>&1 &
    
    sleep 4
    
    # Verify it started successfully with pgrep
    if pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        rm -f "${TEMP_LOG}"
        # Wait for RPC to be ready
        if wait_for_rpc_ready "regtest" ${REGTEST_RPC_PORT} ${REGTEST_DATADIR}; then
            print_success "Regtest node started and is responding"
        else
            print_success "Regtest node started (RPC may not be fully ready yet)"
        fi
    else
        # Process didn't start, check log for errors
        if [ -s "${TEMP_LOG}" ]; then
            print_error "Failed to start regtest node. Error output:"
            cat "${TEMP_LOG}"
        else
            print_error "Failed to start regtest node (process not found, no error output)"
            print_info "Trying to run bitcoind command manually to see error..."
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
                -datadir=${REGTEST_DATADIR} 2>&1 | head -10 || true
        fi
        rm -f "${TEMP_LOG}"
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
    
    # Check if wallet already exists
    WALLET_LIST=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        listwallets 2>/dev/null)
    
    if echo "$WALLET_LIST" | grep -q "\"${SIGNET_WALLET}\""; then
        print_info "Signet wallet already exists and is loaded"
        return 0
    fi
    
    print_info "Creating signet wallet..."
    
    ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        createwallet ${SIGNET_WALLET} > /dev/null 2>&1
    
    if [ $? -eq 0 ]; then
        print_success "Signet wallet created and loaded"
    else
        # Wallet might exist but not be loaded, try loading it
        print_info "Wallet may already exist, attempting to load..."
        ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            loadwallet ${SIGNET_WALLET} > /dev/null 2>&1
        if [ $? -eq 0 ]; then
            print_success "Signet wallet loaded"
        else
            print_error "Failed to create or load signet wallet"
            return 1
        fi
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

# Load signet wallet (create if needed)
load_signet_wallet() {
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    # Wait a moment for RPC to be ready
    sleep 1
    
    # Check if wallet is already loaded
    WALLET_LIST=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        listwallets 2>/dev/null)
    
    if echo "$WALLET_LIST" | grep -q "\"${SIGNET_WALLET}\""; then
        print_info "Signet wallet is already loaded"
        return 0
    fi
    
    # Wallet not loaded, try to load it
    print_info "Loading signet wallet..."
    LOAD_RESULT=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        loadwallet ${SIGNET_WALLET} 2>&1)
    
    if [ $? -eq 0 ]; then
        print_success "Signet wallet loaded"
        return 0
    fi
    
    # If loading failed, wallet might not exist, create it
    if echo "$LOAD_RESULT" | grep -qi "not found\|does not exist"; then
        print_info "Wallet not found, creating signet wallet..."
        create_signet_wallet
        return $?
    else
        print_error "Failed to load signet wallet: $LOAD_RESULT"
        return 1
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
    
    # Check if signet node is running
    if ! pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_error "Signet node is not running. Please start it first (option 2)"
        return 1
    fi
    
    # Ensure wallet is loaded
    load_signet_wallet
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet wallet"
        return 1
    fi
    
    print_info "Mining ${blocks} blocks on signet..."
    
    ADDR=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        -rpcwallet=${SIGNET_WALLET} \
        getnewaddress 2>/dev/null)
    
    if [ -z "$ADDR" ]; then
        print_error "Failed to get new address. Make sure wallet is loaded."
        return 1
    fi
    
    ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        -rpcwallet=${SIGNET_WALLET} \
        generatetoaddress ${blocks} ${ADDR} > /dev/null
    
    if [ $? -eq 0 ]; then
        print_success "Mined ${blocks} blocks on signet"
        # Show balance
        BALANCE=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            getbalance 2>/dev/null)
        if [ ! -z "$BALANCE" ]; then
            print_info "Wallet balance: ${BALANCE} BTC"
        fi
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
    echo "12) Activate signet and mine (starts node + wallet + mines)"
    echo "0) Exit"
    echo ""
    read -p "Select option: " choice
}

# Activate signet and mine (convenience function)
activate_and_mine_signet() {
    local blocks=${1:-101}
    print_info "Activating signet and mining ${blocks} blocks..."
    
    # Generate challenge if needed
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    # Start signet node if not running
    if ! pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        start_signet
        if [ $? -ne 0 ]; then
            print_error "Failed to start signet node"
            return 1
        fi
        # Wait for node to be ready
        sleep 5
    else
        print_info "Signet node is already running"
    fi
    
    # Ensure wallet exists and is loaded
    load_signet_wallet
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet wallet"
        return 1
    fi
    
    # Mine blocks
    mine_signet ${blocks}
    
    if [ $? -eq 0 ]; then
        print_success "Signet activated and ${blocks} blocks mined!"
    fi
}

# Full setup
full_setup() {
    print_info "Starting full setup..."
    
    # Generate challenge
    generate_signet_challenge
    
    # Ensure regtest node is stopped before starting (in case generate_signet_challenge left it running)
    if pgrep -f "bitcoind.*regtest" > /dev/null; then
        print_info "Stopping any existing regtest node before starting fresh..."
        ${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            stop > /dev/null 2>&1 || true
        sleep 3
        # Force kill if still running
        pkill -9 -f "bitcoind.*regtest" > /dev/null 2>&1 || true
        sleep 2
    fi
    
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
            12)
                read -p "Number of blocks to mine (default 101): " blocks
                activate_and_mine_signet ${blocks:-101}
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