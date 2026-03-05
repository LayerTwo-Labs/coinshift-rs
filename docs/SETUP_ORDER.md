# Setup Order - Step by Step Guide

This guide shows the correct order to run all the setup scripts for a complete regtest environment.

## Prerequisites

Make sure you have:
- Bitcoin Core (patched) built and available
- `bip300301_enforcer` built and available
- `grpcurl` installed (for gRPC calls)
- All scripts are executable (`chmod +x scripts/regtest/*.sh`)

## Step-by-Step Setup

### Step 1: Start Mainchain Regtest Node

**Script:** `1_start_mainchain.sh`

```bash
# From project root
./scripts/regtest/1_start_mainchain.sh
```

**What it does:**
- Starts Bitcoin Core regtest node on port 18443
- Creates mainchain wallet (`mainchainwallet`)
- Mines initial blocks to mature coinbase

**Wait for:** Mainchain node to be fully started (script will wait automatically)

---

### Step 2: (Optional) Start Parentchain Regtest Node

**Script:** `scripts/regtest/2_start_parentchain.sh`

```bash
./scripts/regtest/2_start_parentchain.sh
```

**What it does:**
- Starts a separate Bitcoin Core regtest node on port 18444
- Creates parentchain wallet (`parentchainwallet`)
- Used for swap transactions (if you're doing swaps)

**Note:** Only needed if you're testing swap functionality. Can be skipped for basic mining.

---

### Step 3: Start Enforcer

**Script:** `scripts/regtest/3_start_enforcer.sh`

```bash
./scripts/regtest/3_start_enforcer.sh
```

**What it does:**
- Starts the bip300301_enforcer
- Connects to mainchain via RPC and ZMQ
- Enables wallet functionality
- Creates sidechain proposal (unless `--skip-proposal` is used)
- Mines 6 blocks for sidechain activation

**Wait for:** Enforcer gRPC to be ready (script checks automatically)

**Important:** The enforcer must be running before you can create/unlock the wallet or mine blocks.

---

### Step 4: Create Enforcer Wallet

**Script:** `scripts/regtest/create_enforcer_wallet.sh`

```bash
# Create unencrypted wallet (easiest for testing)
./scripts/regtest/create_enforcer_wallet.sh ""

# OR create encrypted wallet
./scripts/regtest/create_enforcer_wallet.sh "mypassword"
```

**What it does:**
- Creates a new enforcer wallet with a mnemonic
- Wallet is separate from the Bitcoin Core mainchain wallet

**When to run:**
- Only if wallet doesn't exist yet
- Check first: The script will warn if wallet already exists

**Note:** If the enforcer was started with `--wallet-auto-create`, this step may be skipped (wallet already exists).

---

### Step 5: Unlock Enforcer Wallet

**Script:** `scripts/regtest/unlock_enforcer_wallet.sh`

```bash
# If wallet has no password
./scripts/regtest/unlock_enforcer_wallet.sh ""

# If wallet has a password
./scripts/regtest/unlock_enforcer_wallet.sh "mypassword"
```

**What it does:**
- Unlocks the enforcer wallet so it can be used
- Required before mining or creating transactions

**When to run:**
- After creating the wallet (if it has a password)
- After restarting the enforcer (wallet locks on restart)
- If you get "wallet not unlocked" errors

**Note:** If wallet was created without password, it may already be unlocked.

---

### Step 6: (Optional) Fund Enforcer Wallet

**Script:** `scripts/regtest/fund_enforcer_wallet.sh`

```bash
# Send 1 BTC (default)
./scripts/regtest/fund_enforcer_wallet.sh

# Send custom amount
./scripts/regtest/fund_enforcer_wallet.sh 5.0
```

**What it does:**
- Sends funds from mainchain wallet to enforcer wallet
- Required for creating deposit transactions
- NOT required for mining blocks

**When to run:**
- Only if you need to create deposit transactions
- Only if you need to send transactions from enforcer wallet
- Skip if you only want to mine blocks

---

### Step 7: Mine Blocks

**Script:** `scripts/regtest/mine_with_enforcer.sh`

```bash
# Mine 1 block (default)
./scripts/regtest/mine_with_enforcer.sh

# Mine multiple blocks
./scripts/regtest/mine_with_enforcer.sh 5

# Mine blocks and ACK all proposals
./scripts/regtest/mine_with_enforcer.sh 5 true
```

**What it does:**
- Mines blocks on the mainchain using the enforcer
- Coinbase rewards go to the enforcer wallet
- No funds needed in wallet to mine

**When to run:**
- After wallet is created and unlocked
- Anytime you want to mine more blocks

---

## Alternative: Mine Blocks on Mainchain Directly

**Script:** `scripts/regtest/4_mine_blocks.sh`

```bash
# Mine 1 block on both mainchain and parentchain
./scripts/regtest/4_mine_blocks.sh

# Mine 5 blocks on mainchain only
./scripts/regtest/4_mine_blocks.sh mainchain 5

# Mine 10 blocks on parentchain only
./scripts/regtest/4_mine_blocks.sh parentchain 10
```

**What it does:**
- Mines blocks directly on Bitcoin Core nodes
- Rewards go to Bitcoin Core wallets (not enforcer wallet)
- Simpler but doesn't use enforcer features

**When to use:**
- For initial setup (mature coinbase)
- When you don't need enforcer-specific features
- For testing Bitcoin Core functionality

---

## Utilities: Addresses and Cross-Chain Sends

### Generate addresses (mainchain / parentchain)

**Script:** `scripts/regtest/generate_addresses.sh`

Always shows **getblockchaininfo** for both mainchain and parentchain, then generates addresses.

```bash
# Mainchain address only (default)
./scripts/regtest/generate_addresses.sh

# Parentchain address only
./scripts/regtest/generate_addresses.sh parentchain

# One address from each chain
./scripts/regtest/generate_addresses.sh both
```

### Send from mainchain or parentchain

**Script:** `scripts/regtest/send_from.sh`

Send from mainchain or parentchain to any address. Always shows **getblockchaininfo** for both chains before and after.

```bash
# Send 1 BTC from mainchain to address
./scripts/regtest/send_from.sh mainchain <address> 1.0

# Send 0.5 BTC from parentchain to address
./scripts/regtest/send_from.sh parentchain <address> 0.5
```

Requires the source chain node to be running. Get an address from the other chain with `./scripts/regtest/generate_addresses.sh mainchain` or `./scripts/regtest/generate_addresses.sh parentchain`.

### Initialize coinshift_app for test users (Alice, Bob, Charles)

**Script:** `scripts/regtest/init_coinshift_app.sh`

Creates a data directory and a `start.sh` script for each user at a given location. Each user gets unique RPC and P2P ports so you can run multiple coinshift_app instances for testing.

```bash
# Initialize one user (e.g. at ./test-users)
./scripts/regtest/init_coinshift_app.sh ./test-users alice
./scripts/regtest/init_coinshift_app.sh ./test-users bob
./scripts/regtest/init_coinshift_app.sh ./test-users charles

# Initialize all three at once
./scripts/regtest/init_coinshift_app.sh ./test-users all
```

Then start each user's app (enforcer and mainchain should be running):

```bash
./test-users/alice/start.sh
./test-users/bob/start.sh
./test-users/charles/start.sh
```

Ports: Alice RPC 6010 / P2P 4010, Bob 6020/4020, Charles 6030/4030. Override binary with `COINSHIFT_APP`, mainchain gRPC with `MAINCHAIN_GRPC_URL`.

### Get transactions / UTXOs for an address

**Script:** `scripts/regtest/get_txs_from_address.sh`

Shows **getblockchaininfo** for both chains, then unspent outputs (and wallet receive history if the address is in the wallet) for a given address.

```bash
# On mainchain (default)
./scripts/regtest/get_txs_from_address.sh mainchain bcrt1q9z447588v4ua9nna7ff83zqfrcqlj8xklf4nl5

# On parentchain
./scripts/regtest/get_txs_from_address.sh parentchain bcrt1q...

# Address only → mainchain
./scripts/regtest/get_txs_from_address.sh bcrt1q9z447588v4ua9nna7ff83zqfrcqlj8xklf4nl5
```

---

## Complete Workflow Example

Here's a complete example for a fresh setup:

```bash
# From project root

# 1. Start mainchain
./scripts/regtest/1_start_mainchain.sh

# 2. Start enforcer
./scripts/regtest/3_start_enforcer.sh

# 3. Create wallet (if needed)
./scripts/regtest/create_enforcer_wallet.sh ""

# 4. Unlock wallet (if needed)
./scripts/regtest/unlock_enforcer_wallet.sh ""

# 5. Mine blocks with enforcer
./scripts/regtest/mine_with_enforcer.sh 10

# 6. (Optional) Fund wallet for deposits
./scripts/regtest/fund_enforcer_wallet.sh 1.0
```

---

## Quick Reference: Dependencies

```
scripts/regtest/1_start_mainchain.sh
    └─> (no dependencies)

scripts/regtest/2_start_parentchain.sh
    └─> (no dependencies, optional)

scripts/regtest/3_start_enforcer.sh
    └─> Requires: 1_start_mainchain.sh

scripts/regtest/create_enforcer_wallet.sh
    └─> Requires: 3_start_enforcer.sh

scripts/regtest/unlock_enforcer_wallet.sh
    └─> Requires: 3_start_enforcer.sh
        (wallet should exist, but script checks)

scripts/regtest/fund_enforcer_wallet.sh
    └─> Requires: 1_start_mainchain.sh
        Requires: 3_start_enforcer.sh
        Requires: Wallet created and unlocked

scripts/regtest/mine_with_enforcer.sh
    └─> Requires: 3_start_enforcer.sh
        Requires: Wallet created and unlocked

scripts/regtest/4_mine_blocks.sh
    └─> Requires: 1_start_mainchain.sh (and optionally 2_start_parentchain.sh)
```

---

## Troubleshooting

### "Mainchain is not running"
- Run `scripts/regtest/1_start_mainchain.sh` first

### "Enforcer gRPC is not accessible"
- Run `scripts/regtest/3_start_enforcer.sh` first
- Wait for enforcer to fully start (check logs)

### "Wallet not unlocked"
- Run `scripts/regtest/unlock_enforcer_wallet.sh` with the correct password

### "Wallet already exists"
- Wallet was already created, skip `scripts/regtest/create_enforcer_wallet.sh`
- Just unlock it with `scripts/regtest/unlock_enforcer_wallet.sh`

### "Insufficient funds"
- Mine more blocks on mainchain: `scripts/regtest/4_mine_blocks.sh mainchain 101`
- Or send funds: `scripts/regtest/fund_enforcer_wallet.sh`

---

## Stopping Everything

To stop all services:

```bash
# Stop Bitcoin Core nodes
pkill -f bitcoind

# Stop enforcer
pkill -f bip300301_enforcer
```

Or use the stop commands in the individual scripts.

