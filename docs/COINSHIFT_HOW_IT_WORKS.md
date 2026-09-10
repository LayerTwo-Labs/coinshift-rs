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

### 2. Paying on L1 (Bob)

1. **Bob sends the L1 payment** to Alice's `l1_recipient_address` for at least
   `l1_amount`, **with an `OP_RETURN` output committing to the swap and to
   Bob's L2 address**:

   ```
   OP_RETURN "CSFT" || swap_id (32 bytes) || l2_claimer_address (20 bytes)
   ```

   `coinshift_app_cli l1-payment-commitment --swap-id <id> --l2-claimer-address <addr>`
   prints the payload as hex, for a `"data"` output in `createrawtransaction`
   or `send`. The commitment is what ties the payment to this swap and what
   makes Bob the party entitled to the escrow: a proof of a payment committing
   to Bob's address cannot be produced by anyone who did not make that payment.
   For a pre-specified swap the committed address must be `l2_recipient`.

2. **Local monitoring (advisory).** Nodes that run a parent-chain RPC discover
   the payment (`scantxoutset` plus `listunspent`) during 2WPD processing and
   through the periodic confirmation task, and record `l1_txid`,
   `l2_claimer_address` (from the commitment) and a `SwapState` for display.
   None of that gates anything any more: it tells the taker when the payment is
   deep enough to claim, and it lets the node build the proof without the
   taker pasting it. A node with no RPC config validates every claim exactly
   as one with it.

### 3. Reserving an open swap (Bob), optional

An open swap has no recipient fixed at creation, so Bob may reserve it
on-chain before paying:

```
coinshift_app_cli accept-swap --swap-id <id>
```

This broadcasts `TxData::SwapAccept { swap_id, l2_claimer_address }`. Validation
(`lib/state/swap.rs::validate_swap_accept()`) requires:

- the swap exists, is open (`l2_recipient == None`), and has not expired
- it is unreserved, or its previous reservation has lapsed — first accept wins
- the transaction **spends an input owned by `l2_claimer_address`**, which
  proves the reserver controls the address they are reserving for

`connect` writes the reservation to the `swap_reservations` database. The
reservation is **coordination, not entitlement**: it tells other takers the
swap is spoken for, so two of them do not both pay on L1 and one eat the loss.
Whoever proves a payment committing to their address gets the escrow, reserved
or not. Reservations lapse after `ParentChainType::accept_expiration_blocks()`.

### 4. Swap Claiming (Bob)

1. **Bob creates SwapClaim** (`claim_swap`), carrying an `L1PaymentProof` in
   `proof_data`: the parent-chain block header and merkle branch from
   `gettxoutproof [txid]`, plus the raw payment from `getrawtransaction`. The
   node builds it from its configured parent-chain RPC, or the taker passes it
   as hex (`--l1-proof`). See `lib/state/l1_proof.rs`.

2. **Consensus validation** (`validate_swap_claim_consensus()`), identical on
   the mempool path and in block validation, anchored at the mainchain block
   the L2 block was built against (`Header::prev_main_hash`; the mempool uses
   the tip's). It requires:
   - the claim spends an output locked to this swap, and nothing locked to another
   - the swap record exists (fail closed)
   - the proof's header hashes to a mainchain block the node has validated
     through the enforcer, on the anchor's ancestry, at most
     `max_l1_tx_age_blocks()` deep, and at least `required_confirmations` deep
     counting from the anchor
   - the payment is in that block (merkle branch recomputes the header's root)
   - it pays at least `l1_amount` to the swap's `l1_recipient_address`
   - it carries the `OP_RETURN` commitment to this `swap_id`; the committed
     L2 address is the **entitled claimer** (and must equal `l2_recipient` for
     a pre-specified swap)
   - the full `l2_amount` reaches the entitled claimer; a legacy
     `l2_claimer_address` on the claim, if present, must agree with it

   Every input to these rules is block data or headers derived from block
   data, so every node reaches the same verdict. A block producer cannot take
   an escrow without paying: without a payment there is no proof to include.

3. **Block processing — SwapClaim** (`lib/state/block.rs`): record the proven
   `l1_txid` and committed claimer on the swap, unlock inputs, mark the swap
   `Completed`, save.

4. Bob's L2 address receives the coins; swap is complete.

### Which parent chains

A proof is checked against the mainchain headers this sidechain validates, so
swaps can only be created against that chain (`ParentChainType::Signet` or
`Regtest`, depending on the network; `supports_payment_proofs()`).
`SwapCreate` for any other chain is rejected by consensus, and the recipient
address must parse for that chain. Bitcoin Cash and Litecoin would need a
header relay that does not exist.

### Why the reservation is a separate database

`Swap` is persisted as bincode, a non-self-describing format, and
`State::get_swap` reports a decode failure as `Ok(None)` — a silently missing
swap. Adding a field to the `Swap` record would make every record written by an
earlier version read as "no swap". Keeping reservations in `swap_reservations`
(and the per-height mainchain anchors in `main_anchors`) leaves the `Swap`
record byte-identical.

`Swap::state` is node-local display state. `l1_txid` and `l2_claimer_address`
are written by consensus when a claim connects (from the proof) and, before
that, by local monitoring as a convenience; validation never reads them.

---

## Security Checks (Current Implementation)

### Implemented in code

| Check | Status | Where |
|-------|--------|--------|
| **Swap ID verification** | ✅ | `validate_swap_create()`: computed ID must match tx |
| **Swap uniqueness** | ✅ | `validate_swap_create()`: swap must not already exist |
| **Provable parent chain / valid L1 address** | ✅ | `validate_swap_create()`: `supports_payment_proofs()` and the recipient parses for that chain |
| **Output locking** | ✅ | SwapCreate locks outputs; only SwapClaim can unlock |
| **Locked-input checks** | ✅ | Non-SwapClaim txs cannot spend locked outputs; SwapClaim must spend only this swap’s locks |
| **L1 payment proof** | ✅ | `validate_swap_claim_consensus()` → `l1_proof::verify()`: known header on the anchor's ancestry, merkle inclusion, amount to recipient, commitment to swap |
| **Confirmations threshold** | ✅ | Depth below the L2 header's `prev_main_hash` must be ≥ `required_confirmations` and ≤ `max_l1_tx_age_blocks()` |
| **L1 payment uniqueness** | ✅ | The `OP_RETURN` commitment names one `swap_id`; a payment proves at most one swap |
| **Claim payout binding** | ✅ | Full `l2_amount` must reach the address the payment committed to (`l2_recipient` for pre-specified swaps) |
| **Expiration** | ✅ | Swaps can have `expires_at_height`; expired swaps are marked Cancelled |
| **Reservation is controlled by the reserver** | ✅ | `validate_swap_accept()` requires an input owned by `l2_claimer_address` |
| **Local monitoring (advisory)** | ✅ | RPC discovery by address + amount, `confirmations > 0`, block inclusion; drives `SwapState` for display only |

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

- A `SwapClaim` proves its L1 payment: header + merkle branch + raw
  transaction (`L1PaymentProof`), authenticated against the mainchain header
  chain above and anchored at the L2 header's `prev_main_hash` for the
  confirmation count. See "Swap Claiming" above and `lib/state/l1_proof.rs`.
- The parent-chain RPC is a convenience for discovering payments and building
  proofs; it is not a source of truth for validation.

---

## Advanced Security (Planned / Not in This Repo)

| Feature | Status |
|---------|--------|
| **Merkle proof of L1 tx in block** | Implemented: `L1PaymentProof` in `SwapClaim::proof_data`, verified by consensus (`lib/state/l1_proof.rs`). |
| **Confirmation count from header chain** | Implemented: depth of the proof's block below the L2 header's `prev_main_hash`, using `main_header_infos`. |
| **Header chain per foreign parent chain** | Not present. Only the sidechain's own mainchain is verifiable, so only it can be swapped against. |
| **BMM-based L1 transaction reports** | Not present and not needed: the proof replaces reports. |

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
  The parent-chain proof-of-work, as validated by the enforcer and recorded in
  the mainchain header chain, plus the confirmation depth the swap creator
  chose. The parent-chain RPC (`l1_rpc_configs.json`, GUI "L1 Config" pane or
  `coinshift_app_cli set-l1-config`) is used to discover payments and to build
  proofs; a wrong or hostile RPC can delay a claim or make the node build an
  invalid proof, but cannot make any node accept a claim that was not paid
  for. The application ships local-node defaults with no credentials and no
  third-party endpoint, and warns about plaintext `http://` to remote hosts.

- **Protected against:**  
  - A block producer claiming an escrow without paying on L1 (no proof, no claim).  
  - Front-running an open swap's fill (the payment commits to the claimer).  
  - Reusing one L1 payment for two swaps (the commitment names the swap).  
  - Spending locked outputs (only SwapClaim can unlock).  
  - Wrong recipient/amount, underpayment of the claimer, unknown or shallow
    L1 blocks, blocks off the anchor's ancestry.  
  - Invalid swap ID, duplicate swap, unprovable chain or unparseable L1
    address at creation (`validate_swap_create`).

- **Still trusted:**  
  - The enforcer, for the mainchain header chain itself (as for deposits and
    withdrawals).  
  - The swap creator's choice of `required_confirmations` against parent-chain
    reorgs deeper than that.

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

1. **One parent chain.** Only the chain this sidechain is anchored to can be
   swapped against. Other chains need a header relay validated in consensus.
2. **Proof size.** A claim carries an 80-byte header, a merkle branch and the
   raw L1 transaction; a few hundred bytes to a few kilobytes.
3. **Anchor lag.** Confirmations are counted from the mainchain block the L2
   tip was built against, so a payment becomes claimable only once a sidechain
   block has been produced on top of a mainchain block deep enough below it.
4. **`update_swap_l1_txid` is advisory.** It records a txid for display and
   for building the proof; it does not make a swap claimable.

---

## Summary

- **Implemented:** Swap creation and claim flow, output locking, deterministic
  swap ID, expiration, on-chain reservations (coordination), and
  proof-carrying claims: the L1 payment's inclusion proof, its confirmation
  depth against the L2 header's mainchain anchor, and its `OP_RETURN`
  commitment to the swap and claimer, all enforced by consensus on every node.
  Parent-chain 2WPD security: mainchain header chain, PoW, BMM merge-mining,
  2WPD only from verified mainchain blocks.
- **Advisory:** RPC-based payment discovery and confirmation tracking, for
  display and for building proofs.
- **Not implemented (in this repo):** header relay for foreign parent chains.

This document is intended to match the current codebase and can be updated as features are added or removed.
