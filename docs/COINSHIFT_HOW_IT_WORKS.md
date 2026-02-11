# Coinshift: How It Works

**Document status:** Codebase-accurate as of review. Describes the current implementation in this repository.

---

## Overview

Coinshift is a trustless swap system for a BIP300-style sidechain that enables peer-to-peer exchanges between L2 (sidechain) coins and L1 (parent chain) assets. The system currently supports **L2 → L1 swaps** where Alice offers L2 coins in exchange for L1 assets (BTC, BCH, LTC, Signet, etc.) sent by Bob.

---

## Core Architecture

### System Components

```
┌─────────────────────────────────────────────────────────────┐
│ Sidechain Node                                                │
│                                                               │
│ ┌──────────────────────────────────────────────────────────┐  │
│ │ State Management (lib/state/)                            │  │
│ │ - Swap storage (swaps, swaps_by_l1_txid, swaps_by_state,│  │
│ │   swaps_by_recipient, locked_swap_outputs)               │  │
│ │ - Output locking/unlocking                               │  │
│ │ - Transaction validation                                 │  │
│ └──────────────────────────────────────────────────────────┘  │
│                              │                                 │
│                              ▼                                 │
│ ┌──────────────────────────────────────────────────────────┐  │
│ │ Block Processing (lib/state/block.rs)                    │  │
│ │ - SwapCreate: Create swap, lock outputs                  │  │
│ │ - SwapClaim: Verify state, unlock outputs                │  │
│ └──────────────────────────────────────────────────────────┘  │
│                              │                                 │
│                              ▼                                 │
│ ┌──────────────────────────────────────────────────────────┐  │
│ │ L1 Monitoring (lib/state/two_way_peg_data.rs)            │  │
│ │ - process_coinshift_transactions() during 2WPD connect   │  │
│ │ - query_and_update_swap(): RPC match, update state       │  │
│ └──────────────────────────────────────────────────────────┘  │
│                              │                                 │
│                              ▼                                 │
│ ┌──────────────────────────────────────────────────────────┐  │
│ │ RPC Client (lib/parent_chain_rpc.rs)                      │  │
│ │ - Query swap target chain (Signet, Mainnet, Regtest…)    │  │
│ │ - find_transactions_by_address_and_amount()              │  │
│ │ - get_transaction(), get_transaction_confirmations()     │  │
│ └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ Parent Chain (L1) — swap target                              │
│ (Bitcoin, Signet, BCH, LTC, Regtest, etc.)                   │
└─────────────────────────────────────────────────────────────┘
```

**Note:** The sidechain’s **mainchain** (for deposits/withdrawals and BMM) can be different from the **swap target chain** (e.g. sidechain on Regtest, swaps on Signet).

---

## Complete Swap Flow

### 1. Swap Creation (Alice)

1. **Alice creates swap offer**  
   Alice has L2 coins and wants L1 assets. She calls RPC `create_swap()` with:
   - `l1_recipient_address`: Her L1 address
   - `l1_amount`: Amount of L1 she wants
   - `l2_amount`: Amount of L2 she offers
   - `l2_recipient`: Optional; if set, only that address can claim
   - `required_confirmations`: L1 confirmations needed (defaults by chain)

2. **Swap ID**  
   Deterministic: `SwapId = blake3(l1_addr || l1_amt || l2_sender || l2_recipient)`.  
   Code: `lib/types/swap.rs::SwapId::from_l2_to_l1()`.

3. **SwapCreate transaction**  
   Wallet builds SwapCreate (metadata + locked L2 inputs), signs, broadcasts.  
   Code: `lib/wallet.rs`.

4. **Block processing — SwapCreate**  
   When a block containing SwapCreate is connected:
   - **Validation** (`lib/state/swap.rs::validate_swap_create()`):
     - Computed swap ID matches tx swap ID
     - Swap does not already exist
     - Transaction structure, outputs, no locked inputs (except as allowed), sufficient input value
   - **Output locking** (`lib/state/block.rs`): All outputs of the SwapCreate are locked to the swap; stored in `locked_swap_outputs`.
   - **Storage**: Swap saved via `save_swap()`; indexes updated (`swaps`, `swaps_by_l1_txid`, `swaps_by_state`, `swaps_by_recipient`).
   - Initial state: `SwapState::Pending`.

### 2. L1 Transaction Monitoring (Bob)

1. **Bob sends L1 transaction** to Alice’s `l1_recipient_address` for the exact `l1_amount`.

2. **When L1 monitoring runs**  
   During **2WPD (two-way peg data) processing**, when the sidechain’s mainchain tip changes, `connect_two_way_peg_data()` runs and calls `process_coinshift_transactions()`.

3. **Matching**  
   For each pending (or waiting-confirmations) swap, the system calls the RPC for the **swap target chain** (`swap.parent_chain`), not necessarily the sidechain’s mainchain:
   - `find_transactions_by_address_and_amount(l1_recipient, l1_amount_sats)`  
   Code: `lib/parent_chain_rpc.rs`.

4. **Update**  
   `query_and_update_swap()` uses the first match: it sets `l1_txid`, `l1_txid_validated_at_block_hash` / `l1_txid_validated_at_height`, and state:
   - `confirmations >= required_confirmations` → `ReadyToClaim`
   - else → `WaitingConfirmations(current, required)`  
   Then `state.save_swap(rwtxn, &swap)` is called.

### 3. Swap Claiming (Bob)

1. **Bob creates SwapClaim** (e.g. via `claim_swap()`) with `swap_id`, optional `l2_claimer_address` for open swaps, and fee.

2. **Validation** (`lib/state/swap.rs::validate_swap_claim()`):
   - Swap exists, state is `ReadyToClaim`
   - At least one input locked to this swap; all locked inputs to same swap
   - For open swaps: L1 tx was detected (non-zero `l1_txid`)
   - At least one output to the correct recipient (swap’s `l2_recipient` or claimer)

3. **Block processing — SwapClaim** (`lib/state/block.rs`):
   - Unlock all inputs locked to this swap
   - Set swap state to `Completed`
   - Save swap

4. Bob’s L2 address receives the coins; swap is complete.

---

## Security Checks (Current Implementation)

### Implemented in code

| Check | Status | Where |
|-------|--------|--------|
| **Swap ID verification** | ✅ | `validate_swap_create()`: computed ID must match tx |
| **Swap uniqueness** | ✅ | `validate_swap_create()`: swap must not already exist |
| **Output locking** | ✅ | SwapCreate locks outputs; only SwapClaim can unlock |
| **Locked-input checks** | ✅ | Non-SwapClaim txs cannot spend locked outputs; SwapClaim must spend only this swap’s locks |
| **Recipient / amount matching** | ✅ | RPC matching by address + exact amount in `find_transactions_by_address_and_amount` |
| **State machine** | ✅ | Pending → WaitingConfirmations → ReadyToClaim → Completed; claim only in ReadyToClaim |
| **Block reference** | ✅ | `l1_txid_validated_at_block_hash` / `l1_txid_validated_at_height` stored when L1 tx is applied |
| **Confirmations threshold** | ✅ | State moves to ReadyToClaim only when `confirmations >= required_confirmations` |
| **Expiration** | ✅ | Swaps can have `expires_at_height`; expired swaps are marked Cancelled |

### Not implemented (doc vs code)

| Check | Doc claim | Code reality |
|-------|-----------|--------------|
| **L1 transaction uniqueness** | “Check L1 tx not already used by another swap; `L1TransactionAlreadyUsed`” | ❌ **Not implemented.** `get_swap_by_l1_txid()` exists but is **never** called before accepting an L1 tx. Saving a swap with a new `l1_txid` does not check if that `(parent_chain, l1_txid)` is already used by a *different* swap. |
| **Reject confirmations == 0** | “Only confirmed transactions accepted” | ❌ **Not implemented.** `query_and_update_swap()` does not reject when `confirmations == 0`. |
| **Block inclusion** | “Transaction must have block height” | ❌ **Not implemented.** `TransactionInfo` has `blockheight: Option<u32>` but it is not passed into `query_and_update_swap()` and there is no “must have block height” check. |
| **Error `L1TransactionAlreadyUsed`** | Listed in errors | ❌ **Does not exist** in `lib/state/error.rs`. |

---

## Parent-Chain Payment Confirmation (2WPD and Sidechain)

For **deposits and withdrawals** (two-way peg), the sidechain confirms “payment” on the parent (mainchain) using the following. There is **no** merkle proof of a specific L1 transaction inside a Bitcoin block in this codebase.

### 1. Mainchain header chain (SPV-style)

- Parent (mainchain) blocks are only stored if their **parent** is already in the archive (`lib/archive.rs::put_main_header_info()`).
- Headers are fetched from the CUSF mainchain validator and stored in order, forming a single verified chain.

### 2. Proof-of-work (total work)

- Each mainchain header has `work`; **total work** is accumulated along the chain.
- When syncing with a peer, the node verifies `peer_tip_info.total_work == computed_total_work` from the archive (`lib/net/peer/task.rs`). Tip choice uses total work.

### 3. BMM (merge-mining) verification

- A sidechain block is only considered verified when a **mainchain block** commits to it (`bmm_commitment == sidechain_block_hash`).
- The archive checks: commitment match, `prev_main_hash` consistency, and that the parent sidechain block had a valid BMM commitment in the main ancestry (`lib/archive.rs`).

### 4. Two-way peg data only from verified mainchain blocks

- Deposits and withdrawal-bundle events (submitted/confirmed/failed) are applied only from mainchain blocks that are **already in the archive** on the mainchain path.
- `TwoWayPegData` is built from `archive.main_ancestors()` and `archive.try_get_main_block_info()` (`lib/node/net_task.rs`). Events are not taken from arbitrary or unverified blocks.

### 5. Withdrawal bundle identity (M6id)

- Withdrawal bundle events are matched by M6id so that Submitted/Confirmed/Failed apply to the correct bundle.

### 6. Sidechain block body merkle root

- Used for **sidechain** block validation: `header.merkle_root` must equal `Body::compute_merkle_root(...)` (`lib/state/block.rs`). This is **not** a proof of L1 payment; it ties the sidechain block body to the sidechain header.

### Swaps (L2 → L1)

- For **Coinshift swaps**, “payment on parent chain” is confirmed by:
  - RPC to the **swap target chain** (`parent_chain_rpc.rs`): match by address + amount, then use confirmation count from RPC.
  - Transition to `ReadyToClaim` when `confirmations >= required_confirmations`.
- There is no separate header chain, BMM reports, or merkle proof for swap L1 transactions in this repository.

---

## Advanced Security (Planned / Not in This Repo)

The following are **not** present in the current codebase. Treat as planned or from another implementation:

| Feature | Doc often claims | Codebase |
|---------|------------------|----------|
| **BMM-based L1 transaction reports** | BMM participants include L1TransactionReport; N participants (min 2) consensus | No `lib/types/l1_report.rs`, no `lib/state/bmm_reports.rs`. BMM here is merge-mining only (mainchain commits to sidechain block hash). |
| **Header chain per swap parent chain** | HeaderChain, sync, prev_hash, PoW for each parent chain | No `lib/types/header_chain.rs`, no `lib/state/header_chain.rs`. Mainchain header chain exists for 2WPD only. |
| **Confirmation count from header chain** | Confirmations from header chain; BMM reports verified against it | No; confirmations for swaps come from RPC only. |
| **Merkle proof of L1 tx in block** | MerkleProof, verify(), merkle_proof_verified on Swap | No `lib/types/merkle.rs`; no `merkle_proof_verified` field on `Swap`. |

---

## Data Structures (As in Code)

### Swap (`lib/types/swap.rs`)

```rust
pub struct Swap {
    pub id: SwapId,
    pub direction: SwapDirection,
    pub parent_chain: ParentChainType,
    pub l1_txid: SwapTxId,
    pub required_confirmations: u32,
    pub state: SwapState,
    pub l2_recipient: Option<Address>,
    pub l2_amount: bitcoin::Amount,
    pub l1_recipient_address: Option<String>,
    pub l1_amount: Option<bitcoin::Amount>,
    pub l1_claimer_address: Option<String>,
    pub created_at_height: u32,
    pub expires_at_height: Option<u32>,
    pub l1_txid_validated_at_block_hash: Option<BlockHash>,
    pub l1_txid_validated_at_height: Option<u32>,
}
```

There is **no** `merkle_proof_verified` field in the current struct.

### Databases (`lib/state/mod.rs`)

- **swaps**: `SwapId` → `Swap`
- **swaps_by_l1_txid**: `(ParentChainType, SwapTxId)` → `SwapId` (used for lookups; uniqueness across swaps not enforced on save)
- **swaps_by_state**: `(SwapState, SwapId)` → `()`
- **swaps_by_recipient**: `Address` → `Vec<SwapId>`
- **locked_swap_outputs**: `OutPointKey` → `SwapId`

### Error types (`lib/state/error.rs`)

- Swap-related: `SwapNotFound`, `InvalidTransaction(String)`.
- **No** `L1TransactionAlreadyUsed` variant.

---

## Trust Model (Current)

- **Trusted for swap L1 confirmation:**  
  RPC to the swap target chain (and its confirmation count). No multi-source BMM consensus or header-chain verification for swaps in this codebase.

- **Protected against:**  
  - Spending locked outputs (only SwapClaim can unlock).  
  - Claiming before ReadyToClaim (validation in `validate_swap_claim`).  
  - Wrong recipient/amount (RPC match by address + amount).  
  - Invalid swap ID or duplicate swap at creation (validate_swap_create).

- **Not yet enforced:**  
  - One L1 transaction used for multiple swaps (no `get_swap_by_l1_txid` check before accept).  
  - Rejecting unconfirmed or non-block-included L1 txs (no explicit confirmations == 0 or block height check).

---

## Integration Points

| What | Where |
|------|--------|
| Block processing | `lib/state/block.rs` — SwapCreate (lock), SwapClaim (unlock, complete) |
| L1 monitoring | `lib/state/two_way_peg_data.rs::process_coinshift_transactions()` during 2WPD connect |
| RPC client | `lib/parent_chain_rpc.rs` (not `bitcoin_rpc.rs` in this repo) |
| Swap validation | `lib/state/swap.rs` — `validate_swap_create`, `validate_swap_claim`, `validate_no_locked_outputs` |
| State persistence | `lib/state/mod.rs` — `save_swap`, `update_swap_l1_txid`, `get_swap_by_l1_txid`, etc. |

---

## Current Limitations

1. **L1 transaction uniqueness:** Enforced: `get_swap_by_l1_txid` is used before accepting an L1 tx in `query_and_update_swap` and in `update_swap_l1_txid`; the same L1 tx cannot be associated with more than one swap.
2. **Confirmations and block inclusion:** No explicit “reject if confirmations == 0” or “must have block height” in `query_and_update_swap`.
3. **BMM reports / header chain / merkle proof:** None of these exist for swap L1 verification in this repo.
4. **RPC dependency:** Swap L1 presence and confirmation count rely on the configured RPC for the swap target chain.

---

## Summary

- **Implemented:** Swap creation and claim flow, output locking, deterministic swap ID, state machine, RPC-based L1 matching and confirmation threshold, block reference tracking, expiration. Parent-chain 2WPD security: mainchain header chain, PoW, BMM merge-mining, 2WPD only from verified mainchain blocks.
- **Not implemented (in this repo):** L1 tx uniqueness check before accept, explicit reject for confirmations == 0 or missing block height, BMM-based L1 reports, per–parent-chain header chain for swaps, merkle proof of L1 tx in block, and `merkle_proof_verified` on Swap.

This document is intended to match the current codebase and can be updated as features are added or removed.
