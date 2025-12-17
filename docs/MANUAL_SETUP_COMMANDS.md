## Manual setup (signet + regtest + enforcer, descriptor wallets)

This file is a **copy/paste** guide for bringing up:
- a **local custom signet** (and mining on it) using **descriptor wallets**
- an optional **regtest** node (useful for local testing)
- the **bip300301 enforcer** (connected to the signet node via RPC/ZMQ)

Notes:
- This guide uses **descriptor wallets only**.
- This guide avoids legacy/BDB wallet usage (no `-deprecatedrpc=create_bdb`). For the **1-of-1 multisig custom signet challenge** option below, we use `dumpprivkey` to feed the signet miner a block-signing key.

---

### 0) Set environment variables (paste once per terminal)

```bash
export BITCOIN_DIR="/Users/rob/projects/layertwolabs/bitcoin-patched/build/bin"
export BITCOIND="$BITCOIN_DIR/bitcoind"
export BITCOIN_CLI="$BITCOIN_DIR/bitcoin-cli"
export ENFORCER="/Users/rob/projects/layertwolabs/bip300301_enforcer/target/debug/bip300301_enforcer"
export BITCOIN_UTIL="${BITCOIN_UTIL:-$BITCOIN_DIR/bitcoin-util}"
export SIGNET_MINER="${SIGNET_MINER:-/Users/rob/projects/layertwolabs/bitcoin-patched/contrib/signet/miner}"
export RPC_USER="user"
export RPC_PASSWORD="passwordDC"
export SIGNET_RPC_PORT="18443"
export SIGNET_P2P_PORT="38333"
export SIGNET_DATADIR="/Users/rob/projects/layertwolabs/coinshift-signet-data"
export SIGNET_WALLET="signetwallet"
export REGTEST_RPC_PORT="18444"
export REGTEST_P2P_PORT="18445"
export REGTEST_DATADIR="/Users/rob/projects/layertwolabs/coinshift-regtest-data"
export REGTEST_WALLET="regtestwallet"
export ENFORCER_GRPC_PORT="50051"
export ENFORCER_GRPC_ADDR="127.0.0.1:$ENFORCER_GRPC_PORT"
export ENFORCER_GRPC_URL="http://$ENFORCER_GRPC_ADDR"
export ZMQ_SEQUENCE="tcp://127.0.0.1:29000"
export ZMQ_HASHBLOCK="tcp://127.0.0.1:29001"
export ZMQ_HASHTX="tcp://127.0.0.1:29002"
export ZMQ_RAWBLOCK="tcp://127.0.0.1:29003"
export ZMQ_RAWTX="tcp://127.0.0.1:29004"
export SIGNET_CHALLENGE_FILE="$SIGNET_DATADIR/.signet_challenge"
export SIGNET_PRIVKEY_FILE="$SIGNET_DATADIR/.signet_privkey"
```

Create the datadir:

```bash
mkdir -p "$SIGNET_DATADIR" "$REGTEST_DATADIR"
```

---

### 1) Choose a signet challenge (local mining)

For local development where you want to mine blocks yourself, start with the **trivial** challenge (anyone-can-mine). If you want the integration-test style **1-of-1 multisig** challenge, do that later in **step 3b** (no jumping around).

```bash
# OP_TRUE (anyone-can-mine). Good for local dev.
export SIGNET_CHALLENGE="51"
echo "$SIGNET_CHALLENGE" > "$SIGNET_CHALLENGE_FILE"
```

If you already have a challenge file, load it:

```bash
export SIGNET_CHALLENGE="$(cat "$SIGNET_CHALLENGE_FILE")"
echo "SIGNET_CHALLENGE=$SIGNET_CHALLENGE"
```

---

### 2) Start signet (noconnect + custom challenge)

```bash
# Sanity defaults (helps on clean shells / zsh)
: "${SIGNET_DATADIR:?SIGNET_DATADIR is not set (run step 0)}"
: "${SIGNET_CHALLENGE_FILE:="$SIGNET_DATADIR/.signet_challenge"}"
mkdir -p "$SIGNET_DATADIR"

# Load persisted challenge into the environment.
# If this is a clean setup and the file doesn't exist yet, default to OP_TRUE.
if [ -f "$SIGNET_CHALLENGE_FILE" ]; then
  export SIGNET_CHALLENGE="$(cat "$SIGNET_CHALLENGE_FILE")"
else
  export SIGNET_CHALLENGE="51"
  echo "$SIGNET_CHALLENGE" > "$SIGNET_CHALLENGE_FILE"
fi
echo "SIGNET_CHALLENGE=$SIGNET_CHALLENGE"

# Stop an already-running signet node (ignore errors if it's not running)
"$BITCOIN_CLI" -signet -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  stop || true

"$BITCOIND" -signet -noconnect \
  -signetchallenge="$SIGNET_CHALLENGE" \
  -fallbackfee=0.0002 \
  -rpcuser="$RPC_USER" \
  -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$SIGNET_RPC_PORT" \
  -server -txindex -rest \
  -zmqpubsequence="$ZMQ_SEQUENCE" \
  -zmqpubhashblock="$ZMQ_HASHBLOCK" \
  -zmqpubhashtx="$ZMQ_HASHTX" \
  -zmqpubrawblock="$ZMQ_RAWBLOCK" \
  -zmqpubrawtx="$ZMQ_RAWTX" \
  -listen -port="$SIGNET_P2P_PORT" \
  -datadir="$SIGNET_DATADIR" \
  -daemon
```

Wait until RPC is ready:

```bash
"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  getblockchaininfo
```

---

### 3) Create a descriptor wallet (signet)

```bash
# Descriptor wallet (default in modern Core)
"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  createwallet "$SIGNET_WALLET" || true
```

---

### 3b) (Optional) Switch to the integration-test style 1-of-1 multisig signet challenge

This matches how the `bip300301_enforcer` integration tests do custom signets: `5121<PUBKEY>51ae` (1-of-1 multisig) plus a private key used by the signet miner to sign blocks.

This step is **self-contained** (it generates the key material, stops signet, writes the new challenge, and restarts signet).

```bash
BOOTSTRAP_SIGNET_CHALLENGE="$(cat "$SIGNET_CHALLENGE_FILE")"

# Generate key material in the signet wallet (while signet is still running)
ADDR=$("$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$BOOTSTRAP_SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  -rpcwallet="$SIGNET_WALLET" \
  getnewaddress)

PUBKEY=$("$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$BOOTSTRAP_SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  -rpcwallet="$SIGNET_WALLET" \
  getaddressinfo "$ADDR" | python3 -c 'import sys,json; print(json.load(sys.stdin)["pubkey"])')

PRIVKEY=$("$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$BOOTSTRAP_SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  -rpcwallet="$SIGNET_WALLET" \
  dumpprivkey "$ADDR")

# Stop signet (must use the bootstrap challenge to talk to the currently-running node)
"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$BOOTSTRAP_SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  stop || true

# Write the new challenge + signing key
export SIGNET_CHALLENGE="5121${PUBKEY}51ae"
echo "$SIGNET_CHALLENGE" > "$SIGNET_CHALLENGE_FILE"
echo "$PRIVKEY" > "$SIGNET_PRIVKEY_FILE"

# Start signet again with the new challenge
"$BITCOIND" -signet -noconnect \
  -signetchallenge="$SIGNET_CHALLENGE" \
  -fallbackfee=0.0002 \
  -rpcuser="$RPC_USER" \
  -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$SIGNET_RPC_PORT" \
  -server -txindex -rest \
  -zmqpubsequence="$ZMQ_SEQUENCE" \
  -zmqpubhashblock="$ZMQ_HASHBLOCK" \
  -zmqpubhashtx="$ZMQ_HASHTX" \
  -zmqpubrawblock="$ZMQ_RAWBLOCK" \
  -zmqpubrawtx="$ZMQ_RAWTX" \
  -listen -port="$SIGNET_P2P_PORT" \
  -datadir="$SIGNET_DATADIR" \
  -daemon

# Reload SIGNET_CHALLENGE for the next steps
export SIGNET_CHALLENGE="$(cat "$SIGNET_CHALLENGE_FILE")"
echo "SIGNET_CHALLENGE=$SIGNET_CHALLENGE"
```

---

### 4) Mine initial blocks

Mine 101 blocks (matures coinbase).

If you are using the **trivial** challenge (OP_TRUE), `generatetoaddress` should work:

```bash
ADDR=$("$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  -rpcwallet="$SIGNET_WALLET" \
  getnewaddress)

"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  generatetoaddress 101 "$ADDR"
```

If `generatetoaddress` doesn’t work for your build (or you switched to the **1-of-1 multisig** challenge in step 3b), use the signet miner:

```bash
NBITS_DEFAULT="1e0377ae"
NBITS="${NBITS:-$NBITS_DEFAULT}"

ADDR=$("$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  -rpcwallet="$SIGNET_WALLET" \
  getnewaddress)

"$SIGNET_MINER" \
  --debug \
  --cli="$BITCOIN_CLI -signet -rpcwait -signetchallenge=$SIGNET_CHALLENGE -rpcuser=$RPC_USER -rpcpassword=$RPC_PASSWORD -rpcport=$SIGNET_RPC_PORT -datadir=$SIGNET_DATADIR -rpcwallet=$SIGNET_WALLET" \
  generate \
  --grind-cmd="$BITCOIN_UTIL grind" \
  --address="$ADDR" \
  --nbits="$NBITS" \
  --set-block-time="$(date +%s)"
```

If you switched to the **1-of-1 multisig** challenge in step 3b, pass the private key so the miner can sign blocks:

```bash
"$SIGNET_MINER" \
  --debug \
  --cli="$BITCOIN_CLI -signet -rpcwait -signetchallenge=$SIGNET_CHALLENGE -rpcuser=$RPC_USER -rpcpassword=$RPC_PASSWORD -rpcport=$SIGNET_RPC_PORT -datadir=$SIGNET_DATADIR -rpcwallet=$SIGNET_WALLET" \
  generate \
  --grind-cmd="$BITCOIN_UTIL grind" \
  --address="$ADDR" \
  --nbits="$NBITS" \
  --set-block-time="$(date +%s)" \
  --signet-key="$(cat "$SIGNET_PRIVKEY_FILE")"
```

---

### 5) Basic status checks

```bash
"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  getblockchaininfo

"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  getwalletinfo
```

---

### 6) Start regtest (optional)

```bash
# Stop an already-running regtest node (ignore errors if it's not running)
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  stop || true

"$BITCOIND" -regtest \
  -rpcuser="$RPC_USER" \
  -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$REGTEST_RPC_PORT" \
  -server -txindex -rest \
  -listen -port="$REGTEST_P2P_PORT" \
  -datadir="$REGTEST_DATADIR" \
  -daemon
```

Wait until RPC is ready:

```bash
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  getblockchaininfo
```

Create a descriptor wallet (regtest):

```bash
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  createwallet "$REGTEST_WALLET" || true
```

Mine 101 blocks on regtest:

```bash
ADDR=$("$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  -rpcwallet="$REGTEST_WALLET" \
  getnewaddress)

"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  generatetoaddress 101 "$ADDR"
```

---

### 7) Start the enforcer (for signet)

This assumes **signet is running** (step 2). The enforcer connects to the signet node via RPC and ZMQ.

```bash
"$ENFORCER" \
  --node-rpc-addr=127.0.0.1:"$SIGNET_RPC_PORT" \
  --node-rpc-user="$RPC_USER" \
  --node-rpc-pass="$RPC_PASSWORD" \
  --node-zmq-addr-sequence="$ZMQ_SEQUENCE" \
  --serve-grpc-addr "$ENFORCER_GRPC_ADDR" \
  --enable-wallet \
  --wallet-sync-source=disabled
```

---

### 7b) Create the sidechain proposal (gRPC)

Submit the proposal payload in `docs/create_sidechain_proposal.json` to the enforcer's gRPC server.

Install `grpcurl` (macOS):

```bash
brew install grpcurl
```

Optional: confirm the gRPC service/method exists (uses server reflection):

```bash
grpcurl -plaintext "$ENFORCER_GRPC_ADDR" list | grep -Ei 'cusf\.mainchain\.v1|WalletService|proposal'
grpcurl -plaintext "$ENFORCER_GRPC_ADDR" describe cusf.mainchain.v1.WalletService
```

Submit the proposal:

```bash
grpcurl -plaintext -d @ "$ENFORCER_GRPC_ADDR" \
  cusf.mainchain.v1.WalletService/CreateSidechainProposal \
  < docs/create_sidechain_proposal.json
```

### 8) Stop everything

```bash
# Stop signet
"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  stop || true

# Stop regtest
"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  stop || true

# Stop enforcer (best-effort)
pkill -f "bip300301_enforcer" || true
```

```bash
"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  stop || true
```

