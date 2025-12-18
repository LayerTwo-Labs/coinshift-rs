# Setup Script Commands - Run Separately

## Configuration Variables
```bash
PROJECT_ROOT="/home/parallels/Projects"
BITCOIN_DIR="${PROJECT_ROOT}/bitcoin-patched/build/bin"
SIGNET_DATADIR="${PROJECT_ROOT}/coinshift-signet-data"
REGTEST_DATADIR="${PROJECT_ROOT}/coinshift-regtest-data"
ENFORCER="${PROJECT_ROOT}/bip300301_enforcer/target/debug/bip300301_enforcer"
BITCOIND="${BITCOIN_DIR}/bitcoind"
BITCOIN_CLI="${BITCOIN_DIR}/bitcoin-cli"

RPC_USER="user"
RPC_PASSWORD="passwordDC"
SIGNET_RPC_PORT=18443
REGTEST_RPC_PORT=18444
SIGNET_WALLET="signetwallet"
REGTEST_WALLET="regtestwallet"

ZMQ_SEQUENCE="tcp://0.0.0.0:29000"
ZMQ_HASHBLOCK="tcp://0.0.0.0:29001"
ZMQ_HASHTX="tcp://0.0.0.0:29002"
ZMQ_RAWBLOCK="tcp://0.0.0.0:29003"
ZMQ_RAWTX="tcp://0.0.0.0:29004"

SIGNET_CHALLENGE_FILE="${SIGNET_DATADIR}/.signet_challenge"
```

## 1. Create Data Directories
```bash
mkdir -p /home/parallels/Projects/coinshift-signet-data
mkdir -p /home/parallels/Projects/coinshift-regtest-data
```

## 2. Generate Signet Challenge

### Step 2a: Start temporary regtest node (if not running)
```bash
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoind -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -daemon \
  -datadir=/home/parallels/Projects/coinshift-regtest-data
sleep 3
```

### Step 2b: Create temporary wallet
```bash
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  createwallet "temp"
```

### Step 2c: Generate address and get public key
```bash
ADDR=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  getnewaddress)

PUBKEY=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  getaddressinfo ${ADDR} | grep -o '"pubkey": "[^"]*"' | cut -d'"' -f4)
```

### Step 2d: Create and save signet challenge
```bash
SIGNET_CHALLENGE="5121${PUBKEY}51ae"
mkdir -p /home/parallels/Projects/coinshift-signet-data
echo ${SIGNET_CHALLENGE} > /home/parallels/Projects/coinshift-signet-data/.signet_challenge
```

### Step 2e: Load existing challenge (if already generated)
```bash
SIGNET_CHALLENGE=$(cat /home/parallels/Projects/coinshift-signet-data/.signet_challenge)
```

## 3. Start Signet Node
```bash
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoind -signet -noconnect \
  -signetchallenge=${SIGNET_CHALLENGE} \
  -fallbackfee=0.0002 \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18443 \
  -server -txindex -rest \
  -zmqpubsequence=tcp://0.0.0.0:29000 \
  -zmqpubhashblock=tcp://0.0.0.0:29001 \
  -zmqpubhashtx=tcp://0.0.0.0:29002 \
  -zmqpubrawblock=tcp://0.0.0.0:29003 \
  -zmqpubrawtx=tcp://0.0.0.0:29004 \
  -listen -port=38333 \
  -datadir=/home/parallels/Projects/coinshift-signet-data &

sleep 3
```

## 4. Start Regtest Node
```bash
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoind -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -server -txindex -rest \
  -zmqpubsequence=tcp://0.0.0.0:29000 \
  -zmqpubhashblock=tcp://0.0.0.0:29001 \
  -zmqpubhashtx=tcp://0.0.0.0:29002 \
  -zmqpubrawblock=tcp://0.0.0.0:29003 \
  -zmqpubrawtx=tcp://0.0.0.0:29004 \
  -listen \
  -datadir=/home/parallels/Projects/coinshift-regtest-data &

sleep 3
```

## 5. Create Signet Wallet
```bash
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -signet \
  -signetchallenge=${SIGNET_CHALLENGE} \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18443 \
  -datadir=/home/parallels/Projects/coinshift-signet-data \
  createwallet signetwallet
```

## 6. Create Regtest Wallet
```bash
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  createwallet regtestwallet
```

## 7. Mine Blocks on Signet
```bash
# Get address first
ADDR=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -signet \
  -signetchallenge=${SIGNET_CHALLENGE} \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18443 \
  -datadir=/home/parallels/Projects/coinshift-signet-data \
  getnewaddress)

# Mine blocks (replace 101 with desired number)
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -signet \
  -signetchallenge=${SIGNET_CHALLENGE} \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18443 \
  -datadir=/home/parallels/Projects/coinshift-signet-data \
  generatetoaddress 101 ${ADDR}
```

## 8. Mine Blocks on Regtest
```bash
# Get address first
ADDR=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  getnewaddress)

# Mine blocks (replace 101 with desired number)
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user \
  -rpcpassword=passwordDC \
  -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  generatetoaddress 101 ${ADDR}
```

## 9. Start Enforcer
```bash
/home/parallels/Projects/bip300301_enforcer/target/debug/bip300301_enforcer \
  --node-rpc-addr=127.0.0.1:18443 \
  --node-rpc-user=user \
  --node-rpc-pass=passwordDC \
  --node-zmq-addr-sequence=tcp://0.0.0.0:29000 \
  --enable-wallet \
  --wallet-sync-source=disabled &

sleep 2
```

## 10. Stop All Services
```bash
# Stop enforcer
pkill -f "bip300301_enforcer"

# Stop signet node
pkill -f "bitcoind.*signet.*18443"

# Stop regtest node
pkill -f "bitcoind.*regtest.*18444"
```

## 11. Check Status
```bash
# Check signet node
pgrep -f "bitcoind.*signet.*18443" && echo "Signet node: Running" || echo "Signet node: Not running"

# Check regtest node
pgrep -f "bitcoind.*regtest.*18444" && echo "Regtest node: Running" || echo "Regtest node: Not running"

# Check enforcer
pgrep -f "bip300301_enforcer" && echo "Enforcer: Running" || echo "Enforcer: Not running"
```

## Full Setup Sequence (All Commands in Order)
```bash
# 1. Create directories
mkdir -p /home/parallels/Projects/coinshift-signet-data
mkdir -p /home/parallels/Projects/coinshift-regtest-data

# 2. Generate signet challenge (if not exists)
if [ ! -f /home/parallels/Projects/coinshift-signet-data/.signet_challenge ]; then
  # Start temp regtest
  /home/parallels/Projects/bitcoin-patched/build/bin/bitcoind -regtest \
    -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
    -daemon -datadir=/home/parallels/Projects/coinshift-regtest-data
  sleep 3
  
  # Create temp wallet
  /home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
    -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
    -datadir=/home/parallels/Projects/coinshift-regtest-data createwallet "temp"
  
  # Get pubkey
  ADDR=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
    -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
    -datadir=/home/parallels/Projects/coinshift-regtest-data getnewaddress)
  PUBKEY=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
    -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
    -datadir=/home/parallels/Projects/coinshift-regtest-data \
    getaddressinfo ${ADDR} | grep -o '"pubkey": "[^"]*"' | cut -d'"' -f4)
  
  # Save challenge
  echo "5121${PUBKEY}51ae" > /home/parallels/Projects/coinshift-signet-data/.signet_challenge
fi

# 3. Load challenge
SIGNET_CHALLENGE=$(cat /home/parallels/Projects/coinshift-signet-data/.signet_challenge)

# 4. Start signet node
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoind -signet -noconnect \
  -signetchallenge=${SIGNET_CHALLENGE} -fallbackfee=0.0002 \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18443 \
  -server -txindex -rest \
  -zmqpubsequence=tcp://0.0.0.0:29000 \
  -zmqpubhashblock=tcp://0.0.0.0:29001 \
  -zmqpubhashtx=tcp://0.0.0.0:29002 \
  -zmqpubrawblock=tcp://0.0.0.0:29003 \
  -zmqpubrawtx=tcp://0.0.0.0:29004 \
  -listen -port=38333 \
  -datadir=/home/parallels/Projects/coinshift-signet-data &

# 5. Start regtest node
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoind -regtest \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
  -server -txindex -rest \
  -zmqpubsequence=tcp://0.0.0.0:29000 \
  -zmqpubhashblock=tcp://0.0.0.0:29001 \
  -zmqpubhashtx=tcp://0.0.0.0:29002 \
  -zmqpubrawblock=tcp://0.0.0.0:29003 \
  -zmqpubrawtx=tcp://0.0.0.0:29004 \
  -listen \
  -datadir=/home/parallels/Projects/coinshift-regtest-data &

# 6. Wait for nodes to be ready
sleep 5

# 7. Create wallets
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -signet \
  -signetchallenge=${SIGNET_CHALLENGE} \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18443 \
  -datadir=/home/parallels/Projects/coinshift-signet-data \
  createwallet signetwallet

/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  createwallet regtestwallet

# 8. Mine initial blocks
ADDR_SIGNET=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -signet \
  -signetchallenge=${SIGNET_CHALLENGE} \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18443 \
  -datadir=/home/parallels/Projects/coinshift-signet-data getnewaddress)
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -signet \
  -signetchallenge=${SIGNET_CHALLENGE} \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18443 \
  -datadir=/home/parallels/Projects/coinshift-signet-data \
  generatetoaddress 101 ${ADDR_SIGNET}

ADDR_REGTEST=$(/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data getnewaddress)
/home/parallels/Projects/bitcoin-patched/build/bin/bitcoin-cli -regtest \
  -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 \
  -datadir=/home/parallels/Projects/coinshift-regtest-data \
  generatetoaddress 101 ${ADDR_REGTEST}
```
