# Enforcer Wallet Guide

## Overview

The enforcer has its own wallet (separate from the Bitcoin Core mainchain wallet) that is used for:
- Mining blocks (receiving coinbase rewards)
- Creating deposit transactions to sidechains
- Creating BMM (Blind Merged Mining) transactions
- Sending transactions

## Wallet Creation

### Option 1: Auto-create (if enforcer started with `--auto-create`)

The enforcer can automatically create a wallet when it starts if the `--auto-create` flag is used. Check your `3_start_enforcer.sh` script to see if this is enabled.

### Option 2: Manual creation via gRPC

Use the `scripts/regtest/create_enforcer_wallet.sh` script:

```bash
# Create encrypted wallet with generated mnemonic
./scripts/regtest/create_enforcer_wallet.sh "mypassword"

# Create unencrypted wallet with generated mnemonic
./scripts/regtest/create_enforcer_wallet.sh ""

# Create wallet with existing mnemonic
./scripts/regtest/create_enforcer_wallet.sh "mypassword" /path/to/mnemonic.txt
```

## Do You Need to Move Funds?

### For Mining Blocks: **NO**

When mining blocks, the enforcer wallet:
- Generates a coinbase transaction (which has no inputs)
- Receives the block reward + transaction fees as the coinbase output
- Does NOT need existing funds to mine blocks

The wallet just needs to:
1. Exist (be created)
2. Be unlocked
3. Have an address to receive coinbase rewards (generated automatically)

### For Other Operations: **YES**

The enforcer wallet **DOES need funds** for:
- **Creating deposit transactions**: Requires UTXOs to send coins to sidechain deposit addresses
- **Creating BMM transactions**: Requires funds to pay transaction fees
- **Sending transactions**: Requires UTXOs to spend

## Workflow

### 1. Create the wallet (if not auto-created)

```bash
./scripts/regtest/create_enforcer_wallet.sh [password]
```

### 2. Unlock the wallet

```bash
./scripts/regtest/unlock_enforcer_wallet.sh [password]
```

### 3. Get the wallet address

```bash
grpcurl -plaintext 127.0.0.1:50051 \
  cusf.mainchain.v1.WalletService/CreateNewAddress
```

### 4. (Optional) Fund the wallet

If you need to create deposits or send transactions, send funds from your mainchain wallet:

```bash
# Get enforcer wallet address
ENFORCER_ADDR=$(grpcurl -plaintext 127.0.0.1:50051 \
  cusf.mainchain.v1.WalletService/CreateNewAddress | \
  grep -o '"address":"[^"]*"' | cut -d'"' -f4)

# Send funds from mainchain wallet (set BITCOIN_CLI and MAINCHAIN_DATADIR to match your setup)
$BITCOIN_CLI -regtest \
  -rpcuser=user -rpcpassword=passwordDC \
  -rpcport=18443 -datadir=$MAINCHAIN_DATADIR \
  -rpcwallet=mainchainwallet \
  sendtoaddress "$ENFORCER_ADDR" 1.0
```

### 5. Check wallet balance

```bash
grpcurl -plaintext 127.0.0.1:50051 \
  cusf.mainchain.v1.WalletService/GetBalance
```

### 6. Mine blocks

```bash
./scripts/regtest/mine_with_enforcer.sh 5
```

## Important Notes

1. **Two separate wallets**:
   - **Mainchain wallet** (Bitcoin Core): Used for general Bitcoin operations
   - **Enforcer wallet**: Used for BIP300/BIP301 operations (mining, deposits, etc.)

2. **Mining doesn't require funds**: The coinbase transaction is the reward itself - no inputs needed, so no fees to pay.

3. **Deposits require funds**: To create deposit transactions, the enforcer wallet must have UTXOs to spend.

4. **Wallet persistence**: The wallet is stored in the enforcer's data directory (typically `~/.local/share/bip300301_enforcer` or similar).

5. **Auto-unlock**: If the wallet was created without a password, it may be automatically unlocked. If it has a password, you must unlock it each time the enforcer starts (unless auto-unlock is implemented).

