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

## Complete Swap Flow Sequence

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
       │ 3. Create SwapCreate transaction
       │───────────────────────────────────────────────────────────────────────│
       │    create_swap(                                                       │
       │      parent_chain: Signet,                                            │
       │      l1_recipient: alice_signet_addr,                                  │
       │      l1_amount: 0.001 BTC,                                            │
       │      l2_recipient: bob_l2_addr,                                        │
       │      l2_amount: 100000 sats                                            │
       │    )                                                                   │
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │    Compute swap_id = hash(alice_signet_addr || l1_amount ||           │
       │                          alice_l2_addr || bob_l2_addr)                │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 4. Submit SwapCreate to sidechain
       │───────────────────────────────────────────────────────────────────────│
       │    SwapCreate TX                                                      │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 5. Validate & Process SwapCreate                                      │
       │───────────────────────────────────────────────────────────────────────│
       │    - Verify swap_id matches computed                                   │
       │    - Check swap doesn't exist                                         │
       │    - Verify sufficient L2 funds                                        │
       │    - Lock outputs to swap                                             │
       │    - Save swap to database (state: Pending)                            │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 6. Block includes SwapCreate
       │───────────────────────────────────────────────────────────────────────│
       │    Block N includes SwapCreate TX                                      │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 7. Swap is now active (state: Pending)                                 │
       │───────────────────────────────────────────────────────────────────────│
       │    Swap locked outputs are not spendable                              │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 8. Bob discovers swap offer
       │───────────────────────────────────────────────────────────────────────│
       │    Bob queries: list_swaps() or get_swap_status(swap_id)              │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 9. Bob sends Signet transaction
       │───────────────────────────────────────────────────────────────────────│
       │    signet_tx = send_signet_bitcoin(                                   │
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
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 13. Update swap with L1 transaction                                  │
       │───────────────────────────────────────────────────────────────────────│
       │    update_swap_l1_txid(                                               │
       │      swap_id,                                                          │
       │      signet_txid,                                                     │
       │      confirmations: 1                                                  │
       │    )                                                                   │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 14. Process coinshift in 2WPD                                          │
       │───────────────────────────────────────────────────────────────────────│
       │    When connecting 2WPD:                                              │
       │    - Check all pending swaps                                          │
       │    - Query Signet for matching transactions                           │
       │    - Update swap state based on confirmations                         │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 15. Swap state transitions                                            │
       │───────────────────────────────────────────────────────────────────────│
       │    Pending → WaitingConfirmations → ReadyToClaim                      │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │ 16. Bob claims swap
       │───────────────────────────────────────────────────────────────────────│
       │    claim_swap(swap_id)                                                │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 17. Create SwapClaim transaction                                      │
       │───────────────────────────────────────────────────────────────────────│
       │    SwapClaim TX:                                                      │
       │    - Spends locked outputs                                            │
       │    - Sends L2 coins to bob_l2_addr                                     │
       │───────────────────────────────────────────────────────────────────────│
       │                                                                        │
       │───────────────────────────────────────────────────────────────────────│
       │ 18. Validate & Process SwapClaim                                      │
       │───────────────────────────────────────────────────────────────────────│
       │    - Verify swap is ReadyToClaim                                       │
       │    - Verify inputs are locked to this swap                            │
       │    - Verify output goes to swap.l2_recipient                           │
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
       │    - Bob received 100000 L2 sats                                      │
       │    - Swap state: Completed                                             │
       │───────────────────────────────────────────────────────────────────────│
```

## Key Points

1. **Cross-Chain Nature**: The sidechain's mainchain (Regtest) is different from the swap target chain (Signet)
2. **Deposit Flow**: Alice first deposits Regtest BTC to get L2 coins (handled by BIP300)
3. **Swap Creation**: Alice creates swap offer with Signet address (different network)
4. **L1 Transaction Monitoring**: System monitors Signet (not Regtest) for coinshift transactions
5. **Coinshift Detection**: When 2WPD is processed, system queries Signet for matching transactions
6. **State Transitions**: Swap moves through states based on Signet transaction confirmations
7. **Claim Process**: Bob claims L2 coins after Signet transaction has required confirmations

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
4. Update swap state based on found transactions and confirmations

