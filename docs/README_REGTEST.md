# Regtest Setup: Mainchain + Parentchain + 3 Coinshift Users

This setup gives you:

- **Two regtest chains**: one **mainchain** (BIP300 sidechain) and one **parentchain** (for swaps).
- **Three Coinshift instances** (User1, User2, User3), each with its own wallet and data.
- **Deposits** made on the mainchain and credited in all three Coinshift instances.
- **Client (GUI)** to view balances, swaps, and fulfill swaps.

## Prerequisites

- **bitcoin-patched** built (bitcoind, bitcoin-cli in `$BITCOIN_DIR` or path used in scripts).
- **bip300301_enforcer** built (used by `3_start_enforcer.sh`).
- **coinshift-rs** built: `cargo build --release` (or use `cargo run` in scripts).
- **grpcurl** (for enforcer gRPC): e.g. `go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest`.
- **jq** (optional, for parsing JSON in deposit script).

## Quick Start

Run in order (from the `docs/` folder):

```bash
cd docs

# 1. Mainchain regtest (RPC 18443, P2P 38333)
./1_start_mainchain.sh

# 2. Parentchain regtest (RPC 18444, P2P 38334)
./2_start_parentchain.sh

# 3. Enforcer (gRPC 50051) + sidechain proposal
./3_start_enforcer.sh
# If enforcer wallet doesn't exist: ./create_enforcer_wallet.sh then ./3_start_enforcer.sh --skip-proposal

# 4. Create enforcer wallet (if not done) and unlock
./create_enforcer_wallet.sh
./unlock_enforcer_wallet.sh ""

# 5. Start 3 Coinshift instances (User1, User2, User3)
./5_start_coinshift_users.sh

# 6. Deposit to all 3 Coinshift users (from mainchain wallet)
./6_deposit_all.sh
```

Then open the client (see [Opening the client](#opening-the-client)).

## Scripts Overview

| Script | Purpose |
|--------|--------|
| `1_start_mainchain.sh` | Start mainchain regtest (deposits/withdrawals). |
| `2_start_parentchain.sh` | Start parentchain regtest (swap payments). |
| `3_start_enforcer.sh` | Start enforcer + create/wait for sidechain proposal. |
| `create_enforcer_wallet.sh` | Create enforcer mainchain wallet. |
| `unlock_enforcer_wallet.sh` | Unlock enforcer wallet. |
| `fund_enforcer_wallet.sh` | Send mainchain coins to enforcer wallet (optional). |
| `4_mine_blocks.sh` | Mine blocks on mainchain and/or parentchain. |
| `mine_with_enforcer.sh` | Mine mainchain via enforcer gRPC. |
| `5_start_coinshift_users.sh` | Start 3 Coinshift instances (User1/2/3). |
| `6_deposit_all.sh` | Deposit from mainchain to all 3 Coinshift users. |
| `7_open_client.sh` | Open Coinshift GUI for a chosen user (1, 2, or 3). |
| `8_stop_all.sh` | Stop mainchain, parentchain, enforcer, and Coinshift processes. |

## Ports and data dirs

- **Mainchain**: RPC 18443, P2P 38333, ZMQ 29000–29004, datadir `coinshift-mainchain-data`.
- **Parentchain**: RPC 18444, P2P 38334, datadir `coinshift-parentchain-data`.
- **Enforcer**: gRPC 50051.
- **Coinshift User1**: RPC 6255, net 4255, datadir `coinshift-user1`.
- **Coinshift User2**: RPC 6256, net 4256, datadir `coinshift-user2`.
- **Coinshift User3**: RPC 6257, net 4257, datadir `coinshift-user3`.

Paths are under `$PROJECT_ROOT` (default `/home/parallels/Projects`). Override if needed:

```bash
export PROJECT_ROOT="/path/to/your/projects"
```

## Opening the client

Each Coinshift user has its own data directory and RPC port. To use the GUI for a specific user:

```bash
# User 1 (default-like)
./7_open_client.sh 1

# User 2
./7_open_client.sh 2

# User 3
./7_open_client.sh 3
```

Or manually:

```bash
# From repo root
cargo run --bin coinshift_app -- -d /home/parallels/Projects/coinshift-user1 --rpc-addr 127.0.0.1:6255 --net-addr 0.0.0.0:4255
```

**L1 (Regtest) for swaps**: In the GUI, open **L1 Config**, choose **Regtest**, and set:

- **RPC URL**: `http://127.0.0.1:18444`
- **User**: `user`
- **Password**: `passwordDC`

Save. This lets Coinshift watch the parentchain for swap payments.

## Viewing details and fulfilling swaps

1. **Balances**: In the client, check the balance view (L2 balance after deposits).
2. **Swaps**: Use the Swaps section to create an offer (L2 → L1) or to fulfill/claim swaps.
3. **Create swap (e.g. User1)**: Create swap, set L1 recipient (e.g. a parentchain address), L1/L2 amounts, and required confirmations (e.g. 1 for regtest).
4. **Fulfill swap**: Another user sends the correct amount to the swap’s L1 address on the **parentchain** (port 18444). After confirmations, the swap becomes ready to claim.
5. **Claim**: In the client, claim the swap (optionally specifying L2 recipient for open swaps).

Mine blocks when needed:

```bash
./4_mine_blocks.sh both 1    # one block on mainchain and parentchain
./mine_with_enforcer.sh 1    # one mainchain block via enforcer
```

## Stopping everything

```bash
# Stop Coinshift instances
pkill -f "coinshift_app.*coinshift-user" || true

# Stop enforcer
pkill -f bip300301_enforcer || true

# Stop both regtest nodes
"$BITCOIN_CLI" -regtest -rpcuser=user -rpcpassword=passwordDC -rpcport=18443 -datadir="$MAINCHAIN_DATADIR" stop || true
"$BITCOIN_CLI" -regtest -rpcuser=user -rpcpassword=passwordDC -rpcport=18444 -datadir="$PARENTCHAIN_DATADIR" stop || true
```

Or run `./8_stop_all.sh`.

## Troubleshooting

- **Port in use**: Stop existing bitcoind/coinshift/enforcer or change ports in the scripts.
- **Enforcer “wallet not found”**: Run `./create_enforcer_wallet.sh`, then `./3_start_enforcer.sh --skip-proposal`.
- **Coinshift “cannot connect to enforcer”**: Start mainchain and enforcer first; Coinshift uses `--mainchain-grpc-url http://127.0.0.1:50051`.
- **Swap not updating**: Ensure L1 Config has Regtest set to `http://127.0.0.1:18444` and mine a block on mainchain after the parentchain payment so 2WPD can run.
