# Swap Implementation Testing Guide

This guide walks you through setting up a proper environment and testing the swap functionality.

## Network Architecture

For testing purposes, the setup uses two Bitcoin networks:

- **Signet**: The sidechain's mainchain (for deposits/withdrawals)
  - The Coinshift sidechain connects to Signet as its mainchain
  - 2WPD (2-way peg deposits) are processed from Signet blocks
  
- **Regtest**: The swap parent chain (for coinshift transactions)
  - Swaps target Regtest as the parent chain
  - Bob sends Regtest transactions to fulfill swaps
  - Swap confirmations are tracked on Regtest

**Key Point**: The sidechain monitors Signet for mainchain activity (2WPD), but monitors Regtest for swap-related transactions.

## Prerequisites

1. **Rust toolchain**: Ensure you have Rust installed (1.70+ recommended)
   ```bash
   rustc --version
   cargo --version
   ```

2. **Dependencies**: Install required system dependencies
   ```bash
   # On macOS
   brew install cmake pkg-config
   
   # On Ubuntu/Debian
   sudo apt-get install cmake pkg-config libssl-dev
   ```

3. **Submodules**: Initialize git submodules
   ```bash
   git submodule update --init --recursive
   ```

## Building the Project

```bash
# Build the project
cargo build --release

# Or for development
cargo build
```

## Environment Setup Options

### Option 1: Manual Testing with Signet Mainchain + Regtest Swaps (Recommended for Development)

This is the simplest way to test swap functionality manually.

#### Step 1: Start Mainchain Node (Bitcoin Signet for Sidechain)

```bash
# Start Bitcoin Core in signet mode (for sidechain mainchain)
bitcoind -signet -daemon

# Create a wallet and generate some blocks
bitcoin-cli -signet createwallet "signetwallet"
bitcoin-cli -signet -generate 101

# Get a test address for deposits
bitcoin-cli -signet getnewaddress
```

#### Step 1b: Start Swap Parent Chain Node (Bitcoin Regtest)

```bash
# Start Bitcoin Core in regtest mode (for swap parent chain)
bitcoind -regtest -daemon

# Create a wallet and generate some blocks
bitcoin-cli -regtest createwallet "regtestwallet"
bitcoin-cli -regtest -generate 101

# Get a test address for swaps
bitcoin-cli -regtest getnewaddress
```

**Important**: You need BOTH networks running:
- **Signet**: For sidechain mainchain (deposits/withdrawals)
- **Regtest**: For swap parent chain (coinshift transactions)

#### Step 2: Start the Sidechain Node

```bash
# Create a data directory
mkdir -p ~/coinshift-test-data

# Start the Coinshift node (headless mode)
# Note: mainchain-grpc-url points to Signet (sidechain's mainchain)
cargo run --bin coinshift_app -- \
    --headless \
    --datadir ~/coinshift-test-data \
    --mainchain-grpc-url http://127.0.0.1:50051 \
    --network signet \
    --rpc-addr 127.0.0.1:8332

# Or with GUI
cargo run --bin coinshift_app -- \
    --datadir ~/coinshift-test-data \
    --mainchain-grpc-url http://127.0.0.1:50051 \
    --network signet
```

**Note**: The sidechain connects to Signet as its mainchain, and swaps target Regtest as the parent chain.

#### Step 3: Initialize Wallet

In another terminal, use the CLI or RPC:

```bash
# Generate a mnemonic
cargo run --bin coinshift_app_cli -- generate-mnemonic

# Set the seed (replace with generated mnemonic)
cargo run --bin coinshift_app_cli -- set-seed-from-mnemonic "your mnemonic phrase here"

# Get a new address
cargo run --bin coinshift_app_cli -- get-new-address
```

### Option 2: Integration Tests (Automated)

For automated testing with the full BIP300 enforcer setup:

#### Step 1: Build Integration Test Binaries

```bash
# Build the coinshift app binary
cargo build --release --bin coinshift_app

# Build other required binaries (enforcer, etc.)
# These are typically in submodules
```

#### Step 2: Set Up Environment Variables

Create or use `integration_tests/example.env`:

```bash
# Path to built binaries
COINSHIFT_BIN=target/release/coinshift_app
ENFORCER_BIN=path/to/enforcer/binary
# ... other required paths
```

#### Step 3: Run Integration Tests

```bash
# Run all integration tests
cargo run --example integration_tests

# Or with specific test
cargo run --example integration_tests -- --test swap_test
```

## Testing Swap Functionality

### Test Scenario: Complete L2 → L1 Swap Flow

This tests the full swap lifecycle: Alice creates a swap, Bob sends L1 coins, Bob claims L2 coins.

#### Step 1: Prepare Test Environment

```bash
# Terminal 1: Start Bitcoin Signet node (sidechain mainchain)
bitcoind -signet -daemon
bitcoin-cli -signet createwallet "signetwallet"
bitcoin-cli -signet -generate 101

# Terminal 1b: Start Bitcoin Regtest node (swap parent chain)
bitcoind -regtest -daemon
bitcoin-cli -regtest createwallet "regtestwallet"
bitcoin-cli -regtest -generate 101

# Terminal 2: Start Coinshift node (connects to Signet as mainchain)
cargo run --bin coinshift_app -- --headless --datadir ~/coinshift-test --network signet

# Terminal 3: Use for RPC calls
```

#### Step 2: Set Up Alice's Wallet (Swap Creator)

```bash
# Generate mnemonic for Alice
ALICE_MNEMONIC=$(cargo run --bin coinshift_app_cli -- generate-mnemonic | tail -1)

# Set Alice's seed
cargo run --bin coinshift_app_cli -- set-seed-from-mnemonic "$ALICE_MNEMONIC"

# Get Alice's L2 address
ALICE_L2_ADDR=$(cargo run --bin coinshift_app_cli -- get-new-address | tail -1)

# Get Alice's L1 (Bitcoin) address for receiving on Regtest (swap parent chain)
ALICE_L1_ADDR=$(bitcoin-cli -regtest getnewaddress)
```

#### Step 3: Fund Alice's L2 Wallet

```bash
# Create a deposit to Alice's L2 address
# (This requires the mainchain enforcer to be running)
# For manual testing, you might need to mine some blocks first
```

#### Step 4: Create a Swap (Alice)

```bash
# Create swap via RPC
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "create_swap",
    "params": {
      "parent_chain": "BTC",
      "l1_recipient_address": "'$ALICE_L1_ADDR'",
      "l1_amount_sats": 100000,
      "l2_recipient": "'$BOB_L2_ADDR'",
      "l2_amount_sats": 50000,
      "required_confirmations": 1,
      "fee_sats": 1000
    }
  }'

# Response will contain swap_id and txid
# Save the swap_id for later steps
SWAP_ID="<swap_id_from_response>"
```

#### Step 5: Check Swap Status

```bash
# Get swap status
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "get_swap_status",
    "params": {
      "swap_id": "'$SWAP_ID'"
    }
  }'

# Should show state: "Pending"
```

#### Step 6: Bob Sends Regtest Transaction

**Important**: Bob sends on Regtest (swap parent chain), NOT Signet!

```bash
# Bob sends Regtest Bitcoin to Alice's Regtest address
# This is on Regtest network (swap parent chain), not Signet!
REGTEST_TXID=$(bitcoin-cli -regtest sendtoaddress $ALICE_L1_ADDR 0.001)

# Mine a block on Regtest to confirm
bitcoin-cli -regtest -generate 1

# Get transaction details from Regtest
bitcoin-cli -regtest gettransaction $REGTEST_TXID

# Verify the transaction
bitcoin-cli -regtest getrawtransaction $REGTEST_TXID true
```

**Key Point**: The transaction is on Regtest (swap parent chain), and the sidechain monitors Regtest for this transaction when processing 2WPD.

#### Step 7: Update Swap with Regtest Transaction

**Important**: Update with Regtest transaction ID (swap parent chain), not Signet!

```bash
# Update swap with Regtest transaction ID
# The system should automatically detect this when processing 2WPD,
# but you can also manually update it
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "update_swap_l1_txid",
    "params": {
      "swap_id": "'$SWAP_ID'",
      "l1_txid_hex": "'$REGTEST_TXID'",
      "confirmations": 1
    }
  }'

# Check status again - should show "ReadyToClaim" or "WaitingConfirmations"
# Note: Confirmations are tracked on Regtest (swap parent chain), not Signet!
```

**Automatic Detection**: When the sidechain processes 2WPD (from Signet tip changes), it should automatically:
1. Query Regtest for transactions matching the swap
2. Update swap state based on Regtest confirmations
3. Transition swap to ReadyToClaim when Regtest confirmations are sufficient

#### Step 8: Bob Claims the Swap

```bash
# Set up Bob's wallet (different mnemonic)
BOB_MNEMONIC=$(cargo run --bin coinshift_app_cli -- generate-mnemonic | tail -1)
# ... set Bob's seed ...

# Bob claims the swap
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 4,
    "method": "claim_swap",
    "params": {
      "swap_id": "'$SWAP_ID'"
    }
  }'

# Response will contain the claim transaction ID
```

#### Step 9: Verify Swap Completion

```bash
# Check swap status - should be "Completed"
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 5,
    "method": "get_swap_status",
    "params": {
      "swap_id": "'$SWAP_ID'"
    }
  }'

# List all swaps
curl -X POST http://127.0.0.1:8332 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 6,
    "method": "list_swaps"
  }'
```

## Verification Checklist

### Swap Creation
- [ ] Swap ID is generated correctly (deterministic)
- [ ] SwapCreate transaction is valid and accepted
- [ ] Outputs are locked to the swap
- [ ] Swap appears in `list_swaps`
- [ ] Swap state is "Pending"

### L1 Transaction Detection
- [ ] Swap can be updated with L1 transaction ID
- [ ] Swap state transitions to "WaitingConfirmations" or "ReadyToClaim"
- [ ] Swap is findable by L1 transaction ID

### Swap Claiming
- [ ] SwapClaim transaction is valid
- [ ] Locked outputs are unlocked
- [ ] Swap state transitions to "Completed"
- [ ] L2 coins are received by the recipient

### Edge Cases
- [ ] Cannot spend locked outputs in regular transactions
- [ ] Cannot claim non-ready swaps
- [ ] Expired swaps are marked as "Cancelled"
- [ ] Block rollback properly reverts swap operations
- [ ] Duplicate swap creation is rejected

## Using the GUI

The GUI provides a visual interface for testing:

```bash
# Start with GUI
cargo run --bin coinshift_app -- --datadir ~/coinshift-test

# Navigate to the swap interface (if implemented)
# Or use the RPC endpoints via the console
```

## Debugging Tips

### Check Logs

```bash
# Enable trace logging
RUST_LOG=trace cargo run --bin coinshift_app -- --headless --datadir ~/coinshift-test
```

### Inspect State

```bash
# List all swaps
cargo run --bin coinshift_app_cli -- list-swaps

# Get specific swap
cargo run --bin coinshift_app_cli -- get-swap-status <swap_id>

# Check locked outputs
# (This might require adding a CLI command or using RPC directly)
```

### Common Issues

1. **Swap not found**: Ensure the swap was created and the swap_id is correct
2. **Cannot claim**: Verify swap is in "ReadyToClaim" state and has required confirmations
3. **Locked output error**: Check that outputs are properly locked and not being spent elsewhere
4. **State not updating**: Ensure 2WPD is being processed (from Signet mainchain) and coinshift transactions are detected on Regtest (swap parent chain)
5. **Wrong network**: Remember that the sidechain mainchain is Signet, but swap transactions happen on Regtest

## Integration Test Example

Create a new integration test file `integration_tests/swap_test.rs`:

```rust
use crate::setup::PostSetup;
use coinshift_app_rpc_api::RpcClient as _;

pub async fn swap_test(
    mut post_setup: PostSetup,
    mut enforcer_post_setup: EnforcerPostSetup,
) -> anyhow::Result<()> {
    // 1. Set up Alice and Bob
    let alice = PostSetup::setup(/* ... */).await?;
    let bob = PostSetup::setup(/* ... */).await?;
    
    // 2. Fund Alice's wallet
    // ... deposit to Alice ...
    
    // 3. Create swap
    let (swap_id, txid) = alice.rpc_client.create_swap(
        ParentChainType::BTC,
        "bc1q...", // Alice's L1 address
        100000,    // L1 amount
        bob.deposit_address,
        50000,     // L2 amount
        Some(1),   // confirmations
        1000,      // fee
    ).await?;
    
    // 4. Mine blocks to include swap
    alice.bmm(&mut enforcer_post_setup, 1).await?;
    
    // 5. Simulate L1 transaction (send BTC to Alice)
    // ... send Bitcoin transaction ...
    
    // 6. Update swap with L1 txid
    alice.rpc_client.update_swap_l1_txid(
        swap_id,
        l1_txid_hex,
        1, // confirmations
    ).await?;
    
    // 7. Mine more blocks
    alice.bmm(&mut enforcer_post_setup, 1).await?;
    
    // 8. Bob claims swap
    let claim_txid = bob.rpc_client.claim_swap(swap_id).await?;
    
    // 9. Mine to include claim
    bob.bmm(&mut enforcer_post_setup, 1).await?;
    
    // 10. Verify swap is completed
    let swap = bob.rpc_client.get_swap_status(swap_id).await?;
    assert!(matches!(swap.state, SwapState::Completed));
    
    Ok(())
}
```

## Next Steps

1. **Add CLI commands** for swap operations (if not already added)
2. **Create unit tests** for swap validation logic
3. **Add GUI components** for swap management
4. **Implement L1 transaction monitoring** (currently placeholder)
5. **Add swap expiration handling** (currently basic)

## Troubleshooting

### Database Issues

If you encounter database errors, you may need to reset:

```bash
# Remove data directory (WARNING: This deletes all data)
rm -rf ~/coinshift-test-data
```

### Port Conflicts

If ports are in use:

```bash
# Find and kill processes
lsof -ti:8332 | xargs kill -9  # RPC port
lsof -ti:50051 | xargs kill -9  # gRPC port
```

### Build Issues

```bash
# Clean and rebuild
cargo clean
cargo build --release
```

## Additional Resources

- See `prompt.md` for the full swap specification
- Check `integration_tests/` for existing test patterns
- Review `lib/state/swap.rs` for validation logic
- Check `lib/wallet.rs` for wallet methods

