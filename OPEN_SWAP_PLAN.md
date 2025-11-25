# Open Swap Integration Plan

## Current Architecture (Pre-specified Recipient)

**Current Flow:**
1. Alice creates swap with `l2_recipient: bob_l2_addr` (Bob's address predetermined)
2. Bob sends L1 transaction to Alice's L1 address
3. Bob claims L2 coins to his predetermined address

**Current Swap Structure:**
```rust
pub struct Swap {
    pub l2_recipient: Address,  // Required - predetermined recipient
    // ...
}
```

**Current SwapCreate:**
```rust
SwapCreate {
    l2_recipient: Address,  // Required
    // ...
}
```

**Current Swap ID Calculation:**
```rust
SwapId::from_l2_to_l1(
    l1_recipient_address,
    l1_amount,
    l2_sender_address,
    l2_recipient_address,  // Included in hash
)
```

## Target Architecture (Open Swaps)

**New Flow:**
1. Alice creates swap **without** specifying recipient (open offer)
2. **Anyone** can send L1 transaction to Alice's L1 address
3. The **first person** who sends the L1 transaction can claim the L2 coins
4. Claimer's address is determined from the L1 transaction

**Key Changes:**
- `l2_recipient` becomes optional in SwapCreate
- Swap ID calculation changes (no recipient in hash)
- Need to track who sent the L1 transaction (the claimer)
- SwapClaim validation allows the claimer to claim

## Detailed Changes Required

### 1. Swap Data Structure Changes

**File: `lib/types/swap.rs`**

```rust
pub struct Swap {
    pub id: SwapId,
    pub direction: SwapDirection,
    pub parent_chain: ParentChainType,
    pub l1_txid: SwapTxId,
    pub required_confirmations: u32,
    pub state: SwapState,
    
    // CHANGED: Make optional - None means open swap
    pub l2_recipient: Option<Address>,
    
    pub l2_amount: bitcoin::Amount,
    pub l1_recipient_address: Option<String>,
    pub l1_amount: Option<bitcoin::Amount>,
    
    // NEW: Track who sent the L1 transaction (the claimer)
    pub l1_claimer_address: Option<String>,  // Address from L1 transaction
    
    pub created_at_height: u32,
    pub expires_at_height: Option<u32>,
}
```

**Rationale:**
- `l2_recipient: Option<Address>` - None means open swap, Some means pre-specified
- `l1_claimer_address: Option<String>` - Set when L1 transaction is detected

### 2. Swap ID Calculation Changes

**File: `lib/types/swap.rs`**

**Current:**
```rust
pub fn from_l2_to_l1(
    l1_recipient_address: &str,
    l1_amount: bitcoin::Amount,
    l2_sender_address: &Address,
    l2_recipient_address: &Address,  // Included
) -> Self
```

**New:**
```rust
pub fn from_l2_to_l1(
    l1_recipient_address: &str,
    l1_amount: bitcoin::Amount,
    l2_sender_address: &Address,
    l2_recipient_address: Option<&Address>,  // Optional
) -> Self {
    let mut id_data = Vec::new();
    id_data.extend_from_slice(l1_recipient_address.as_bytes());
    id_data.extend_from_slice(&l1_amount.to_sat().to_le_bytes());
    id_data.extend_from_slice(&l2_sender_address.0);
    // Only include recipient if specified (for backward compatibility)
    if let Some(recipient) = l2_recipient_address {
        id_data.extend_from_slice(&recipient.0);
    } else {
        // For open swaps, use a fixed marker
        id_data.extend_from_slice(b"OPEN_SWAP");
    }
    let hash = blake3::hash(&id_data);
    Self(*hash.as_bytes())
}
```

**Rationale:**
- Open swaps have different ID calculation (no recipient in hash)
- Maintains backward compatibility for pre-specified swaps
- Open swaps are uniquely identified by: L1 address + L1 amount + L2 sender

### 3. Transaction Data Changes

**File: `lib/types/transaction.rs`**

**Current:**
```rust
SwapCreate {
    swap_id: [u8; 32],
    parent_chain: ParentChainType,
    l1_txid_bytes: Vec<u8>,
    required_confirmations: u32,
    l2_recipient: Address,  // Required
    l2_amount: u64,
    l1_recipient_address: Option<String>,
    l1_amount: Option<u64>,
}
```

**New:**
```rust
SwapCreate {
    swap_id: [u8; 32],
    parent_chain: ParentChainType,
    l1_txid_bytes: Vec<u8>,
    required_confirmations: u32,
    l2_recipient: Option<Address>,  // Optional - None = open swap
    l2_amount: u64,
    l1_recipient_address: Option<String>,
    l1_amount: Option<u64>,
}
```

**Rationale:**
- Make `l2_recipient` optional to support open swaps

### 4. SwapCreate Validation Changes

**File: `lib/state/swap.rs`**

**Current validation:**
- Requires `l2_recipient` to be specified
- Swap ID includes recipient

**New validation:**
```rust
pub fn validate_swap_create(...) -> Result<(), Error> {
    // ...
    
    // Compute swap ID (recipient is optional now)
    let computed_swap_id = if let (Some(l1_addr), Some(l1_amt)) =
        (l1_recipient_address.as_ref(), l1_amount)
    {
        let first_input = filled_transaction
            .spent_utxos
            .first()
            .ok_or_else(|| {
                Error::InvalidTransaction("SwapCreate must have inputs".to_string())
            })?;
        let l2_sender_address = first_input.address;
        
        SwapId::from_l2_to_l1(
            l1_addr,
            bitcoin::Amount::from_sat(*l1_amt),
            &l2_sender_address,
            l2_recipient.as_ref(),  // Now optional
        )
    } else {
        return Err(Error::InvalidTransaction(
            "L2 → L1 swap requires l1_recipient_address and l1_amount".to_string(),
        ));
    };
    
    // ... rest of validation
}
```

**Rationale:**
- Allow `l2_recipient` to be None (open swap)
- Update swap ID calculation accordingly

### 5. L1 Transaction Detection & Claimer Tracking

**File: `lib/state/two_way_peg_data.rs` or `lib/state/swap.rs`**

**New functionality needed:**
```rust
/// When L1 transaction is detected, extract claimer address and update swap
pub fn update_swap_with_l1_transaction(
    state: &State,
    rwtxn: &mut RwTxn,
    swap_id: SwapId,
    l1_txid: SwapTxId,
    l1_claimer_address: String,  // NEW: Address from L1 transaction
    confirmations: u32,
) -> Result<(), Error> {
    let mut swap = state.get_swap(rwtxn, &swap_id)?
        .ok_or_else(|| Error::SwapNotFound { swap_id })?;
    
    // Update L1 transaction ID
    swap.update_l1_txid(l1_txid);
    
    // NEW: Store claimer address
    swap.l1_claimer_address = Some(l1_claimer_address);
    
    // Update state based on confirmations
    // ...
    
    state.save_swap(rwtxn, &swap)?;
    Ok(())
}
```

**How to extract claimer address:**
- Query L1 transaction from swap target chain
- Extract sender address from L1 transaction
- Store in `swap.l1_claimer_address`

**Rationale:**
- Need to track who sent the L1 transaction
- This person (or anyone who can prove they sent it) can claim

### 6. SwapClaim Validation Changes

**File: `lib/state/swap.rs`**

**Current validation:**
```rust
// 4. Verify at least one output goes to swap.l2_recipient
let recipient_receives = transaction
    .outputs
    .iter()
    .any(|output| output.address == swap.l2_recipient);
```

**New validation:**
```rust
// 4. Verify output goes to correct recipient
let expected_recipient = if let Some(recipient) = swap.l2_recipient {
    // Pre-specified swap: must go to specified recipient
    recipient
} else if let Some(claimer_addr) = &swap.l1_claimer_address {
    // Open swap: must go to claimer (from L1 transaction)
    // Need to convert claimer address to L2 address
    // This might require additional logic or the claimer provides their L2 address
    // For now, we could allow anyone to claim if they can prove they sent the L1 tx
    // OR require claimer to provide their L2 address in SwapClaim
    return Err(Error::InvalidTransaction(
        "Open swap claim requires claimer L2 address".to_string(),
    ));
} else {
    return Err(Error::InvalidTransaction(
        "Swap has no recipient or claimer".to_string(),
    ));
};
```

**Alternative approach (better):**
- Add `l2_claimer_address` to SwapClaim transaction
- Claimer provides their L2 address when claiming
- Validate that the claimer actually sent the L1 transaction (or allow anyone if not yet set)

**New SwapClaim structure:**
```rust
SwapClaim {
    swap_id: [u8; 32],
    l2_claimer_address: Option<Address>,  // NEW: For open swaps
    proof_data: Option<Vec<u8>>,
}
```

**Rationale:**
- Open swaps need claimer to specify their L2 address
- Pre-specified swaps use the predetermined recipient

### 7. Wallet Changes

**File: `lib/wallet.rs`**

**Current:**
```rust
pub fn create_swap_create_tx(
    &self,
    // ...
    l2_recipient: Address,  // Required
    // ...
) -> Result<(Transaction, SwapId), Error>
```

**New:**
```rust
pub fn create_swap_create_tx(
    &self,
    // ...
    l2_recipient: Option<Address>,  // Optional - None = open swap
    // ...
) -> Result<(Transaction, SwapId), Error> {
    // ...
    
    // Compute swap ID
    let swap_id = SwapId::from_l2_to_l1(
        &l1_recipient_address,
        l1_amount,
        &sender_address,
        l2_recipient.as_ref(),  // Optional
    );
    
    // ...
}
```

**Current:**
```rust
pub fn create_swap_claim_tx(
    &self,
    // ...
    l2_recipient: Address,  // From swap
    // ...
) -> Result<Transaction, Error>
```

**New:**
```rust
pub fn create_swap_claim_tx(
    &self,
    // ...
    swap: &Swap,
    l2_claimer_address: Address,  // NEW: Claimer's L2 address (for open swaps)
    // ...
) -> Result<Transaction, Error> {
    // Determine recipient
    let recipient = swap.l2_recipient
        .unwrap_or(l2_claimer_address);  // Use claimer address for open swaps
    
    // ...
    
    TxData::SwapClaim {
        swap_id: swap.id.0,
        l2_claimer_address: swap.l2_recipient.is_none().then_some(l2_claimer_address),
        proof_data: None,
    }
}
```

### 8. RPC API Changes

**File: `rpc-api/lib.rs`**

**Current:**
```rust
async fn create_swap(
    &self,
    parent_chain: ParentChainType,
    l1_recipient_address: String,
    l1_amount_sats: u64,
    l2_recipient: Address,  // Required
    l2_amount_sats: u64,
    required_confirmations: Option<u32>,
) -> RpcResult<(SwapId, Txid)>;
```

**New:**
```rust
async fn create_swap(
    &self,
    parent_chain: ParentChainType,
    l1_recipient_address: String,
    l1_amount_sats: u64,
    l2_recipient: Option<Address>,  // Optional - None = open swap
    l2_amount_sats: u64,
    required_confirmations: Option<u32>,
) -> RpcResult<(SwapId, Txid)>;
```

**Current:**
```rust
async fn claim_swap(
    &self,
    swap_id: SwapId,
) -> RpcResult<Txid>;
```

**New:**
```rust
async fn claim_swap(
    &self,
    swap_id: SwapId,
    l2_claimer_address: Option<Address>,  // Required for open swaps
) -> RpcResult<Txid>;
```

### 9. State Management Changes

**File: `lib/state/mod.rs`**

**When saving swap:**
- Handle `l2_recipient: Option<Address>`
- Handle `l1_claimer_address: Option<String>`

**When querying swaps:**
- Open swaps have `l2_recipient: None`
- Pre-specified swaps have `l2_recipient: Some(address)`

### 10. Block Processing Changes

**File: `lib/state/block.rs`**

**SwapCreate processing:**
- Handle optional `l2_recipient`
- Swap ID calculation updated

**SwapClaim processing:**
- Handle optional `l2_claimer_address`
- Determine recipient based on swap type

## Migration Strategy

### Backward Compatibility

1. **Existing swaps** (with `l2_recipient`):
   - Continue to work as before
   - `l2_recipient` is Some(address)
   - No claimer address needed

2. **New open swaps**:
   - `l2_recipient` is None
   - Claimer address set when L1 transaction detected
   - Claimer provides L2 address when claiming

### Database Migration

- Existing swaps in database have `l2_recipient: Address`
- New swaps can have `l2_recipient: Option<Address>`
- Need to handle deserialization of old format

## Testing Considerations

1. **Test open swap creation:**
   - Create swap without recipient
   - Verify swap ID is different from pre-specified swap
   - Verify swap is in Pending state

2. **Test L1 transaction detection:**
   - Send L1 transaction to swap address
   - Verify claimer address is extracted and stored
   - Verify swap state updates

3. **Test open swap claiming:**
   - Claimer provides their L2 address
   - Verify claim succeeds
   - Verify L2 coins go to claimer

4. **Test pre-specified swap (backward compatibility):**
   - Create swap with recipient
   - Verify it works as before

5. **Test multiple claimers:**
   - Multiple people send L1 transactions
   - Only first one can claim (or handle race condition)

## Security Considerations

1. **Race conditions:**
   - Multiple people send L1 transactions simultaneously
   - First one detected wins (or use transaction ordering)

2. **Claimer verification:**
   - Verify claimer actually sent the L1 transaction
   - Prevent front-running attacks

3. **Open swap expiration:**
   - If no one fills the swap, it expires
   - Alice can reclaim locked outputs (if implemented)

## Implementation Order

1. ✅ Update Swap data structure (add optional fields)
2. ✅ Update Swap ID calculation
3. ✅ Update SwapCreate transaction structure
4. ✅ Update validation logic
5. ✅ Update L1 transaction detection (extract claimer)
6. ✅ Update SwapClaim transaction structure
7. ✅ Update claim validation
8. ✅ Update wallet methods
9. ✅ Update RPC API
10. ✅ Update block processing
11. ✅ Add tests
12. ✅ Update documentation

## Open Questions

1. **Who can claim an open swap?**
   - Only the person who sent the L1 transaction?
   - Anyone who can prove they sent it?
   - First person to claim after L1 transaction is detected?

2. **How to verify claimer identity?**
   - Require signature from L1 transaction?
   - Trust the L1 transaction sender address?
   - Allow anyone to claim (less secure)?

3. **Multiple L1 transactions:**
   - What if multiple people send L1 transactions?
   - First one wins? All can claim? Only one can claim?

4. **Claimer L2 address:**
   - How does claimer provide their L2 address?
   - In SwapClaim transaction?
   - Pre-registered mapping?

## Recommended Approach

**Simplest and most secure:**
1. Open swap: `l2_recipient = None`
2. When L1 transaction detected: Extract sender address, store in `l1_claimer_address`
3. When claiming: Claimer provides their L2 address in SwapClaim
4. Validation: Verify the L1 transaction exists and matches swap
5. First valid claim wins (others fail validation)

This approach:
- ✅ Simple to implement
- ✅ Secure (verifies L1 transaction)
- ✅ Flexible (claimer chooses L2 address)
- ✅ Backward compatible (pre-specified swaps still work)

