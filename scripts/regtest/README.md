# Regtest scripts

Scripts for running a full regtest environment: mainchain, optional parentchain, enforcer, and Coinshift app.

**Run from project root.** Ensure scripts are executable: `chmod +x scripts/regtest/*.sh`.

## Quick reference

| Script | Purpose |
|--------|---------|
| `1_start_mainchain.sh` | Start Bitcoin Core regtest (mainchain) |
| `2_start_parentchain.sh` | Start second regtest node (parentchain, for swaps) |
| `3_start_enforcer.sh` | Start bip300301 enforcer |
| `4_mine_blocks.sh` | Mine blocks on mainchain/parentchain |
| `create_enforcer_wallet.sh` | Create enforcer wallet |
| `unlock_enforcer_wallet.sh` | Unlock enforcer wallet |
| `fund_enforcer_wallet.sh` | Send funds to enforcer wallet |
| `mine_with_enforcer.sh` | Mine blocks using enforcer |
| `init_coinshift_app.sh` | Set up test users (Alice, Bob, Charles) |
| `generate_addresses.sh` | Generate mainchain/parentchain addresses |
| `send_from.sh` | Send from mainchain or parentchain |
| `get_txs_from_address.sh` | List UTXOs/txs for an address |
| `get_raw_transaction.sh` | Fetch raw transaction by txid |

## Full guide

See **[docs/SETUP_ORDER.md](../../docs/SETUP_ORDER.md)** for the correct order, dependencies, and troubleshooting.
