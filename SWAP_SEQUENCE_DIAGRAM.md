# Swap Sequence Diagram

## Architecture Overview

The swap system enables **cross-chain swaps** where:
- **Sidechain Mainchain**: Bitcoin Regtest (where the sidechain is initialized via BIP300)
- **Swap Target Chain**: Bitcoin Signet (or other networks like BTC, BCH, LTC) - **different from mainchain**
- **L2**: Sidechain coins (pegged from Regtest deposits)

### Key Distinction

- **Deposits/Withdrawals**: Use the sidechain's mainchain (Regtest in this example)
- **Swaps**: Can target a different chain (Signet in this example)
- **Coinshift Monitoring**: Must query the swap target chain (Signet), NOT the mainchain (Regtest)

## Swap Types

The system supports two types of swaps:

1. **Open Swaps** (shown in diagram below): No predetermined recipient - anyone can fill
2. **Pre-Specified Swaps**: Predetermined recipient - only that address can claim

Both types follow similar flows, but open swaps:
- Don't require `l2_recipient` at creation
- Extract claimer from L1 transaction
- Require claimer to provide L2 address when claiming

## Complete Swap Flow Sequence (Open Swap)

```
┌─────────────┐  ┌──────────────┐  ┌─────────────┐  ┌──────────────┐  ┌─────────────┐
│   Alice     │  │  Sidechain   │  │  Regtest    │  │   Signet     │  │    Bob      │
│  (L2 User)  │  │    Node      │  │  (Mainchain)│  │  (Swap Chain)│  │ (Signet User)│
└──────┬──────┘  └──────┬───────┘  └──────┬──────┘  └──────┬───────┘  └──────┬──────┘
       │                │                 │                │                 │
       │ 1. Alice has L2 coins from Regtest deposit
       │───────────────────────────────────────────────────────────────────────│
       │    (Alice previously deposited Regtest BTC → got L2 coins)            │
       │                                                                        │
       │ 2. Create Signet address for receiving swap payment
       │───────────────────────────────────────────────────────────────────────│
       │    alice_signet_addr = generate_signet_address()                      │
       │                                                                        │
       │ 3. Create Open Swap (SwapCreate transaction)
       │───────────────────────────────────────────────────────────────────────│
       │    create_swap(                                                       │
       │      parent_chain: Signet,                                            │
       │      l1_recipient: alice_signet_addr,                                  │
       │      l1_amount: 0.001 BTC,                                            │
       │      l2_recipient: None,  // OPEN SWAP - anyone can claim             │
       │      l2_amount: 100000 sats                                            │
       │    )                                                                   │
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │    Compute swap_id = hash(alice_signet_addr || l1_amount ||           │
       │                          alice_l2_addr || "OPEN_SWAP")               │
       │    (Note: No recipient in hash for open swaps)                        │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 4. Submit SwapCreate to sidechain
       │───────────────────────────────────────────────────────────────────────│
       │    SwapCreate TX (l2_recipient: None)                                 │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 5. Validate & Process SwapCreate                                      │
       │───────────────────────────────────────────────────────────────────────│
       │    - Verify swap_id matches computed                                   │
       │    - Check swap doesn't exist                                         │
       │    - Verify sufficient L2 funds                                        │
       │    - Lock outputs to swap                                             │
       │    - Save swap to database:                                           │
       │      * state: Pending                                                 │
       │      * l2_recipient: None (open swap)                                 │
       │      * l1_claimer_address: None (not set yet)                        │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 6. Block includes SwapCreate
       │───────────────────────────────────────────────────────────────────────│
       │    Block N includes SwapCreate TX                                      │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 7. Open swap is now active (state: Pending)                            │
       │───────────────────────────────────────────────────────────────────────│
       │    - Swap locked outputs are not spendable                            │
       │    - Anyone can discover and fill this swap                            │
       │    - No predetermined recipient                                        │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 8. Anyone discovers swap offer (Bob in this case)
       │───────────────────────────────────────────────────────────────────────│
       │    Bob queries: list_swaps() or get_swap_status(swap_id)              │
       │    Discovers open swap (l2_recipient: None)                           │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 9. Bob sends Signet transaction (becomes the claimer)
       │───────────────────────────────────────────────────────────────────────│
       │    signet_tx = send_signet_bitcoin(                                   │
       │      from: bob_signet_addr,  // Bob's Signet address                  │
       │      to: alice_signet_addr,                                           │
       │      amount: 0.001 BTC                                                │
       │    )                                                                   │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 10. Signet transaction confirmed                                      │
       │───────────────────────────────────────────────────────────────────────│
       │    Signet block includes Bob's transaction                            │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 11. Monitor Signet for coinshift transactions                         │
       │───────────────────────────────────────────────────────────────────────│
       │    (When sidechain mainchain tip changes)                             │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 12. Get coinshift transactions from Signet                            │
       │───────────────────────────────────────────────────────────────────────│
       │    Query Signet for transactions matching:                            │
       │    - Address: alice_signet_addr                                       │
       │    - Amount: 0.001 BTC                                                │
       │    - Extract sender address: bob_signet_addr (the claimer)           │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 13. Update swap with L1 transaction & claimer                         │
       │───────────────────────────────────────────────────────────────────────│
       │    update_swap_with_l1_transaction(                                   │
       │      swap_id,                                                          │
       │      signet_txid,                                                     │
       │      l1_claimer_address: bob_signet_addr,  // NEW: Store claimer      │
       │      confirmations: 1                                                  │
       │    )                                                                   │
       │    Swap now has:                                                      │
       │    - l1_txid: signet_txid                                             │
       │    - l1_claimer_address: bob_signet_addr                              │
       │    - state: WaitingConfirmations                                      │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 14. Process coinshift in 2WPD                                          │
       │───────────────────────────────────────────────────────────────────────│
       │    When connecting 2WPD:                                              │
       │    - Check all pending swaps                                          │
       │    - Query Signet for matching transactions                           │
       │    - Extract claimer address from L1 transaction                      │
       │    - Update swap with claimer and confirmations                       │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 15. Swap state transitions                                            │
       │───────────────────────────────────────────────────────────────────────│
       │    Pending → WaitingConfirmations → ReadyToClaim                      │
       │    (Based on Signet confirmations)                                    │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 16. Bob claims swap (provides his L2 address)
       │───────────────────────────────────────────────────────────────────────│
       │    claim_swap(                                                        │
       │      swap_id,                                                          │
       │      l2_claimer_address: bob_l2_addr  // Bob's L2 address            │
       │    )                                                                   │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 17. Create SwapClaim transaction                                      │
       │───────────────────────────────────────────────────────────────────────│
       │    SwapClaim TX:                                                      │
       │    - Spends locked outputs                                            │
       │    - l2_claimer_address: bob_l2_addr  // For open swaps              │
       │    - Sends L2 coins to bob_l2_addr                                     │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 18. Validate & Process SwapClaim                                      │
       │───────────────────────────────────────────────────────────────────────│
       │    - Verify swap is ReadyToClaim                                       │
       │    - Verify inputs are locked to this swap                            │
       │    - For open swap:                                                   │
       │      * Verify claimer matches l1_claimer_address (Bob)                │
       │      * Verify output goes to provided l2_claimer_address              │
       │    - Unlock outputs                                                   │
       │    - Mark swap as Completed                                            │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 19. Block includes SwapClaim                                          │
       │───────────────────────────────────────────────────────────────────────│
       │    Block N+1 includes SwapClaim TX                                     │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 20. Swap completed                                                    │
       │───────────────────────────────────────────────────────────────────────│
       │    - Alice received 0.001 BTC on Signet                                │
       │    - Bob received 100000 L2 sats (to bob_l2_addr)                      │
       │    - Swap state: Completed                                             │
       │    - First person to send L1 transaction became the claimer           │
       │───────────────────────────────────────────────────────────────────────│
```

## Key Points

1. **Cross-Chain Nature**: The sidechain's mainchain (Regtest) is different from the swap target chain (Signet)
2. **Deposit Flow**: Alice first deposits Regtest BTC to get L2 coins (handled by BIP300)
3. **Open Swap Creation**: Alice creates swap offer without specifying recipient (`l2_recipient: None`)
4. **Anyone Can Fill**: Any user can discover and fill an open swap by sending the L1 transaction
5. **Claimer Detection**: The first person to send the L1 transaction becomes the claimer (stored in `l1_claimer_address`)
6. **L1 Transaction Monitoring**: System monitors Signet (not Regtest) for coinshift transactions
7. **Coinshift Detection**: When 2WPD is processed, system queries Signet for matching transactions and extracts claimer address
8. **State Transitions**: Swap moves through states based on Signet transaction confirmations
9. **Claim Process**: Claimer provides their L2 address when claiming; validation ensures they are the actual claimer
10. **Swap ID**: Open swaps have different ID calculation (includes "OPEN_SWAP" marker instead of recipient address)

## Open Swap vs Pre-Specified Swap

### Open Swap (Current Diagram)
- **Creation**: `l2_recipient: None` - no predetermined recipient
- **Swap ID**: Includes "OPEN_SWAP" marker instead of recipient address
- **Discovery**: Anyone can discover and fill the swap
- **Claimer**: First person to send L1 transaction becomes the claimer
- **Claim**: Claimer provides their L2 address when claiming
- **Use Case**: Public swap offers, market-making, liquidity provision

### Pre-Specified Swap (Alternative)
- **Creation**: `l2_recipient: Some(address)` - predetermined recipient
- **Swap ID**: Includes recipient address in hash
- **Discovery**: Only the specified recipient can claim
- **Claimer**: Predetermined at swap creation
- **Claim**: Uses predetermined recipient address
- **Use Case**: Direct swaps between known parties

## Network Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Sidechain Ecosystem                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌──────────────────┐              ┌──────────────────┐                    │
│  │   Regtest        │─────────────▶│   Sidechain      │                    │
│  │  (Mainchain)     │   Deposit    │      (L2)        │                    │
│  │                  │              │                  │                    │
│  │  Alice deposits  │              │  Alice gets      │                    │
│  │  Regtest BTC     │              │  L2 coins        │                    │
│  │                  │◀─────────────│                  │                    │
│  │                  │   Withdraw   │                  │                    │
│  └──────────────────┘              └────────┬─────────┘                    │
│                                             │                               │
│                                             │ Swap (Cross-Chain)            │
│                                             │                               │
│  ┌──────────────────┐                      │      ┌──────────────────┐    │
│  │   Signet         │◀─────────────────────┼──────│      Bob          │    │
│  │  (Swap Chain)    │   Coinshift TX        │      │  (Signet User)   │    │
│  │                  │   (0.001 BTC)         │      │                  │    │
│  │  Alice receives  │                      │      │  Bob sends       │    │
│  │  Signet BTC      │                      │      │  Signet coins     │    │
│  └──────────────────┘                      │      └──────────────────┘    │
│                                             │                               │
│                                             │      ┌──────────────────┐    │
│                                             └──────▶│     Alice        │    │
│                                                    │   (L2 User)      │    │
│                                                    │                  │    │
│                                                    │  Creates swap    │    │
│                                                    │  with Signet addr│    │
│                                                    └──────────────────┘    │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘

Key Points:
- Regtest: Sidechain's mainchain (for deposits/withdrawals)
- Signet: Swap target chain (different network!)
- Sidechain: L2 layer where swaps are created and claimed
- Coinshift monitoring queries Signet, NOT Regtest
```

## Implementation Notes

### Critical Implementation Details

1. **ParentChainType**: Can be different from the sidechain's mainchain network
   - Sidechain mainchain: Regtest (for deposits/withdrawals)
   - Swap target: Signet (for coinshift transactions)
   - These are separate networks with separate RPC clients

2. **Coinshift Monitoring**: Must query the swap target chain (Signet), NOT the mainchain (Regtest)
   - When 2WPD is processed for Regtest, ALSO query Signet for coinshift transactions
   - Match transactions by: `swap.l1_recipient_address` and `swap.l1_amount`
   - Update swap state based on Signet confirmations

3. **Transaction Detection Flow**:
   ```
   Sidechain mainchain tip changes (Regtest)
   ↓
   Process 2WPD (Regtest deposits/withdrawals)
   ↓
   ALSO: For each pending swap:
     - Get swap.parent_chain (e.g., Signet)
     - Query Signet RPC for transactions to swap.l1_recipient_address
     - Match by address and amount
     - Update swap with Signet transaction ID and confirmations
   ```

4. **Network Clients**: Need separate clients for each supported swap chain
   - Regtest client: For sidechain mainchain operations
   - Signet client: For coinshift transaction monitoring
   - BTC/BCH/LTC clients: For other swap targets

5. **Address Generation**: 
   - Alice generates Signet address (different network from Regtest)
   - This address is used in the swap offer
   - Bob sends Signet coins to this address

### Example Configuration

```rust
// Sidechain configuration
let sidechain_mainchain = Network::Regtest;  // Sidechain's mainchain
let mainchain_grpc_url = "http://regtest-node:50051";

// Swap configuration (can be different!)
let swap_target = ParentChainType::Signet;  // Swap target chain
let signet_rpc_url = "http://signet-node:8332";  // For coinshift monitoring
```

### Coinshift Detection Implementation

The `process_coinshift_transactions` function should:
1. Get all pending swaps
2. For each swap, determine the target chain (swap.parent_chain)
3. Query that chain's RPC for transactions matching:
   - Recipient address: `swap.l1_recipient_address`
   - Amount: `swap.l1_amount`
4. For each matching transaction:
   - Extract sender address from L1 transaction (the claimer)
   - Update swap with:
     - `l1_txid`: Transaction ID
     - `l1_claimer_address`: Sender address (for open swaps)
     - `confirmations`: Current confirmation count
5. Update swap state based on confirmations:
   - `Pending` → `WaitingConfirmations` (when transaction found)
   - `WaitingConfirmations` → `ReadyToClaim` (when required confirmations reached)

### Open Swap Implementation Details

1. **Swap ID Calculation**:
   ```rust
   // Open swap: l2_recipient is None
   SwapId::from_l2_to_l1(
       l1_recipient_address,
       l1_amount,
       l2_sender_address,
       None,  // No recipient = open swap
   )
   // Hash includes "OPEN_SWAP" marker instead of recipient
   ```

2. **Claimer Detection**:
   - When L1 transaction is detected, extract sender address
   - Store in `swap.l1_claimer_address`
   - This identifies who can claim the swap

3. **Claim Validation**:
   ```rust
   // For open swaps
   if swap.l2_recipient.is_none() {
       // Verify claimer matches L1 transaction sender
       assert_eq!(swap.l1_claimer_address, Some(bob_signet_addr));
       // Verify output goes to provided L2 address
       assert_eq!(claim_tx.l2_claimer_address, bob_l2_addr);
   }
   ```

4. **Race Conditions**:
   - Multiple users may send L1 transactions simultaneously
   - First transaction detected becomes the claimer
   - Subsequent transactions are ignored (or swap already claimed)

