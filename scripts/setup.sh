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
SIGNET_PRIVKEY_FILE="${SIGNET_DATADIR}/.signet_privkey"

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

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
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
    
    # Create descriptor wallet (default) for key generation
    # Since BDB is not available, we must use descriptor wallets
    print_info "Creating temporary descriptor wallet for key generation..."
    WALLET_CREATE_RESULT=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        createwallet "temp" 2>&1)
    
    WALLET_CREATE_EXIT=$?
    
    # Check if wallet already exists
    if [ $WALLET_CREATE_EXIT -ne 0 ]; then
        if echo "$WALLET_CREATE_RESULT" | grep -qi "already exists"; then
            print_info "Temporary wallet already exists, loading it..."
            # Try to load the existing wallet
            LOAD_RESULT=$(${BITCOIN_CLI} -regtest \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${REGTEST_RPC_PORT} \
                -datadir=${REGTEST_DATADIR} \
                loadwallet "temp" 2>&1)
            if [ $? -ne 0 ] && ! echo "$LOAD_RESULT" | grep -qi "already loaded"; then
                print_error "Failed to load existing temporary wallet: ${LOAD_RESULT}"
                if [ "$temp_node_started" = true ]; then
                    print_info "Stopping temporary regtest node..."
                    pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
                fi
                return 1
            fi
        else
            print_error "Failed to create temporary wallet: ${WALLET_CREATE_RESULT}"
            if [ "$temp_node_started" = true ]; then
                print_info "Stopping temporary regtest node..."
                pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
            fi
            return 1
        fi
    else
        print_info "Temporary descriptor wallet created successfully"
    fi
    
    # Wait a moment for wallet to be ready
    sleep 2
    
    # Verify wallet is loaded
    WALLET_LIST=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        listwallets 2>&1)
    
    if ! echo "$WALLET_LIST" | grep -q "\"temp\""; then
        print_error "Temporary wallet 'temp' is not in loaded wallets list: ${WALLET_LIST}"
        print_info "Attempting to load it..."
        ${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            loadwallet "temp" > /dev/null 2>&1
        sleep 1
    fi
    
    # Generate address and get public key (must use -rpcwallet=temp)
    print_info "Generating address and extracting public key..."
    ADDR=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=temp \
        getnewaddress 2>&1)
    
    if [ -z "$ADDR" ] || echo "$ADDR" | grep -qi "error"; then
        print_error "Failed to generate address. Response: ${ADDR}"
        if [ "$temp_node_started" = true ]; then
            print_info "Stopping temporary regtest node..."
            pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
        fi
        return 1
    fi
    
    print_info "Generated address: ${ADDR}"
    
    PUBKEY=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=temp \
        getaddressinfo ${ADDR} 2>&1 | grep -o '"pubkey": "[^"]*"' | cut -d'"' -f4)
    
    if [ -z "$PUBKEY" ]; then
        print_error "Failed to extract public key from address"
        print_info "Address info: $(${BITCOIN_CLI} -regtest -rpcuser=${RPC_USER} -rpcpassword=${RPC_PASSWORD} -rpcport=${REGTEST_RPC_PORT} -datadir=${REGTEST_DATADIR} -rpcwallet=temp getaddressinfo ${ADDR} 2>&1)"
        if [ "$temp_node_started" = true ]; then
            print_info "Stopping temporary regtest node..."
            pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
        fi
        return 1
    fi
    
    print_info "Extracted public key: ${PUBKEY:0:20}..."
    
    # Extract private key from descriptor wallet
    # Note: dumpwallet only works with legacy wallets, so we'll use listdescriptors
    print_info "Extracting private key for signet block signing..."
    print_info "Using listdescriptors method (dumpwallet doesn't work with descriptor wallets)..."
    
    # Use listdescriptors to get xprv/tprv and derive key
    DESCRIPTORS=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=temp \
        listdescriptors true 2>&1)
    
    if echo "$DESCRIPTORS" | grep -qi "error"; then
        print_error "Failed to list descriptors: ${DESCRIPTORS}"
        if [ "$temp_node_started" = true ]; then
            print_info "Stopping temporary regtest node..."
            pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
        fi
        return 1
    fi
    
    # Get address info to find the keypath
    ADDR_INFO=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=temp \
        getaddressinfo ${ADDR} 2>&1)
    
    KEYPATH=$(echo "$ADDR_INFO" | grep -o '"hdkeypath": "[^"]*"' | cut -d'"' -f4)
    DESC_TYPE=$(echo "$ADDR_INFO" | grep -o '"desc": "[^"]*"' | cut -d'"' -f4 | sed 's/(.*//')
    
    print_info "Address keypath: ${KEYPATH}"
    print_info "Descriptor type: ${DESC_TYPE}"
    
    # Extract tprv/xprv from descriptor - handle both testnet (tprv) and mainnet (xprv)
    # Descriptor format: pkh(tprv.../path) or wpkh(tprv.../path)
    XPRV=$(echo "$DESCRIPTORS" | grep -oE '(tprv|xprv)[a-zA-Z0-9]{100,}' | head -1 || echo "")
    
    if [ -z "$XPRV" ]; then
        # Try extracting from descriptor string directly
        XPRV=$(echo "$DESCRIPTORS" | sed -n 's/.*\(tprv[a-zA-Z0-9]\{100,\}\).*/\1/p' | head -1 || echo "")
    fi
    
    if [ -z "$XPRV" ]; then
        print_error "Could not extract tprv/xprv from descriptors"
        print_info "Descriptor output (first 500 chars): ${DESCRIPTORS:0:500}"
        if [ "$temp_node_started" = true ]; then
            print_info "Stopping temporary regtest node..."
            pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
        fi
        return 1
    fi
    
    print_info "Extracted extended private key: ${XPRV:0:20}..."
    
    # Use the derive_privkey.py script to derive the private key
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    DERIVE_SCRIPT="${SCRIPT_DIR}/derive_privkey.py"
    
    PRIVKEY=""
    if [ -f "${DERIVE_SCRIPT}" ] && command -v python3 > /dev/null 2>&1; then
        print_info "Using derive_privkey.py to extract private key..."
        # Check if base58 is installed
        if python3 -c "import base58" 2>/dev/null; then
            PRIVKEY=$(python3 "${DERIVE_SCRIPT}" "${XPRV}" "${KEYPATH}" 2>&1)
            
            if [ -n "$PRIVKEY" ] && [ ${#PRIVKEY} -gt 20 ] && ! echo "$PRIVKEY" | grep -qi "error"; then
                print_success "Successfully derived private key using Python script"
            else
                print_error "Python script failed: ${PRIVKEY}"
                PRIVKEY=""
            fi
        else
            print_error "Python base58 library not installed"
            print_info "Attempting to install base58..."
            if command -v pip3 > /dev/null 2>&1; then
                pip3 install base58 > /dev/null 2>&1
                if python3 -c "import base58" 2>/dev/null; then
                    print_success "base58 installed successfully"
                    # Try again
                    PRIVKEY=$(python3 "${DERIVE_SCRIPT}" "${XPRV}" "${KEYPATH}" 2>&1)
                    if [ -n "$PRIVKEY" ] && [ ${#PRIVKEY} -gt 20 ] && ! echo "$PRIVKEY" | grep -qi "error"; then
                        print_success "Successfully derived private key after installing base58"
                    else
                        print_error "Still failed after installing base58: ${PRIVKEY}"
                        PRIVKEY=""
                    fi
                else
                    print_error "Failed to install base58 automatically"
                    print_info "Please install manually: pip install base58"
                    print_info "Falling back to descriptor import method (may not work for block signing)..."
                    PRIVKEY=""
                fi
            else
                print_error "pip3 not found, cannot install base58 automatically"
                print_info "Please install manually: pip install base58"
                print_info "Falling back to descriptor import method (may not work for block signing)..."
                PRIVKEY=""
            fi
        fi
    else
        print_info "derive_privkey.py not found or python3 not available"
        PRIVKEY=""
    fi
    
    # If Python method failed, save descriptor for import
    if [ -z "$PRIVKEY" ]; then
        print_info "Cannot derive WIF private key, will use descriptor import method..."
        
        # Get the parent descriptor (master xprv descriptor) from listdescriptors
        # This is better than the address-specific descriptor
        PARENT_DESC=$(echo "$DESCRIPTORS" | grep -o '"desc": "[^"]*tprv[^"]*"' | head -1 | cut -d'"' -f4 || echo "")
        
        if [ -z "$PARENT_DESC" ]; then
            # Fallback: Get the address descriptor
            ADDR_DESC=$(echo "$ADDR_INFO" | grep -o '"desc": "[^"]*"' | cut -d'"' -f4)
            if [ -n "$ADDR_DESC" ]; then
                print_info "Using address-specific descriptor (may be limited)"
                PARENT_DESC="$ADDR_DESC"
            fi
        else
            print_info "Found parent descriptor with master key"
        fi
        
        if [ -n "$PARENT_DESC" ]; then
            print_info "Saving descriptor for import into signet wallet"
            print_info "Descriptor: ${PARENT_DESC:0:80}..."
            
            # Save descriptor for later use in signet wallet
            mkdir -p "${SIGNET_DATADIR}"
            echo "${PARENT_DESC}" > "${SIGNET_DATADIR}/.signet_address_descriptor" 2>/dev/null || true
            
            # Set flag to use descriptor import
            PRIVKEY="DESCRIPTOR_IMPORT_NEEDED"
        else
            print_error "Could not find any descriptor with private key"
            if [ "$temp_node_started" = true ]; then
                print_info "Stopping temporary regtest node..."
                pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
            fi
            return 1
        fi
    fi
    
    if [ "$PRIVKEY" = "DESCRIPTOR_IMPORT_NEEDED" ]; then
        print_info "Using descriptor import method for signet"
        # We'll import the descriptor into signet wallet instead of using WIF private key
        # Save the address descriptor for later import
        ADDR_DESC=$(cat "${SIGNET_DATADIR}/.signet_address_descriptor" 2>/dev/null || echo "")
        if [ -z "$ADDR_DESC" ]; then
            print_error "Address descriptor not saved"
            print_error "Cannot proceed without private key or descriptor for signet block signing"
            if [ "$temp_node_started" = true ]; then
                print_info "Stopping temporary regtest node..."
                pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
            fi
            return 1
        fi
        # Save descriptor instead of private key
        mkdir -p ${SIGNET_DATADIR}
        echo "${ADDR_DESC}" > "${SIGNET_DATADIR}/.signet_descriptor"
        PRIVKEY=""  # No WIF key, will use descriptor
        print_info "Saved descriptor for signet block signing (will import into signet wallet)"
        print_warning "Note: Descriptor import method may not work for all signet block signing scenarios"
    else
        if [ -z "$PRIVKEY" ]; then
            print_error "Failed to extract private key and descriptor method also failed"
            if [ "$temp_node_started" = true ]; then
                print_info "Stopping temporary regtest node..."
                pkill -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null 2>&1 || true
            fi
            return 1
        fi
        print_info "Extracted private key: ${PRIVKEY:0:10}...${PRIVKEY: -10}"
    fi
    
    # Create challenge script (1-of-1 multisig: OP_1 <pubkey> OP_1 OP_CHECKMULTISIG)
    SIGNET_CHALLENGE="5121${PUBKEY}51ae"
    
    # Save challenge and private key/descriptor to files
    mkdir -p ${SIGNET_DATADIR}
    echo ${SIGNET_CHALLENGE} > ${SIGNET_CHALLENGE_FILE}
    if [ -n "$PRIVKEY" ] && [ "$PRIVKEY" != "DESCRIPTOR_IMPORT_NEEDED" ]; then
        echo ${PRIVKEY} > ${SIGNET_PRIVKEY_FILE}
        chmod 600 ${SIGNET_PRIVKEY_FILE}  # Secure the private key file
        print_success "Saved private key for signet block signing"
    else
        # No private key file, will use descriptor import
        rm -f ${SIGNET_PRIVKEY_FILE}
        print_info "Will use descriptor import for signet block signing"
    fi
    
    # Clean up temporary wallet (optional - we can leave it for debugging)
    # Try to unload the temporary wallet (it's okay if it fails)
    print_info "Cleaning up temporary wallet..."
    ${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        unloadwallet "temp" > /dev/null 2>&1 || true
    
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
    
    local wallet_exists=false
    if echo "$WALLET_LIST" | grep -q "\"${SIGNET_WALLET}\""; then
        print_info "Signet wallet already exists and is loaded"
        wallet_exists=true
    fi
    
    if [ "$wallet_exists" = false ]; then
        print_info "Creating signet wallet..."
        
        ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            createwallet ${SIGNET_WALLET} > /dev/null 2>&1
        
        if [ $? -ne 0 ]; then
            # Wallet might exist but not be loaded, try loading it
            print_info "Wallet may already exist, attempting to load..."
            ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${SIGNET_RPC_PORT} \
                -datadir=${SIGNET_DATADIR} \
                loadwallet ${SIGNET_WALLET} > /dev/null 2>&1
            if [ $? -ne 0 ]; then
                print_error "Failed to create or load signet wallet"
                return 1
            fi
        fi
    fi
    
    # Import the private key or descriptor for block signing (CRITICAL for signet)
    if [ -f "${SIGNET_PRIVKEY_FILE}" ]; then
        print_info "Importing private key for signet block signing..."
        PRIVKEY=$(cat ${SIGNET_PRIVKEY_FILE})
        
        # Check if key is already imported
        KEY_CHECK=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            importprivkey "${PRIVKEY}" "signet_mining_key" false 2>&1)
        
        if echo "$KEY_CHECK" | grep -qi "already exists\|already have"; then
            print_info "Private key already imported in wallet"
        elif [ $? -eq 0 ]; then
            print_success "Private key imported for block signing"
        else
            print_error "Failed to import private key: ${KEY_CHECK}"
            print_info "Continuing anyway - blocks may not be signable"
        fi
    elif [ -f "${SIGNET_DATADIR}/.signet_descriptor" ]; then
        print_info "Importing descriptor for signet block signing..."
        DESCRIPTOR=$(cat "${SIGNET_DATADIR}/.signet_descriptor")
        
        # Import the descriptor (which includes the private key)
        DESC_CHECK=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            importdescriptors "[{\"desc\":\"${DESCRIPTOR}\",\"timestamp\":\"now\",\"label\":\"signet_mining_key\"}]" 2>&1)
        
        if echo "$DESC_CHECK" | grep -qi '"success": true'; then
            print_success "Descriptor imported for block signing"
        elif echo "$DESC_CHECK" | grep -qi "already exists\|already have"; then
            print_info "Descriptor already imported in wallet"
        else
            print_error "Failed to import descriptor: ${DESC_CHECK}"
            print_error "WARNING: Descriptor import may not work for signet block signing!"
            print_error "Signet requires WIF private key import via importprivkey, not just descriptor"
            print_info "To fix this, install base58 and regenerate the challenge:"
            print_info "  pip install base58"
            print_info "  Then run option 1 to regenerate signet challenge"
            print_warning "Block signing may fail - signet needs the WIF private key"
            # Don't return error - let it continue but warn
        fi
    else
        print_error "Neither private key file nor descriptor file found!"
        print_error "Private key file: ${SIGNET_PRIVKEY_FILE}"
        print_error "Descriptor file: ${SIGNET_DATADIR}/.signet_descriptor"
        print_error "Signet blocks cannot be signed without the private key or descriptor!"
        print_info "You may need to regenerate the signet challenge"
        return 1
    fi
    
    if [ "$wallet_exists" = false ]; then
        print_success "Signet wallet created and loaded"
    else
        print_success "Signet wallet is ready"
    fi
}

# Create regtest wallet
create_regtest_wallet() {
    # Check if wallet already exists
    WALLET_LIST=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        listwallets 2>/dev/null)
    
    if echo "$WALLET_LIST" | grep -q "\"${REGTEST_WALLET}\""; then
        print_info "Regtest wallet already exists and is loaded"
        return 0
    fi
    
    print_info "Creating regtest wallet..."
    
    ${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        createwallet ${REGTEST_WALLET} > /dev/null 2>&1
    
    if [ $? -eq 0 ]; then
        print_success "Regtest wallet created and loaded"
    else
        # Wallet might exist but not be loaded, try loading it
        print_info "Wallet may already exist, attempting to load..."
        ${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            loadwallet ${REGTEST_WALLET} > /dev/null 2>&1
        if [ $? -eq 0 ]; then
            print_success "Regtest wallet loaded"
        else
            print_error "Failed to create or load regtest wallet"
            return 1
        fi
    fi
}

# Load regtest wallet (create if needed)
load_regtest_wallet() {
    # Wait a moment for RPC to be ready
    sleep 1
    
    # Check if wallet is already loaded
    WALLET_LIST=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        listwallets 2>/dev/null)
    
    if echo "$WALLET_LIST" | grep -q "\"${REGTEST_WALLET}\""; then
        print_info "Regtest wallet is already loaded"
        return 0
    fi
    
    # Wallet not loaded, try to load it
    print_info "Loading regtest wallet..."
    LOAD_RESULT=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        loadwallet ${REGTEST_WALLET} 2>&1)
    
    if [ $? -eq 0 ]; then
        print_success "Regtest wallet loaded"
        return 0
    fi
    
    # If loading failed, wallet might not exist, create it
    if echo "$LOAD_RESULT" | grep -qi "not found\|does not exist"; then
        print_info "Wallet not found, creating regtest wallet..."
        create_regtest_wallet
        return $?
    else
        print_error "Failed to load regtest wallet: $LOAD_RESULT"
        return 1
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

# Verify signet setup (challenge and private key/descriptor)
verify_signet_setup() {
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    # Check for either private key file OR descriptor file
    if [ -f "${SIGNET_PRIVKEY_FILE}" ]; then
        print_info "Signet challenge: ${SIGNET_CHALLENGE:0:40}..."
        print_info "Private key file exists"
        return 0
    elif [ -f "${SIGNET_DATADIR}/.signet_descriptor" ]; then
        print_info "Signet challenge: ${SIGNET_CHALLENGE:0:40}..."
        print_info "Descriptor file exists (will be used for block signing)"
        return 0
    else
        print_error "Neither private key file nor descriptor file found!"
        print_error "Private key file: ${SIGNET_PRIVKEY_FILE}"
        print_error "Descriptor file: ${SIGNET_DATADIR}/.signet_descriptor"
        print_error "Signet blocks cannot be signed without the private key or descriptor!"
        print_info "You may need to regenerate the signet challenge"
        return 1
    fi
}

# Mine blocks on signet
mine_signet() {
    local blocks=${1:-101}
    
    # Verify setup
    verify_signet_setup
    if [ $? -ne 0 ]; then
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
    
    # Verify private key or descriptor is available for block signing
    print_info "Verifying signing key is available for block signing..."
    if [ -f "${SIGNET_PRIVKEY_FILE}" ]; then
        print_info "Using private key file for block signing..."
        PRIVKEY=$(cat ${SIGNET_PRIVKEY_FILE})
        
        # Attempt to import the key (importprivkey handles duplicates gracefully)
        IMPORT_RESULT=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            importprivkey "${PRIVKEY}" "signet_mining_key" false 2>&1)
        
        if echo "$IMPORT_RESULT" | grep -qi "already exists\|already have"; then
            print_info "Private key already in wallet"
        elif [ $? -eq 0 ] && ! echo "$IMPORT_RESULT" | grep -qi "error"; then
            print_success "Private key imported into wallet"
        else
            print_info "Import result: ${IMPORT_RESULT}"
            print_info "Continuing - key may already be available"
        fi
    elif [ -f "${SIGNET_DATADIR}/.signet_descriptor" ]; then
        print_info "Using descriptor for block signing..."
        DESCRIPTOR=$(cat "${SIGNET_DATADIR}/.signet_descriptor")
        
        # Verify descriptor is imported in wallet
        DESC_CHECK=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            listdescriptors false 2>&1)
        
        # Check if descriptor is already imported (compare without checksum)
        DESC_WITHOUT_CHECKSUM=$(echo "$DESCRIPTOR" | cut -d'#' -f1)
        if echo "$DESC_CHECK" | grep -qF "$DESC_WITHOUT_CHECKSUM"; then
            print_info "Descriptor already imported in wallet"
        else
            print_info "Importing descriptor into wallet..."
            IMPORT_DESC=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${SIGNET_RPC_PORT} \
                -datadir=${SIGNET_DATADIR} \
                -rpcwallet=${SIGNET_WALLET} \
                importdescriptors "[{\"desc\":\"${DESCRIPTOR}\",\"timestamp\":\"now\",\"label\":\"signet_mining_key\"}]" 2>&1)
            
            if echo "$IMPORT_DESC" | grep -qi '"success": true'; then
                print_success "Descriptor imported into wallet"
            elif echo "$IMPORT_DESC" | grep -qi "already exists\|already have"; then
                print_info "Descriptor already imported in wallet"
            else
                print_error "Failed to import descriptor: ${IMPORT_DESC}"
                print_error "Signet blocks may not be signable!"
                print_info "The descriptor may need to be re-imported or the challenge regenerated"
                # Don't return error - let it try to mine anyway
            fi
        fi
        
        # Verify the descriptor can derive addresses (to ensure it's working)
        print_info "Verifying descriptor can derive addresses..."
        DERIVE_TEST=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            deriveaddresses "${DESC_WITHOUT_CHECKSUM}" "[0,0]" 2>&1)
        
        if echo "$DERIVE_TEST" | grep -q "tb1"; then
            print_info "Descriptor can derive addresses - should work for block signing"
        else
            print_warning "Could not verify descriptor address derivation: ${DERIVE_TEST}"
        fi
    else
        print_error "Neither private key file nor descriptor file found!"
        print_error "Cannot sign signet blocks without private key or descriptor!"
        return 1
    fi
    
    # Get address and show raw response
    print_info "Getting mining address..."
    ADDR_RESPONSE=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        -rpcwallet=${SIGNET_WALLET} \
        getnewaddress 2>&1)
    
    ADDR=$(echo "${ADDR_RESPONSE}" | head -1)
    
    if [ -z "$ADDR" ] || echo "${ADDR_RESPONSE}" | grep -qi "error"; then
        print_error "Failed to get new address. Raw response:"
        echo "${ADDR_RESPONSE}"
        return 1
    fi
    
    print_info "Mining to address: ${ADDR}"
    print_info "Raw getnewaddress response: ${ADDR_RESPONSE}"
    
    # Verify address is in wallet
    print_info "Verifying address is in wallet..."
    ADDR_INFO=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        -rpcwallet=${SIGNET_WALLET} \
        getaddressinfo ${ADDR} 2>&1)
    print_info "Address info: ${ADDR_INFO}"
    
    # Get list of addresses in wallet for comparison
    WALLET_ADDRESSES=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        -rpcwallet=${SIGNET_WALLET} \
        getaddressesbylabel "" 2>&1)
    print_info "Wallet addresses count: $(echo "${WALLET_ADDRESSES}" | grep -o '"' | wc -l)"
    
    # Check blockchain height before mining (without wallet context)
    print_info "Checking blockchain state before mining..."
    BLOCKCHAIN_INFO_BEFORE=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        getblockchaininfo 2>&1)
    HEIGHT_BEFORE=$(echo "${BLOCKCHAIN_INFO_BEFORE}" | grep -o '"blocks": [0-9]*' | grep -o '[0-9]*' || echo "0")
    BESTBLOCK_BEFORE=$(echo "${BLOCKCHAIN_INFO_BEFORE}" | grep -o '"bestblockhash": "[^"]*"' | cut -d'"' -f4)
    print_info "Blockchain height before: ${HEIGHT_BEFORE}"
    print_info "Best block hash before: ${BESTBLOCK_BEFORE}"
    
    # Check node configuration
    print_info "Checking signet node configuration..."
    NODE_INFO=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        getnetworkinfo 2>&1)
    print_info "Network info: $(echo "${NODE_INFO}" | head -5)"
    
    # Mine blocks and capture output
    # Note: generatetoaddress on signet doesn't need -rpcwallet, but we'll try both ways
    print_info "Executing generatetoaddress command..."
    print_info "Command: generatetoaddress ${blocks} ${ADDR}"
    
    # Try without -rpcwallet first (as per COMMANDS.md)
    GENERATE_OUTPUT=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        generatetoaddress ${blocks} ${ADDR} 2>&1)
    GENERATE_EXIT_CODE=$?
    
    # If that fails, try with -rpcwallet
    if [ $GENERATE_EXIT_CODE -ne 0 ] || [ -z "$GENERATE_OUTPUT" ] || echo "$GENERATE_OUTPUT" | grep -qi "error"; then
        print_info "First attempt failed, trying with -rpcwallet..."
        GENERATE_OUTPUT=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            generatetoaddress ${blocks} ${ADDR} 2>&1)
        GENERATE_EXIT_CODE=$?
    fi
    
    print_info "Generatetoaddress output: ${GENERATE_OUTPUT}"
    print_info "Generatetoaddress exit code: ${GENERATE_EXIT_CODE}"
    
    # Check blockchain height after mining (without wallet context)
    print_info "Checking blockchain state after mining..."
    sleep 1  # Brief pause to ensure blocks are processed
    BLOCKCHAIN_INFO_AFTER=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        getblockchaininfo 2>&1)
    HEIGHT_AFTER=$(echo "${BLOCKCHAIN_INFO_AFTER}" | grep -o '"blocks": [0-9]*' | grep -o '[0-9]*' || echo "0")
    BESTBLOCK_AFTER=$(echo "${BLOCKCHAIN_INFO_AFTER}" | grep -o '"bestblockhash": "[^"]*"' | cut -d'"' -f4)
    BLOCKS_ADDED=$((HEIGHT_AFTER - HEIGHT_BEFORE))
    print_info "Blockchain height after: ${HEIGHT_AFTER}"
    print_info "Best block hash after: ${BESTBLOCK_AFTER}"
    print_info "Blocks added: ${BLOCKS_ADDED}"
    
    # Also check if bestblock hash changed
    if [ "$BESTBLOCK_BEFORE" = "$BESTBLOCK_AFTER" ] && [ "$BLOCKS_ADDED" -eq 0 ]; then
        print_error "WARNING: Blockchain state did not change - blocks may not have been created!"
    fi
    
    if [ $GENERATE_EXIT_CODE -eq 0 ]; then
        # Check if blocks were actually created
        if [ -n "$HEIGHT_AFTER" ] && [ "$HEIGHT_AFTER" -gt "$HEIGHT_BEFORE" ]; then
            print_success "Mined ${blocks} blocks on signet (height: ${HEIGHT_BEFORE} -> ${HEIGHT_AFTER})"
        else
            print_error "generatetoaddress returned success but no blocks were created!"
            print_error "Height before: ${HEIGHT_BEFORE}, Height after: ${HEIGHT_AFTER}"
        fi
        
        # Wait a moment for wallet to update
        print_info "Waiting for wallet to update..."
        sleep 3
        
        # Try to rescan the wallet if balance is still zero
        print_info "Checking if wallet needs rescan..."
        WALLET_INFO=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            getwalletinfo 2>&1)
        print_info "Wallet info: ${WALLET_INFO}"
        
        # Get and dump raw balance responses
        print_info "Getting wallet balances..."
        
        # getbalance (simple)
        BALANCE_RESPONSE=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            getbalance 2>&1)
        print_info "Raw getbalance response: ${BALANCE_RESPONSE}"
        
        # getbalances (detailed)
        BALANCES_JSON=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            getbalances 2>&1)
        print_info "Raw getbalances response: ${BALANCES_JSON}"
        
        # Also check listunspent to see if there are any UTXOs
        UTXOS=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            listunspent 2>&1)
        print_info "Raw listunspent response: ${UTXOS}"
        
        # Check if address received any funds
        ADDR_BALANCE=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            getreceivedbyaddress ${ADDR} 0 2>&1)
        print_info "Balance for mining address ${ADDR}: ${ADDR_BALANCE}"
        
        # Also check the address directly (not through wallet)
        ADDR_TX_COUNT=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            scantxoutset start "[\"addr(${ADDR})\"]" 2>&1 | head -20)
        print_info "Direct address scan result: ${ADDR_TX_COUNT}"
        
        # If balance is still zero but blocks were created, try rescanning
        if [ -n "$HEIGHT_AFTER" ] && [ "$HEIGHT_AFTER" -gt "$HEIGHT_BEFORE" ]; then
            BALANCE_CHECK=$(echo "${BALANCE_RESPONSE}" | grep -oE '[0-9]+\.[0-9]+' | head -1)
            if [ -z "$BALANCE_CHECK" ] || [ "$(echo "$BALANCE_CHECK == 0" | bc 2>/dev/null || echo "1")" = "1" ]; then
                print_info "Balance is zero but blocks were created. Attempting wallet rescan..."
                ${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
                    -rpcuser=${RPC_USER} \
                    -rpcpassword=${RPC_PASSWORD} \
                    -rpcport=${SIGNET_RPC_PORT} \
                    -datadir=${SIGNET_DATADIR} \
                    -rpcwallet=${SIGNET_WALLET} \
                    rescanblockchain 0 > /dev/null 2>&1 &
                print_info "Rescan started in background (this may take a while)"
            fi
        fi
    else
        print_error "Failed to mine blocks (exit code: ${GENERATE_EXIT_CODE})"
        print_error "Error output: ${GENERATE_OUTPUT}"
        return 1
    fi
}

# Mine blocks on regtest
mine_regtest() {
    local blocks=${1:-101}
    
    # Check if regtest node is running
    if ! pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        print_error "Regtest node is not running. Please start it first (option 3)"
        return 1
    fi
    
    # Ensure wallet is loaded
    load_regtest_wallet
    if [ $? -ne 0 ]; then
        print_error "Failed to load regtest wallet"
        return 1
    fi
    
    print_info "Mining ${blocks} blocks on regtest..."
    
    # Get address
    print_info "Getting mining address..."
    ADDR_RESPONSE=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=${REGTEST_WALLET} \
        getnewaddress 2>&1)
    
    ADDR=$(echo "${ADDR_RESPONSE}" | head -1)
    
    if [ -z "$ADDR" ] || echo "${ADDR_RESPONSE}" | grep -qi "error"; then
        print_error "Failed to get new address. Raw response:"
        echo "${ADDR_RESPONSE}"
        return 1
    fi
    
    print_info "Mining to address: ${ADDR}"
    print_info "Raw getnewaddress response: ${ADDR_RESPONSE}"
    
    # Verify address is in wallet
    print_info "Verifying address is in wallet..."
    ADDR_INFO=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=${REGTEST_WALLET} \
        getaddressinfo ${ADDR} 2>&1)
    print_info "Address info: ${ADDR_INFO}"
    
    # Get list of addresses in wallet for comparison
    WALLET_ADDRESSES=$(${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=${REGTEST_WALLET} \
        getaddressesbylabel "" 2>&1)
    print_info "Wallet addresses count: $(echo "${WALLET_ADDRESSES}" | grep -o '"' | wc -l)"
    
    ${BITCOIN_CLI} -regtest \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${REGTEST_RPC_PORT} \
        -datadir=${REGTEST_DATADIR} \
        -rpcwallet=${REGTEST_WALLET} \
        generatetoaddress ${blocks} ${ADDR} > /dev/null
    
    if [ $? -eq 0 ]; then
        print_success "Mined ${blocks} blocks on regtest"
        
        # Wait a moment for wallet to update
        print_info "Waiting for wallet to update..."
        sleep 2
        
        # Get and dump raw balance responses
        print_info "Getting wallet balances..."
        
        # getbalance (simple)
        BALANCE_RESPONSE=$(${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            -rpcwallet=${REGTEST_WALLET} \
            getbalance 2>&1)
        print_info "Raw getbalance response: ${BALANCE_RESPONSE}"
        
        # getbalances (detailed)
        BALANCES_JSON=$(${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            -rpcwallet=${REGTEST_WALLET} \
            getbalances 2>&1)
        print_info "Raw getbalances response: ${BALANCES_JSON}"
        
        # Also check listunspent to see if there are any UTXOs
        UTXOS=$(${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            -rpcwallet=${REGTEST_WALLET} \
            listunspent 2>&1)
        print_info "Raw listunspent response: ${UTXOS}"
        
        # Check if address received any funds
        ADDR_BALANCE=$(${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            -rpcwallet=${REGTEST_WALLET} \
            getreceivedbyaddress ${ADDR} 0 2>&1)
        print_info "Balance for mining address ${ADDR}: ${ADDR_BALANCE}"
        
        # Also check the address directly (not through wallet)
        ADDR_TX_COUNT=$(${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            scantxoutset start "[\"addr(${ADDR})\"]" 2>&1 | head -20)
        print_info "Direct address scan result: ${ADDR_TX_COUNT}"
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

# Diagnostic function to check signet node and mining capability
diagnose_signet() {
    echo ""
    echo "=== Signet Diagnostic Information ==="
    
    load_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to load signet challenge"
        return 1
    fi
    
    # Check node status
    print_info "1. Checking signet node status..."
    if pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_success "Signet node process is running"
    else
        print_error "Signet node process is NOT running"
        return 1
    fi
    
    # Check RPC connectivity
    print_info "2. Checking RPC connectivity..."
    RPC_TEST=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        getblockchaininfo 2>&1)
    if echo "$RPC_TEST" | grep -q '"chain"'; then
        print_success "RPC is responding"
        CHAIN=$(echo "$RPC_TEST" | grep -o '"chain": "[^"]*"' | cut -d'"' -f4)
        HEIGHT=$(echo "$RPC_TEST" | grep -o '"blocks": [0-9]*' | grep -o '[0-9]*')
        print_info "   Chain: ${CHAIN}, Height: ${HEIGHT}"
    else
        print_error "RPC is not responding: ${RPC_TEST}"
        return 1
    fi
    
    # Check challenge
    print_info "3. Checking signet challenge..."
    print_info "   Challenge: ${SIGNET_CHALLENGE}"
    print_info "   Challenge file: ${SIGNET_CHALLENGE_FILE}"
    
    # Check private key
    print_info "4. Checking private key..."
    if [ -f "${SIGNET_PRIVKEY_FILE}" ]; then
        print_success "Private key file exists"
        PRIVKEY=$(cat ${SIGNET_PRIVKEY_FILE})
        print_info "   Private key: ${PRIVKEY:0:20}...${PRIVKEY: -10}"
    else
        print_error "Private key file NOT found: ${SIGNET_PRIVKEY_FILE}"
        print_error "   This is required for signet block signing!"
    fi
    
    # Check wallet
    print_info "5. Checking signet wallet..."
    load_signet_wallet > /dev/null 2>&1
    WALLET_LIST=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        listwallets 2>/dev/null)
    if echo "$WALLET_LIST" | grep -q "\"${SIGNET_WALLET}\""; then
        print_success "Signet wallet is loaded"
        
        # Check if private key is in wallet
        if [ -f "${SIGNET_PRIVKEY_FILE}" ]; then
            PRIVKEY=$(cat ${SIGNET_PRIVKEY_FILE})
            # Try to import and see what happens
            IMPORT_TEST=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${SIGNET_RPC_PORT} \
                -datadir=${SIGNET_DATADIR} \
                -rpcwallet=${SIGNET_WALLET} \
                importprivkey "${PRIVKEY}" "signet_mining_key" false 2>&1)
            if echo "$IMPORT_TEST" | grep -qi "already exists\|already have"; then
                print_success "Private key is already in wallet"
            elif [ $? -eq 0 ]; then
                print_success "Private key imported successfully"
            else
                print_error "Failed to import private key: ${IMPORT_TEST}"
            fi
        fi
    else
        print_error "Signet wallet is NOT loaded"
    fi
    
    # Check network info
    print_info "6. Checking network configuration..."
    NETWORK_INFO=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
        -rpcuser=${RPC_USER} \
        -rpcpassword=${RPC_PASSWORD} \
        -rpcport=${SIGNET_RPC_PORT} \
        -datadir=${SIGNET_DATADIR} \
        getnetworkinfo 2>&1 | head -10)
    print_info "   Network info: $(echo "${NETWORK_INFO}" | grep -o '"networkactive": [^,]*' || echo "N/A")"
    
    echo ""
    return 0
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

# Check balances for both networks
check_balances() {
    echo ""
    echo "=== Wallet Balances ==="
    
    # Check signet balance
    load_signet_challenge
    if pgrep -f "bitcoind.*signet.*${SIGNET_RPC_PORT}" > /dev/null; then
        print_info "Checking signet wallet balance..."
        load_signet_wallet > /dev/null 2>&1
        
        SIGNET_BALANCE=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${SIGNET_RPC_PORT} \
            -datadir=${SIGNET_DATADIR} \
            -rpcwallet=${SIGNET_WALLET} \
            getbalance 2>/dev/null)
        
        if [ -n "$SIGNET_BALANCE" ] && ! echo "$SIGNET_BALANCE" | grep -qi "error"; then
            print_success "Signet balance: ${SIGNET_BALANCE} BTC"
            
            # Get addresses in wallet (using listreceivedbyaddress)
            SIGNET_ADDRESSES=$(${BITCOIN_CLI} -signet -signetchallenge=${SIGNET_CHALLENGE} \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${SIGNET_RPC_PORT} \
                -datadir=${SIGNET_DATADIR} \
                -rpcwallet=${SIGNET_WALLET} \
                listreceivedbyaddress 0 true 2>/dev/null | head -10)
            if [ -n "$SIGNET_ADDRESSES" ]; then
                print_info "Signet wallet addresses with transactions:"
                echo "$SIGNET_ADDRESSES" | sed 's/^/  /'
            fi
        else
            print_error "Signet: Failed to get balance or wallet not loaded"
        fi
    else
        print_error "Signet node: Not running"
    fi
    
    echo ""
    
    # Check regtest balance
    if pgrep -f "bitcoind.*regtest.*${REGTEST_RPC_PORT}" > /dev/null; then
        print_info "Checking regtest wallet balance..."
        load_regtest_wallet > /dev/null 2>&1
        
        REGTEST_BALANCE=$(${BITCOIN_CLI} -regtest \
            -rpcuser=${RPC_USER} \
            -rpcpassword=${RPC_PASSWORD} \
            -rpcport=${REGTEST_RPC_PORT} \
            -datadir=${REGTEST_DATADIR} \
            -rpcwallet=${REGTEST_WALLET} \
            getbalance 2>/dev/null)
        
        if [ -n "$REGTEST_BALANCE" ] && ! echo "$REGTEST_BALANCE" | grep -qi "error"; then
            print_success "Regtest balance: ${REGTEST_BALANCE} BTC"
            
            # Get addresses in wallet (using listreceivedbyaddress)
            REGTEST_ADDRESSES=$(${BITCOIN_CLI} -regtest \
                -rpcuser=${RPC_USER} \
                -rpcpassword=${RPC_PASSWORD} \
                -rpcport=${REGTEST_RPC_PORT} \
                -datadir=${REGTEST_DATADIR} \
                -rpcwallet=${REGTEST_WALLET} \
                listreceivedbyaddress 0 true 2>/dev/null | head -10)
            if [ -n "$REGTEST_ADDRESSES" ]; then
                print_info "Regtest wallet addresses with transactions:"
                echo "$REGTEST_ADDRESSES" | sed 's/^/  /'
            fi
        else
            print_error "Regtest: Failed to get balance or wallet not loaded"
        fi
    else
        print_error "Regtest node: Not running"
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
    echo "13) Check balances (signet + regtest)"
    echo "14) Diagnose signet setup"
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
    
    # Generate challenge - STOP if this fails
    generate_signet_challenge
    if [ $? -ne 0 ]; then
        print_error "Failed to generate signet challenge. Cannot continue with full setup."
        print_error "Please fix the signet challenge generation issue first."
        return 1
    fi
    
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
    
    # Start nodes - STOP if signet fails
    print_info "Starting signet node..."
    start_signet
    if [ $? -ne 0 ]; then
        print_error "Failed to start signet node. Cannot continue with full setup."
        return 1
    fi
    
    print_info "Starting regtest node..."
    start_regtest
    if [ $? -ne 0 ]; then
        print_error "Failed to start regtest node. Cannot continue with full setup."
        return 1
    fi
    
    # Wait for nodes to be ready
    sleep 5
    
    # Create wallets - STOP if signet wallet fails
    print_info "Creating signet wallet..."
    create_signet_wallet
    if [ $? -ne 0 ]; then
        print_error "Failed to create signet wallet. Cannot continue with full setup."
        return 1
    fi
    
    print_info "Creating regtest wallet..."
    create_regtest_wallet
    if [ $? -ne 0 ]; then
        print_error "Failed to create regtest wallet. Cannot continue with full setup."
        return 1
    fi
    
    # Mine initial blocks - STOP if signet mining fails
    print_info "Mining blocks on signet..."
    mine_signet 101
    if [ $? -ne 0 ]; then
        print_error "Failed to mine blocks on signet. Signet setup incomplete."
        print_info "Regtest mining will still be attempted..."
    fi
    
    print_info "Mining blocks on regtest..."
    mine_regtest 101
    if [ $? -ne 0 ]; then
        print_error "Failed to mine blocks on regtest."
    fi
    
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
            13)
                check_balances
                ;;
            14)
                diagnose_signet
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