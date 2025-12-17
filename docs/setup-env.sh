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

rm -rf /Users/rob/Library/Application\ Support/bip300301_enforcer
rm -rf "$SIGNET_DATADIR" "$REGTEST_DATADIR"
mkdir -p "$SIGNET_DATADIR" "$REGTEST_DATADIR"

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

"$BITCOIN_CLI" -signet -rpcwait -signetchallenge="$SIGNET_CHALLENGE" \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$SIGNET_RPC_PORT" \
  -datadir="$SIGNET_DATADIR" \
  createwallet "$SIGNET_WALLET" 


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

"$BITCOIND" -regtest \
  -rpcuser="$RPC_USER" \
  -rpcpassword="$RPC_PASSWORD" \
  -rpcport="$REGTEST_RPC_PORT" \
  -server -txindex -rest \
  -listen -port="$REGTEST_P2P_PORT" \
  -datadir="$REGTEST_DATADIR" \
  -daemon

"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  createwallet "$REGTEST_WALLET" 

ADDR=$("$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  -rpcwallet="$REGTEST_WALLET" \
  getnewaddress)

"$BITCOIN_CLI" -regtest -rpcwait \
  -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASSWORD" -rpcport="$REGTEST_RPC_PORT" \
  -datadir="$REGTEST_DATADIR" \
  generatetoaddress 101 "$ADDR"

"$ENFORCER" \
  --node-rpc-addr=127.0.0.1:"$SIGNET_RPC_PORT" \
  --node-rpc-user="$RPC_USER" \
  --node-rpc-pass="$RPC_PASSWORD" \
  --node-zmq-addr-sequence="$ZMQ_SEQUENCE" \
  --enable-wallet \
  --wallet-sync-source=disabled


