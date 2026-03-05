# Coinshift

## Building

Check out the repo with `git clone`, and then

```bash
$ git submodule update --init
$ cargo build
```

## Running

```bash
# Starts the RPC-API server
$ cargo run --bin coinshift_app -- --headless

# Runs the CLI, for interacting with the JSON-RPC server
$ cargo run --bin coinshift_app_cli

# Runs the user interface. Includes an embedded 
# version of the JSON-RPC server. 
$ cargo run --bin coinshift_app -- --headless
```

## Running multiple instances

You can run two (or more) Coinshift instances on the same machine by giving each instance its own **data directory**, **RPC address**, and **P2P (net) address** so they don’t conflict.

1. **Data directory** (`--datadir` / `-d`) — Use a separate datadir per instance so wallets and chain data don’t clash (e.g. `--datadir /path/to/coinshift2` for the second instance).
2. **RPC address** (`--rpc-addr` / `-r`) — Default is `127.0.0.1:6255`. Use a different port for the second instance (e.g. `127.0.0.1:6256`).
3. **P2P address** (`--net-addr` / `-n`) — Default is `0.0.0.0:4255`. Use a different port for the second instance (e.g. `0.0.0.0:4256`).
4. **Mainchain gRPC** (`--mainchain-grpc-url`) — If both instances use the same mainchain/enforcer, use the same URL. If they use different mainchain nodes, set a different URL per instance.

**Instance 1 (defaults):**
```bash
$ cargo run --bin coinshift_app -- --headless
```

**Instance 2 (separate datadir, RPC, and P2P):**
```bash
$ cargo run --bin coinshift_app -- --headless \
  --datadir ~/coinshift-instance2 \
  --rpc-addr 127.0.0.1:6256 \
  --net-addr 0.0.0.0:4256
```

To talk to the second instance with the CLI, use `--rpc-url`:
```bash
$ cargo run --bin coinshift_app_cli -- --rpc-url http://localhost:6256 balance
```

| What        | Instance 1 (default) | Instance 2              |
|------------|----------------------|-------------------------|
| Data dir   | default              | `--datadir <path>`      |
| RPC        | `127.0.0.1:6255`     | `--rpc-addr 127.0.0.1:6256` |
| P2P        | `0.0.0.0:4255`       | `--net-addr 0.0.0.0:4256`   |
| CLI target | `http://localhost:6255` | `--rpc-url http://localhost:6256` |

## CLI commands

The CLI talks to the Coinshift RPC server (default `http://localhost:6255`). Use `--rpc-url` to override. Run `cargo run --bin coinshift_app_cli <command> --help` for per-command help.

### Wallet / seed

| Command | Description |
|--------|-------------|
| `backup-mnemonic` | Output mnemonic for backup (new phrase, or from file with `--from-file`) |
| `balance` | Get balance in sats |
| `generate-mnemonic` | Generate a new 12-word mnemonic |
| `get-new-address` | Get a new address |
| `get-wallet-addresses` | List wallet addresses (sorted by base58) |
| `get-wallet-utxos` | List wallet UTXOs |
| `recover-from-mnemonic` | Set seed from mnemonic and show addresses + balance |
| `set-seed-from-mnemonic` | Set wallet seed from mnemonic (no extra output) |
| `sidechain-wealth` | Total sidechain wealth (sats) |

### Deposits / withdrawals / transfers

| Command | Description |
|--------|-------------|
| `create-deposit` | Deposit to address (`--address`, `--value-sats`, `--fee-sats`) |
| `format-deposit-address` | Format a deposit address |
| `transfer` | Transfer to L2 address (`--dest`, `--value-sats`, `--fee-sats`) |
| `withdraw` | Withdraw to mainchain (`--mainchain-address`, `--amount-sats`, `--fee-sats`, `--mainchain-fee-sats`) |
| `pending-withdrawal-bundle` | Show pending withdrawal bundle |
| `latest-failed-withdrawal-bundle-height` | Height of latest failed withdrawal bundle |

### Swaps

| Command | Description |
|--------|-------------|
| `create-swap` | Create L2→L1 swap (`--parent-chain`, `--l1-recipient-address`, amounts, `--fee-sats`, etc.) |
| `update-swap-l1-txid` | Set L1 txid and confirmations for a swap |
| `claim-swap` | Claim swap after L1 confirmations |
| `list-swaps` | List all swaps |
| `list-swaps-by-recipient` | List swaps for one recipient |
| `get-swap-status` | Status for one swap (`--swap-id`) |
| `reconstruct-swaps` | Rebuild swap state from chain |

### L1 config

| Command | Description |
|--------|-------------|
| `get-l1-config` | Show L1 RPC config (optional `--chain`) |
| `set-l1-config` | Set L1 RPC for a chain (`--parent-chain`, `--url`, `--user`, `--password`) |

### Chain / blocks / peers

| Command | Description |
|--------|-------------|
| `get-blockcount` | Current block count |
| `get-best-mainchain-block-hash` | Best mainchain block hash |
| `get-best-sidechain-block-hash` | Best sidechain block hash |
| `get-block` | Get block by hash |
| `get-bmm-inclusions` | Mainchain blocks that commit to a block hash |
| `list-peers` | List peers |
| `connect-peer` | Connect to peer (`--addr`) |
| `forget-peer` | Remove peer from known peers (`--addr`) |

### Mempool / mining / node

| Command | Description |
|--------|-------------|
| `list-utxos` | List all UTXOs |
| `remove-from-mempool` | Remove tx from mempool (`--txid`) |
| `mine` | Mine a sidechain block (optional `--fee-sats`) |
| `stop` | Stop the node |

### Other

| Command | Description |
|--------|-------------|
| `openapi-schema` | Print OpenAPI schema |
