# Coinshift

Coinshift is a [BIP300](https://en.bitcoin.it/wiki/BIP_0300)-style sidechain node with a trustless **L2 <-> L1 swap** system. Exchange sidechain (L2) coins for parent-chain (L1) assets such as BTC, BCH, or LTC, and vice versa. The app includes a JSON-RPC server, CLI, and GUI.

- **Live node:** [coinshift.bip300.xyz](https://coinshift.bip300.xyz)
- **Built by:** [Layer Two Labs](https://layertwolabs.com)

## Supported chains (swaps)

Swaps support the following L1 parent chains (Bitcoin Core-compatible RPC):

| Chain            | Ticker | Default RPC port | Confirmations |
|------------------|--------|------------------|---------------|
| Bitcoin          | BTC    | 8332             | 6             |
| Bitcoin Cash     | BCH    | 8332             | 3             |
| Litecoin         | LTC    | 9332             | 3             |
| Bitcoin Signet   | sBTC   | 38332            | 3             |
| Bitcoin Regtest  | rBTC   | 18443            | 3             |

Configure RPC per chain via the GUI (**L1 Config**) or CLI (`set-l1-config`). See [docs/ADDING_PARENT_CHAINS.md](docs/ADDING_PARENT_CHAINS.md) for adding new chains.

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
| `accept-swap` | Reserve an open swap for your L2 address, **before** paying on L1 (`--swap-id`) |
| `update-swap-l1-txid` | Set L1 txid and confirmations for a swap |
| `claim-swap` | Claim swap after L1 confirmations |
| `list-swaps` | List all swaps |
| `list-swaps-by-recipient` | List swaps for one recipient |
| `get-swap-status` | Status for one swap (`--swap-id`) |
| `reconstruct-swaps` | Rebuild swap state from chain |

### L1 config

| Command | Description |
|---------|-------------|
| `get-l1-config` | Show L1 RPC config (optional `--chain`) |
| `set-l1-config` | Set L1 RPC for a chain (`--parent-chain`, `--url`, `--user`, `--password`) |

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

Start at **[docs/index.html](docs/index.html)** — it links everything below and says which
documents live on other branches. Open it directly; there is no build step.

**Protocol documents** (`docs/specs/`) — illustrated, long-form, and where design decisions get argued out:

| Doc | Description |
|-----|-------------|
| [SWAP_MECHANICS.html](docs/specs/SWAP_MECHANICS.html) | Illustrated swap protocol: sequence diagrams, the consensus / node-local boundary, and what the L1 leg still trusts |
| [L1_PAYMENT_PROOFS.html](docs/specs/L1_PAYMENT_PROOFS.html) | Proposal: verifying the parent-chain leg of a swap in consensus |
| [PARENT_CHAIN_VERIFICATION.html](docs/specs/PARENT_CHAIN_VERIFICATION.html) | For decision: four ways to close the L1 gap for a BTC parent chain, and what each costs |
| [swap-implementation-spec.md](docs/specs/swap-implementation-spec.md) | Swap implementation specification |

**Guides** (`docs/`):

| Doc | Description |
|-----|-------------|
| [SETUP_ORDER.md](docs/SETUP_ORDER.md) | Step-by-step regtest setup (mainchain, enforcer, wallets, mining) |
| [ADDING_PARENT_CHAINS.md](docs/ADDING_PARENT_CHAINS.md) | Supported L1 chains and how to add new ones |
| [COINSHIFT_HOW_IT_WORKS.md](docs/COINSHIFT_HOW_IT_WORKS.md) | Architecture and swap flow |
| [MANUAL_SETUP_SWAP_REGTEST.md](docs/MANUAL_SETUP_SWAP_REGTEST.md) | Manual regtest + swap (Alice & Bob) |
| [ENFORCER_WALLET_GUIDE.md](docs/ENFORCER_WALLET_GUIDE.md) | Enforcer wallet creation and usage |
| [SETUP_COMMANDS.md](docs/SETUP_COMMANDS.md) | Copy-paste setup commands (signet/regtest) |

## Scripts

- **Regtest environment:** [scripts/regtest/](scripts/regtest/) — start mainchain, parentchain, enforcer, mine, fund wallets. See [scripts/README.md](scripts/README.md) and [docs/SETUP_ORDER.md](docs/SETUP_ORDER.md).
- **Other:** `scripts/setup.sh`, `scripts/test_swap.sh`.

## License

All rights reserved unless otherwise noted. See [LICENSE.txt](LICENSE.txt).
