# Swap Implementation Notes

## Critical Architecture Understanding

### Network Separation

The swap system operates across **different networks**:

1. **Sidechain Mainchain** (e.g., Regtest)
   - Used for: Deposits, Withdrawals, BIP300 sidechain operations
   - Alice deposits Regtest BTC → gets L2 coins
   - This is the sidechain's "parent chain" for BIP300 operations

2. **Swap Target Chain** (e.g., Signet)
   - Used for: Coinshift transactions (swaps)
   - Alice creates a Signet address for receiving swap payments
   - Bob sends Signet coins to Alice's Signet address
   - This is a **different network** from the sidechain's mainchain

### Key Distinction

```
┌─────────────────────────────────────────────────────────┐
│ Sidechain Operations (BIP300)                          │
│ - Mainchain: Regtest                                   │
│ - Operations: Deposits, Withdrawals                    │
└─────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────┐
│ Swap Operations (Coinshift)                            │
│ - Target Chain: Signet (can be different!)             │
│ - Operations: L2 → L1 swaps                            │
└─────────────────────────────────────────────────────────┘
```

## Implementation Requirements

### 1. Coinshift Transaction Monitoring

When processing 2WPD (two-way peg data), the system must:

1. **Process normal 2WPD** (Regtest deposits/withdrawals)
2. **ALSO process coinshift transactions** from swap target chains

```rust
// Pseudo-code for process_coinshift_transactions
fn process_coinshift_transactions(state, rwtxn, block_height) {
    let swaps = state.load_all_swaps(rwtxn)?;
    
    for swap in pending_swaps {
        // Get the swap target chain (NOT the sidechain's mainchain!)
        let target_chain = swap.parent_chain;  // e.g., Signet
        
        // Query the TARGET CHAIN for transactions
        let client = get_client_for_chain(target_chain)?;  // Signet client
        
        // Look for transactions matching the swap
        let transactions = client.query_transactions(
            address: swap.l1_recipient_address,  // Alice's Signet address
            amount: swap.l1_amount,              // Expected amount
        )?;
        
        // Update swap state based on found transactions
        if let Some(tx) = transactions.first() {
            state.update_swap_l1_txid(
                swap.id,
                tx.txid,
                tx.confirmations,
            )?;
        }
    }
}
```

### 2. Network Client Management

The system needs clients for multiple networks:

```rust
struct SwapChainClients {
    regtest_client: RegtestClient,  // For sidechain mainchain
    signet_client: SignetClient,    // For Signet swaps
    btc_client: Option<BtcClient>,   // For BTC swaps
    bch_client: Option<BchClient>,   // For BCH swaps
    ltc_client: Option<LtcClient>,   // For LTC swaps
}
```

### 3. Coinshift Detection in 2WPD Processing

The `connect_two_way_peg_data` function should:

```rust
pub fn connect(
    state: &State,
    rwtxn: &mut RwTxn,
    two_way_peg_data: &TwoWayPegData,
    swap_chain_clients: &SwapChainClients,  // NEW: Add clients
) -> Result<(), Error> {
    // ... existing 2WPD processing ...
    
    // Process coinshift transactions AFTER processing deposits/withdrawals
    process_coinshift_transactions(
        state,
        rwtxn,
        block_height,
        swap_chain_clients,  // Pass clients for querying swap chains
    )?;
    
    // ... rest of function ...
}
```

### 4. Transaction Matching Logic

When querying swap target chains, match transactions by:

1. **Recipient Address**: Must match `swap.l1_recipient_address`
2. **Amount**: Must match `swap.l1_amount` (within tolerance)
3. **Confirmations**: Track and update as blocks are mined

### 5. State Updates

Swap state transitions based on **swap target chain confirmations**:

- **Pending**: No transaction found yet
- **WaitingConfirmations**: Transaction found, waiting for required confirmations
  - Check confirmations on **Signet** (not Regtest!)
- **ReadyToClaim**: Required confirmations reached on **Signet**
- **Completed**: Swap claimed

## Example Flow

### Setup
- Sidechain initialized on: **Regtest**
- Alice wants to swap L2 coins for: **Signet** coins

### Steps

1. **Alice deposits Regtest BTC** → Gets L2 coins
   - Uses Regtest client
   - Processed via normal 2WPD

2. **Alice creates swap offer**
   - Generates Signet address: `alice_signet_addr`
   - Creates swap with `parent_chain: Signet`
   - Swap stored with state: `Pending`

3. **Bob sends Signet transaction**
   - Bob uses Signet client
   - Sends to `alice_signet_addr`
   - Transaction confirmed on Signet network

4. **Coinshift detection**
   - When Regtest tip changes → 2WPD processed
   - System queries **Signet** (not Regtest!) for matching transactions
   - Finds Bob's transaction
   - Updates swap with Signet transaction ID
   - Updates state based on Signet confirmations

5. **Bob claims swap**
   - After Signet confirmations reach threshold
   - Bob creates SwapClaim transaction
   - Receives L2 coins

## Current Implementation Status

### ✅ Implemented
- Swap data structures
- SwapCreate/SwapClaim transactions
- Output locking mechanism
- State persistence
- Block processing (connect/disconnect)
- RPC endpoints
- Wallet methods

### ⚠️ Needs Enhancement
- **Coinshift transaction monitoring**: Currently placeholder
  - Need to add clients for swap target chains
  - Need to query swap target chains when processing 2WPD
  - Need to match transactions by address and amount

### 🔧 Required Changes

1. **Add swap chain clients** to Node/App
2. **Update process_coinshift_transactions** to actually query swap chains
3. **Pass swap chain clients** to 2WPD processing
4. **Implement transaction matching** logic
5. **Add network configuration** for swap target chains

## Testing Considerations

When testing:

1. **Start Regtest node** (for sidechain mainchain)
2. **Start Signet node** (for swap target chain)
3. **Configure sidechain** to use Regtest as mainchain
4. **Configure swap monitoring** to query Signet
5. **Test cross-chain flow**: Regtest deposit → Signet swap → L2 claim

## Configuration Example

```toml
# Sidechain configuration
[sidechain]
mainchain = "regtest"
mainchain_grpc_url = "http://regtest-node:50051"

# Swap target chains
[swap_chains]
signet_rpc_url = "http://signet-node:8332"
btc_rpc_url = "http://btc-node:8332"  # Optional
```

