
# Complete Swap Implementation Specification

## Overview

Implement a trustless swap system for a BIP300-style sidechain that enables peer-to-peer exchanges between L2 (sidechain) coins and L1 (parent chain) assets. The system supports **L2 → L1 swaps** where Alice offers L2 coins in exchange for L1 assets (BTC, BCH, LTC, etc.) sent by Bob.

## Important considerations

* You don't need to accept non-mainchain assets for swaps (XMR, TRON, Eth, etc.)
* You only need to implement L2-> L1 swaps
* You need to monitor L1 txs to see if the coins were sent on L1, and if so, release the coins on L2
* You need to add an output kind for pending swaps, similar to a withdrawal output. These outputs will not be spendable until expiry, and will automatically be marked as spent if the corresponding L1 transfer is observed
* running a polling-based task for this is the wrong approach. You want to also get all coinshift txs whenever you apply 2WPD (two-way peg data (deposits/withdrawals)) and apply the effect of coinshift txs at the same time
* Communication with the mainchain client should happen via the mainchain task here
https://github.com/rsantacroce/coinshift-rust/blob/main/lib/node/mainchain_task.rs
* Whenever the sidechain's mainchain tip changes, it needs to get all withdrawal/deposit txs from the previous mainchain tip to the new mainchain tip, and apply them. This is the stage at which you need to get coinshift txs from L1 and apply them

**Note**: L1 → L2 deposits are handled by the BIP300 sidechain itself (automatic minting). This swap system is specifically for L2 → L1 peer-to-peer exchanges.

## Core Concepts

### Swap Flow (Alice & Bob Example)

1. **Alice** has L2 coins and wants BTC
2. **Alice** creates a swap offer: "I'll give 100,000 L2 sats if you send 0.001 BTC to my BTC address"
3. **Bob** sends 0.001 BTC to Alice's BTC address
4. **System** monitors Bob's BTC transaction and waits for confirmations
5. **Bob** claims Alice's 100,000 L2 sats after confirmations are reached

### Swap States

```
Pending → WaitingConfirmations → ReadyToClaim → Completed
   ↓                              ↓
Cancelled                      (on expiration)
```

- **Pending**: Swap created, waiting for L1 transaction
- **WaitingConfirmations**: L1 transaction detected, waiting for required confirmations
- **ReadyToClaim**: Required confirmations reached, L2 coins can be claimed
- **Completed**: L2 coins claimed, swap finished
- **Cancelled**: Swap expired or cancelled

## Data Structures

### SwapId
```rust
struct SwapId([u8; 32]);  // 32-byte identifier
```

**Generation Algorithm**:
- For **L2 → L1 swaps**: `blake3_hash(l1_recipient_address || l1_amount_le_bytes || l2_sender_address || l2_recipient_address)`
- For **L1 → L2 swaps**: `blake3_hash(l1_txid_bytes || l2_recipient_address)`
- Result is deterministic: same parameters = same swap ID

### Swap
```rust
struct Swap {
    id: SwapId,
    direction: SwapDirection,  // L1ToL2 or L2ToL1
    parent_chain: ParentChainType,  // BTC, BCH, LTC, XMR, ETH, Tron
    l1_txid: TxId,  // L1 transaction ID (placeholder for L2ToL1 until filled)
    required_confirmations: u32,  // Default: 6 for BTC, 3 for others
    state: SwapState,
    l2_recipient: Address,  // Who receives L2 coins (Bob for L2ToL1)
    l2_amount: Amount,  // Amount of L2 coins
    l1_recipient_address: Option<String>,  // Alice's L1 address (for L2ToL1)
    l1_amount: Option<Amount>,  // Required L1 amount (for L2ToL1)
    created_at_height: u32,
    expires_at_height: Option<u32>,
}
```

### Transaction Types

#### SwapCreate Transaction
```rust
enum TxData {
    SwapCreate {
        swap_id: [u8; 32],  // Pre-computed swap ID
        parent_chain: ParentChainType,
        l1_txid_bytes: Vec<u8>,  // Placeholder [0u8; 32] for L2ToL1
        required_confirmations: u32,
        l2_recipient: Address,  // Bob's address
        l2_amount: u64,  // In satoshis
        l1_recipient_address: Option<String>,  // Alice's BTC address
        l1_amount: Option<u64>,  // Required L1 amount in satoshis
    },
    // ... other transaction types
}
```

#### SwapClaim Transaction
```rust
enum TxData {
    SwapClaim {
        swap_id: [u8; 32],
        proof_data: Option<Vec<u8>>,  // Reserved for future verification
    },
    // ... other transaction types
}
```

## State Management

### Database Schema

The state must maintain four databases:

1. **`swaps`**: `DatabaseUnique<SwapId, Swap>`
   - Primary storage for all swaps

2. **`swaps_by_l1_txid`**: `DatabaseUnique<(ParentChainType, TxId), SwapId>`
   - Lookup swap by parent chain and L1 transaction ID

3. **`swaps_by_recipient`**: `DatabaseUnique<Address, Vec<SwapId>>`
   - Lookup all swaps for a recipient address

4. **`locked_swap_outputs`**: `DatabaseUnique<OutPoint, SwapId>`
   - Tracks which outputs are locked to which swap
   - Prevents locked outputs from being spent except by SwapClaim

### State Methods

```rust
impl State {
    // Swap persistence
    fn save_swap(&self, rwtxn: &mut RwTxn, swap: &Swap) -> Result<()>;
    fn delete_swap(&self, rwtxn: &mut RwTxn, swap_id: &SwapId) -> Result<()>;
    fn get_swap(&self, rotxn: &RoTxn, swap_id: &SwapId) -> Result<Option<Swap>>;
    fn get_swap_by_l1_txid(&self, rotxn: &RoTxn, parent_chain: &ParentChainType, l1_txid: &TxId) -> Result<Option<Swap>>;
    fn get_swaps_by_recipient(&self, rotxn: &RoTxn, recipient: &Address) -> Result<Vec<Swap>>;
    fn load_all_swaps(&self, rotxn: &RoTxn) -> Result<Vec<Swap>>;
    
    // Output locking
    fn lock_output_to_swap(&self, rwtxn: &mut RwTxn, outpoint: &OutPoint, swap_id: &SwapId) -> Result<()>;
    fn unlock_output_from_swap(&self, rwtxn: &mut RwTxn, outpoint: &OutPoint) -> Result<()>;
    fn is_output_locked_to_swap(&self, rotxn: &RoTxn, outpoint: &OutPoint) -> Result<Option<SwapId>>;
}
```

## Transaction Validation

### SwapCreate Validation

When validating a `SwapCreate` transaction:

1. **Swap ID Verification**:
   - Recompute swap ID from transaction parameters
   - Verify computed ID matches `swap_id` in transaction
   - Error if mismatch

2. **Swap Existence Check**:
   - Verify swap with this ID doesn't already exist
   - Error if swap already exists

3. **Amount Validation**:
   - Verify `l2_amount > 0`
   - Error if zero or negative

4. **Coin Locking Validation** (for L2 → L1 swaps):
   - If `l1_recipient_address.is_some()`, this is an L2 → L1 swap
   - Check that no inputs are already locked to another swap
   - Verify transaction spends at least `l2_amount` worth of Bitcoin
   - Error if inputs are locked or insufficient funds

5. **Output Validation**:
   - Transaction must have at least one output
   - Error if no outputs

### SwapClaim Validation

When validating a `SwapClaim` transaction:

1. **Swap Existence**:
   - Verify swap exists in database
   - Error if swap not found

2. **Swap State**:
   - Verify swap is in `ReadyToClaim` state
   - Error if not ready

3. **Locked Output Verification**:
   - Verify at least one input is locked to this swap
   - Verify all locked inputs are locked to the same swap (the one being claimed)
   - Error if no locked inputs or wrong swap

4. **Recipient Output**:
   - Verify at least one output goes to `swap.l2_recipient`
   - Error if recipient doesn't receive coins

### General Locked Output Protection

For **all non-SwapClaim transactions**:
- Check that no inputs are locked to any swap
- Error if attempting to spend locked output
- Only `SwapClaim` transactions can spend locked outputs

## Block Processing

### Block Connection (connect_prevalidated / connect)

When a block containing swap transactions is connected:

#### SwapCreate Processing

1. **Reconstruct Swap Object**:
   ```rust
   let l1_txid = if l1_txid_bytes.len() == 32 {
       let mut hash32 = [0u8; 32];
       hash32.copy_from_slice(&l1_txid_bytes);
       TxId::Hash32(hash32)
   } else {
       TxId::Hash(l1_txid_bytes.clone())
   };
   
   let swap = Swap::new(
       parent_chain.clone(),
       l1_txid,
       Some(required_confirmations),
       l2_recipient,
       Amount::from_sat(l2_amount),
       current_height,
   );
   ```

2. **Verify Swap ID**:
   - Verify `swap.id.0 == swap_id` from transaction
   - Error if mismatch

3. **Lock Outputs** (for L2 → L1 swaps):
   - If `l1_recipient_address.is_some()`:
     - For each output in the transaction:
       - Create `OutPoint` from transaction ID and vout
       - Call `state.lock_output_to_swap(outpoint, swap_id)`

4. **Save Swap**:
   - Call `state.save_swap(rwtxn, &swap)`
   - This saves to all three swap databases with proper indexes

#### SwapClaim Processing

1. **Retrieve Swap**:
   - Get swap from database using `swap_id`
   - Error if not found

2. **Verify State**:
   - Verify swap is in `ReadyToClaim` state
   - Error if not ready

3. **Unlock Outputs**:
   - For each input in the transaction:
     - Check if input is locked to this swap
     - If locked, call `state.unlock_output_from_swap(input)`

4. **Mark Swap Complete**:
   - Call `swap.mark_completed()` (sets state to `Completed`)
   - Save updated swap: `state.save_swap(rwtxn, &swap)`

### Block Disconnection (disconnect_tip)

When a block is disconnected (rollback):

#### SwapCreate Rollback

1. **Unlock Outputs** (for L2 → L1 swaps):
   - If `l1_recipient_address.is_some()`:
     - For each output that was created:
       - Check if output is locked to this swap
       - If locked, call `state.unlock_output_from_swap(outpoint)`

2. **Delete Swap**:
   - Call `state.delete_swap(rwtxn, swap_id)`
   - This removes from all databases

#### SwapClaim Rollback

1. **Re-lock Outputs**:
   - For each input that was unlocked:
     - Check if input is currently unlocked (was unlocked by claim)
     - If unlocked, call `state.lock_output_to_swap(input, swap_id)`

2. **Revert Swap State**:
   - If swap state is `Completed`:
     - Set state back to `ReadyToClaim`
     - Save updated swap: `state.save_swap(rwtxn, &swap)`

## Swap State Updates

### Background Monitoring Task

A background task should run periodically (every 30 seconds) to update swap states:

```rust
async fn update_swap_states() {
    loop {
        // Get all swaps from database
        let swaps = state.load_all_swaps(rotxn)?;
        
        for mut swap in swaps {
            // Update swap state based on L1 transaction status
            swap.update_state(parent_chain_client, current_height).await?;
            
            // Save updated swap
            state.save_swap(rwtxn, &swap)?;
        }
        
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
```

### State Update Logic

```rust
impl Swap {
    async fn update_state(
        &mut self,
        client: &ParentChainClient,
        current_height: u32,
    ) -> Result<()> {
        // Check expiration
        if let Some(expires_at) = self.expires_at_height {
            if current_height >= expires_at {
                self.state = SwapState::Cancelled;
                return Ok(());
            }
        }
        
        // Query L1 transaction
        let tx_info = client
            .get_client(&self.parent_chain)?
            .get_transaction(&self.l1_txid)
            .await?;
        
        match tx_info {
            None => {
                // Transaction not found
                if !matches!(self.state, SwapState::Pending) {
                    return Err(SwapError::TransactionDisappeared);
                }
                // Stay in Pending
            }
            Some(tx) => {
                match self.state {
                    SwapState::Pending => {
                        // Transaction found, start waiting for confirmations
                        self.state = SwapState::WaitingConfirmations {
                            current_confirmations: tx.confirmations,
                            required_confirmations: self.required_confirmations,
                        };
                    }
                    SwapState::WaitingConfirmations { .. } => {
                        // Update confirmation count
                        if tx.confirmations >= self.required_confirmations {
                            self.state = SwapState::ReadyToClaim;
                        } else {
                            self.state = SwapState::WaitingConfirmations {
                                current_confirmations: tx.confirmations,
                                required_confirmations: self.required_confirmations,
                            };
                        }
                    }
                    SwapState::ReadyToClaim | SwapState::Completed | SwapState::Cancelled => {
                        // Already in final state, no update needed
                    }
                }
            }
        }
        
        Ok(())
    }
}
```

## Wallet Integration

### Creating SwapCreate Transaction

```rust
fn create_swap_create_tx(
    &self,
    parent_chain: ParentChainType,
    l1_recipient_address: String,  // Alice's BTC address
    l1_amount: Amount,
    l2_recipient: Address,  // Bob's L2 address
    l2_amount: Amount,
    required_confirmations: Option<u32>,
    current_height: u32,
) -> Result<(Transaction, SwapId)> {
    // 1. Compute swap ID
    let mut id_data = Vec::new();
    id_data.extend_from_slice(l1_recipient_address.as_bytes());
    id_data.extend_from_slice(&l1_amount.to_sat().to_le_bytes());
    id_data.extend_from_slice(&self.get_address().0);  // Alice's L2 address
    id_data.extend_from_slice(&l2_recipient.0);
    let id_hash = blake3::hash(&id_data);
    let swap_id = SwapId(*id_hash.as_bytes());
    
    // 2. Select UTXOs to spend (must spend at least l2_amount)
    let inputs = self.select_utxos_for_amount(l2_amount)?;
    
    // 3. Create outputs
    // Outputs will be locked to swap when transaction is processed
    let outputs = self.create_outputs_for_swap(l2_amount, l2_recipient)?;
    
    // 4. Create transaction
    let mut tx = Transaction::new(inputs, outputs);
    tx.data = Some(TxData::SwapCreate {
        swap_id: swap_id.0,
        parent_chain,
        l1_txid_bytes: vec![0u8; 32],  // Placeholder
        required_confirmations: required_confirmations
            .unwrap_or_else(|| default_confirmations(parent_chain)),
        l2_recipient,
        l2_amount: l2_amount.to_sat(),
        l1_recipient_address: Some(l1_recipient_address),
        l1_amount: Some(l1_amount.to_sat()),
    });
    
    Ok((tx, swap_id))
}
```

### Creating SwapClaim Transaction

```rust
fn create_swap_claim_tx(
    &self,
    swap_id: SwapId,
    locked_outputs: Vec<OutPoint>,  // Outputs locked to this swap
) -> Result<Transaction> {
    // 1. Get swap from state to verify it's ready and get recipient
    let swap = state.get_swap(rotxn, &swap_id)?
        .ok_or(SwapError::SwapNotFound)?;
    
    if !matches!(swap.state, SwapState::ReadyToClaim) {
        return Err(SwapError::InvalidStateTransition);
    }
    
    // 2. Create inputs from locked outputs
    let inputs = locked_outputs;
    
    // 3. Create output to swap recipient
    let output = Output {
        address: swap.l2_recipient,
        content: OutputContent::Bitcoin(swap.l2_amount),
        memo: None,
    };
    
    // 4. Create transaction
    let mut tx = Transaction::new(inputs, vec![output]);
    tx.data = Some(TxData::SwapClaim {
        swap_id: swap_id.0,
        proof_data: None,
    });
    
    Ok(tx)
}
```

## API Endpoints

### Create Swap
```rust
POST /create_swap
{
    "parent_chain": "BTC",
    "l1_recipient_address": "bc1q...",
    "l1_amount_sats": 100000,
    "l2_recipient": "0x...",
    "l2_amount_sats": 100000,
    "required_confirmations": 3
}

Response: {
    "swap_id": "abc123...",
    "txid": "def456..."
}
```

### Update Swap L1 Transaction ID
```rust
POST /update_swap_l1_txid
{
    "swap_id": "abc123...",
    "l1_txid": "def456..."
}

Response: { "success": true }
```

### Get Swap Status
```rust
GET /get_swap_status?swap_id=abc123...

Response: {
    "id": "abc123...",
    "state": "WaitingConfirmations",
    "current_confirmations": 2,
    "required_confirmations": 3,
    "l1_txid": "def456...",
    "l2_amount": 100000,
    ...
}
```

### Claim Swap
```rust
POST /claim_swap
{
    "swap_id": "abc123..."
}

Response: {
    "txid": "xyz789..."
}
```

### List Swaps
```rust
GET /list_swaps

Response: [
    { "id": "abc123...", "state": "ReadyToClaim", ... },
    { "id": "def456...", "state": "Pending", ... },
    ...
]
```

## Error Handling

### SwapError Enum
```rust
enum SwapError {
    ChainNotConfigured(ParentChainType),
    ClientError(String),
    TransactionDisappeared,
    InvalidStateTransition,
    SwapNotFound,
    SwapExpired,
}
```

### State Error Integration
```rust
enum Error {
    // ... existing errors
    InvalidTransaction(String),  // Used for swap validation errors
    // ... other errors
}
```

## Security Considerations

1. **Deterministic Swap IDs**: Same parameters always produce same swap ID, preventing duplicate swaps
2. **Output Locking**: Prevents double-spending of locked coins
3. **State Validation**: Only valid state transitions are allowed
4. **Confirmation Requirements**: Ensures L1 transaction is final before releasing L2 coins
5. **First-Come-First-Served**: First person to claim gets the coins (race condition handled by blockchain consensus)
6. **Rollback Safety**: All operations are properly reverted on block disconnection

## Testing Requirements

1. **Unit Tests**:
   - Swap ID generation (deterministic)
   - State transitions
   - Coin locking/unlocking
   - Database persistence

2. **Integration Tests**:
   - Complete swap flow (Alice creates, Bob fills, Bob claims)
   - Block connection/disconnection
   - Multiple concurrent swaps
   - Expiration handling

3. **Edge Cases**:
   - Invalid swap IDs
   - Attempting to spend locked outputs
   - Claiming non-ready swaps
   - Expired swaps

## Implementation Checklist

- [ ] Define Swap, SwapId, SwapState, SwapDirection data structures
- [ ] Implement swap ID generation algorithm
- [ ] Add SwapCreate and SwapClaim to transaction types
- [ ] Create swap databases in State
- [ ] Implement swap persistence methods (save, load, delete, query)
- [ ] Implement output locking methods
- [ ] Add swap validation to transaction validation
- [ ] Implement SwapCreate processing in block connection
- [ ] Implement SwapClaim processing in block connection
- [ ] Implement swap rollback in block disconnection
- [ ] Add background task for state updates
- [ ] Implement wallet methods for creating swap transactions
- [ ] Add RPC API endpoints
- [ ] Add CLI commands
- [ ] Write comprehensive tests

## Notes

- The system currently focuses on **L2 → L1 swaps** (withdrawals/exchanges)
- L1 → L2 deposits are handled by BIP300 sidechain itself
- Swap IDs are deterministic hashes, ensuring same parameters = same swap
- Output locking is critical for preventing double-spending
- All swap operations must be properly reverted on block rollback
- Background monitoring ensures swap states stay synchronized with L1 chain

