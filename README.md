# Coinshift

Coinshift is a [BIP300](https://en.bitcoin.it/wiki/BIP_0300)-style sidechain node with a trustless **L2 <-> L1 swap** system. Exchange sidechain (L2) coins for parent-chain (L1) assets such as BTC, BCH, or LTC, and vice versa. The app includes a JSON-RPC server, CLI, and GUI.

- **Live node:** [coinshift.bip300.xyz](https://coinshift.bip300.xyz)
- **Built by:** [Layer Two Labs](https://layertwolabs.com)

## Supported chains (swaps)

| Chain                  | Ticker | Asset  | Finality        |
|------------------------|--------|--------|-----------------|
| Bitcoin                | BTC    | native | 6 blocks        |
| Bitcoin Cash           | BCH    | native | 3 blocks        |
| Litecoin               | LTC    | native | 3 blocks        |
| Bitcoin Signet         | sBTC   | native | 3 blocks        |
| Bitcoin Regtest        | rBTC   | native | 3 blocks        |
| Solana                 | SOL    | native | finalized       |
| Solana Devnet          | dSOL   | native | finalized       |
| USDC on Solana         | USDC   | SPL    | finalized       |
| USDC on Solana Devnet  | dUSDC  | SPL    | finalized       |

Every chain can point at any endpoint you run. Configure via the GUI
(**L1 Config**), the CLI (`set-l1-config`), or `coinshift_app init --l1 <chain>=<url>`.

Parent chains are optional and independent: the node runs without any of them,
and one being unreachable pauses detection for that chain alone. See
[docs/OPERATIONS.md](docs/OPERATIONS.md) for what fails versus what degrades, and
[docs/ADDING_PARENT_CHAINS.md](docs/ADDING_PARENT_CHAINS.md) for adding chains.

## Building

```bash
git clone https://github.com/layertwolabs/coinshift-rs.git
cd coinshift-rs
git submodule update --init
cargo build
```

## Running

```bash
# Start the RPC server (headless)
cargo run --bin coinshift_app -- --headless

# Start the GUI (includes an embedded RPC server)
cargo run --bin coinshift_app

# CLI for interacting with the JSON-RPC server
cargo run --bin coinshift_app_cli
```

## Running multiple instances

Run two or more Coinshift instances on the same machine by giving each its own **data directory**, **RPC address**, and **P2P (net) address**.

| What       | Instance 1 (default)    | Instance 2                          |
|------------|-------------------------|-------------------------------------|
| Data dir   | default                 | `--datadir <path>`                  |
| RPC        | `127.0.0.1:6255`        | `--rpc-addr 127.0.0.1:6256`        |
| P2P        | `0.0.0.0:4255`          | `--net-addr 0.0.0.0:4256`          |
| CLI target | `http://localhost:6255` | `--rpc-url http://localhost:6256`   |

**Example (second instance):**

```bash
cargo run --bin coinshift_app -- --headless \
  --datadir ~/coinshift-instance2 \
  --rpc-addr 127.0.0.1:6256 \
  --net-addr 0.0.0.0:4256
```

```bash
# Talk to the second instance with the CLI
cargo run --bin coinshift_app_cli -- --rpc-url http://localhost:6256 balance
```

## CLI commands

The CLI talks to the Coinshift RPC server (default `http://localhost:6255`). Use `--rpc-url` to override. Run `cargo run --bin coinshift_app_cli <command> --help` for per-command help.

### Wallet / seed

| Command | Description |
|---------|-------------|
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
|---------|-------------|
| `create-deposit` | Deposit to address (`--address`, `--value-sats`, `--fee-sats`) |
| `format-deposit-address` | Format a deposit address |
| `transfer` | Transfer to L2 address (`--dest`, `--value-sats`, `--fee-sats`) |
| `withdraw` | Withdraw to mainchain (`--mainchain-address`, `--amount-sats`, `--fee-sats`, `--mainchain-fee-sats`) |
| `pending-withdrawal-bundle` | Show pending withdrawal bundle |
| `latest-failed-withdrawal-bundle-height` | Height of latest failed withdrawal bundle |

### Swaps

| Command | Description |
|---------|-------------|
| `create-swap` | Create L2->L1 swap (`--parent-chain`, `--l1-recipient-address`, amounts, etc.) |
| `update-swap-l1-txid` | Set L1 txid and confirmations for a swap |
| `claim-swap` | Claim swap after L1 confirmations |
| `list-swaps` | List all swaps |
| `list-swaps-by-recipient` | List swaps for one recipient |
| `get-swap-status` | Status for one swap (`--swap-id`) |
| `reconstruct-swaps` | Rebuild swap state from chain |

### L1 config

| Command | Description |
|---------|-------------|
| `get-l1-config` | Show L1 RPC config, credentials omitted (optional `--chain`) |
| `set-l1-config` | Point a chain at an endpoint; verified before saving (`--parent-chain`, `--url`, `--user`, `--password`) |
| `get-connectivity-status` | Enforcer and per-chain reachability |

### Chain / blocks / peers

| Command | Description |
|---------|-------------|
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
|---------|-------------|
| `list-utxos` | List all UTXOs |
| `remove-from-mempool` | Remove tx from mempool (`--txid`) |
| `mine` | Mine a sidechain block (optional `--fee-sats`) |
| `stop` | Stop the node |

### Other

| Command | Description |
|---------|-------------|
| `openapi-schema` | Print OpenAPI schema |

## Documentation

| Doc | Description |
|-----|-------------|
| [docs/SETUP_ORDER.md](docs/SETUP_ORDER.md) | Step-by-step regtest setup (mainchain, enforcer, wallets, mining) |
| [docs/ADDING_PARENT_CHAINS.md](docs/ADDING_PARENT_CHAINS.md) | Supported L1 chains and how to add new ones |
| [docs/OPERATIONS.md](docs/OPERATIONS.md) | Deployment shapes, and what fails vs. degrades |
| [docs/PARENT_CHAIN_ROADMAP.md](docs/PARENT_CHAIN_ROADMAP.md) | Parent-chain audit and the plan this work followed |
| [docs/COINSHIFT_HOW_IT_WORKS.md](docs/COINSHIFT_HOW_IT_WORKS.md) | Architecture and swap flow |
| [docs/MANUAL_SETUP_SWAP_REGTEST.md](docs/MANUAL_SETUP_SWAP_REGTEST.md) | Manual regtest + swap (Alice & Bob) |
| [docs/ENFORCER_WALLET_GUIDE.md](docs/ENFORCER_WALLET_GUIDE.md) | Enforcer wallet creation and usage |
| [docs/SETUP_COMMANDS.md](docs/SETUP_COMMANDS.md) | Copy-paste setup commands (signet/regtest) |
| [docs/specs/swap-implementation-spec.md](docs/specs/swap-implementation-spec.md) | Swap implementation specification |

## Scripts

- **Regtest environment:** [scripts/regtest/](scripts/regtest/) — start mainchain, parentchain, enforcer, mine, fund wallets. See [scripts/README.md](scripts/README.md) and [docs/SETUP_ORDER.md](docs/SETUP_ORDER.md).
- **Other:** `scripts/setup.sh`, `scripts/test_swap.sh`.

## License

All rights reserved unless otherwise noted. See [LICENSE.txt](LICENSE.txt).
