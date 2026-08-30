//! Swap validation and processing

use sneed::RoTxn;

use crate::{
    state::{Error, State},
    types::{
        Address, FilledTransaction, GetValue, OutputContent, SwapId, SwapState,
        SwapTxId, Transaction, TxData,
    },
};

/// Total value of the outputs paying `recipient`.
pub(in crate::state) fn amount_paid_to(
    transaction: &Transaction,
    recipient: &Address,
) -> Result<bitcoin::Amount, Error> {
    transaction
        .outputs
        .iter()
        .filter(|output| output.address == *recipient)
        .map(GetValue::get_value)
        .try_fold(bitcoin::Amount::ZERO, |acc, val| acc.checked_add(val))
        .ok_or_else(|| {
            Error::InvalidTransaction(
                "Output value overflow in SwapClaim".to_string(),
            )
        })
}

/// Validate a SwapCreate transaction
pub fn validate_swap_create(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let TxData::SwapCreate {
        swap_id,
        parent_chain: _,
        l1_txid_bytes: _,
        required_confirmations: _,
        l2_recipient,
        l2_amount,
        l1_recipient_address,
        l1_amount,
    } = &transaction.data
    else {
        return Err(Error::InvalidTransaction(
            "Expected SwapCreate transaction".to_string(),
        ));
    };

    // 1. Verify swap ID matches computed ID
    let computed_swap_id = {
        // L2 → L1 swap
        // We need the sender's address - get it from the first input
        let first_input =
            filled_transaction.spent_utxos.first().ok_or_else(|| {
                Error::InvalidTransaction(
                    "SwapCreate must have inputs".to_string(),
                )
            })?;
        let l2_sender_address = first_input.address;
        SwapId::from_l2_to_l1(
            l1_recipient_address,
            bitcoin::Amount::from_sat(*l1_amount),
            &l2_sender_address,
            l2_recipient.as_ref(), // Now optional
        )
    };

    if computed_swap_id.0 != *swap_id {
        return Err(Error::InvalidTransaction(format!(
            "Swap ID mismatch: expected {}, computed {}",
            hex::encode(swap_id),
            computed_swap_id
        )));
    }

    // 2. Verify swap doesn't already exist
    if state.get_swap(rotxn, &computed_swap_id)?.is_some() {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} already exists",
            computed_swap_id
        )));
    }

    // 3. Verify l2_amount > 0
    if *l2_amount == 0 {
        return Err(Error::InvalidTransaction(
            "L2 amount must be greater than zero".to_string(),
        ));
    }

    // 4. Verify transaction has outputs
    if transaction.outputs.is_empty() {
        return Err(Error::InvalidTransaction(
            "Transaction must have at least one output".to_string(),
        ));
    }

    // 5. For L2 → L1 swaps, verify inputs aren't locked and sufficient funds
    // Check that no inputs are locked to another swap
    for (outpoint, _) in &transaction.inputs {
        if let Some(locked_swap_id) =
            state.is_output_locked_to_swap(rotxn, outpoint)?
            && locked_swap_id.0 != *swap_id
        {
            // Check if the locked swap exists and is valid
            match state.get_swap(rotxn, &locked_swap_id) {
                Ok(Some(_)) => {
                    // Swap exists and is valid - this is a real lock
                    return Err(Error::InvalidTransaction(format!(
                        "Input {} is locked to swap {}",
                        outpoint, locked_swap_id
                    )));
                }
                Ok(None) => {
                    // Swap doesn't exist - orphaned lock
                    return Err(Error::InvalidTransaction(format!(
                        "Input {} is locked to non-existent swap {} (orphaned lock). Please run cleanup_orphaned_locks to fix this.",
                        outpoint, locked_swap_id
                    )));
                }
                Err(err) => {
                    // Check if it's a deserialization error (corrupted swap)
                    let err_str = format!("{err:#}");
                    let err_debug = format!("{err:?}");
                    let is_deserialization_error = err_str.contains("Decoding")
                        || err_str.contains("InvalidTagEncoding")
                        || err_str.contains("deserialize")
                        || err_str.contains("bincode")
                        || err_str.contains("Borsh")
                        || err_debug.contains("Decoding")
                        || err_debug.contains("InvalidTagEncoding")
                        || err_debug.contains("deserialize");

                    if is_deserialization_error {
                        // Swap is corrupted - orphaned lock
                        return Err(Error::InvalidTransaction(format!(
                            "Input {} is locked to corrupted swap {} (orphaned lock). Please run cleanup_orphaned_locks to fix this.",
                            outpoint, locked_swap_id
                        )));
                    } else {
                        // Other database error - return original error
                        return Err(Error::InvalidTransaction(format!(
                            "Input {} is locked to swap {}, but error checking swap: {}",
                            outpoint, locked_swap_id, err
                        )));
                    }
                }
            }
        }
    }

    // Verify transaction spends at least l2_amount
    let total_input_value = filled_transaction
        .spent_utxos
        .iter()
        .map(crate::types::GetValue::get_value)
        .try_fold(bitcoin::Amount::ZERO, |acc, val| {
            acc.checked_add(val).ok_or(())
        })
        .map_err(|_| {
            Error::InvalidTransaction("Input value overflow".to_string())
        })?;

    let required_amount = bitcoin::Amount::from_sat(*l2_amount);
    if total_input_value < required_amount {
        return Err(Error::InvalidTransaction(format!(
            "Insufficient funds: need {}, have {}",
            required_amount, total_input_value
        )));
    }

    // The declared `l2_amount` becomes a consensus obligation once the swap is
    // saved: a claim must pay the recipient at least `swap.l2_amount`. Spending
    // enough inputs is not sufficient, since only the `SwapPending` outputs
    // carrying this swap's id are locked when the block connects; the rest is
    // change the creator keeps. Require those outputs to actually escrow the
    // declared amount, otherwise a creator could declare an inflated
    // `l2_amount` while locking a token value, leaving the L1 filler unable to
    // claim after having already paid on L1.
    let escrowed_value = transaction
        .outputs
        .iter()
        .filter_map(|output| match output.content {
            OutputContent::SwapPending {
                value,
                swap_id: output_swap_id,
            } if output_swap_id == *swap_id => Some(value),
            _ => None,
        })
        .try_fold(bitcoin::Amount::ZERO, |acc, val| {
            acc.checked_add(val).ok_or(())
        })
        .map_err(|_| {
            Error::InvalidTransaction(
                "SwapPending output value overflow".to_string(),
            )
        })?;

    if escrowed_value < required_amount {
        return Err(Error::InvalidTransaction(format!(
            "SwapCreate must lock at least {} in SwapPending outputs for swap {}, but locks {}",
            required_amount, computed_swap_id, escrowed_value
        )));
    }

    Ok(())
}

/// Height of the block currently being validated.
///
/// `try_get_height` returns the tip's height, so the block under validation is
/// the next one. `prevalidate_block`, `connect_block` and the mempool path all
/// run against the same tip, so they all agree on this value.
fn validating_height(state: &State, rotxn: &RoTxn) -> Result<u32, Error> {
    Ok(state.try_get_height(rotxn)?.map_or(0, |height| height + 1))
}

/// Validate a `SwapAccept`: a reservation of an open swap for a claimer.
///
/// Every rule here is decided from block data alone, which is the point — the
/// resulting `Swap::accepted_by` is identical on every node, so claim
/// validation can rely on it (see [`validate_swap_claim_consensus`]).
pub fn validate_swap_accept(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let TxData::SwapAccept {
        swap_id,
        l2_claimer_address,
    } = &transaction.data
    else {
        return Err(Error::InvalidTransaction(
            "Expected SwapAccept transaction".to_string(),
        ));
    };
    let swap_id = SwapId(*swap_id);

    // A reservation must not double as a way to move escrowed value.
    let () = validate_no_locked_outputs(state, rotxn, transaction)?;

    let swap = state
        .get_swap(rotxn, &swap_id)?
        .ok_or(Error::SwapNotFound { swap_id })?;

    if swap.l2_recipient.is_some() {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} is not an open swap; its recipient was fixed at creation",
            swap_id
        )));
    }

    let height = validating_height(state, rotxn)?;

    if let Some(expires_at) = swap.expires_at_height
        && height >= expires_at
    {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} expired at height {}, cannot be accepted at {}",
            swap_id, expires_at, height
        )));
    }

    // First reservation wins while it is live; a lapsed one releases the swap.
    if let Some(reservation) = state.get_swap_reservation(rotxn, &swap_id)?
        && reservation.is_live_at(swap.parent_chain, height)
    {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} is already reserved by {} until height {}",
            swap_id,
            reservation.claimer,
            reservation.expires_at(swap.parent_chain),
        )));
    }

    // Proof that the reserver controls the address they are reserving for:
    // spend an input owned by it. `prevalidate`/`connect` already require every
    // authorization to match its spent output's address (SwapAccept gets no
    // exemption), so this input carries a signature from that key.
    //
    // Without it, anyone could reserve every open swap for an address they do
    // not own and stall the market for free.
    if !filled_transaction
        .spent_utxos
        .iter()
        .any(|utxo| utxo.address == *l2_claimer_address)
    {
        return Err(Error::InvalidTransaction(format!(
            "SwapAccept must spend an input owned by {}, the address it reserves for",
            l2_claimer_address
        )));
    }

    Ok(())
}

/// Validate a SwapClaim transaction in the mempool.
///
/// This is [`validate_swap_claim_consensus`] plus the node-local checks that
/// consensus deliberately omits: that this node has seen the swap reach
/// `ReadyToClaim`, and for open swaps that it has observed the L1 fill. Those
/// come from each node's own parent-chain monitoring, so they can legitimately
/// differ between nodes and must never gate block validation.
///
/// Everything that decides *who gets paid* now lives in the consensus half, so
/// the two paths cannot drift apart the way they had.
pub fn validate_swap_claim(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    _filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let TxData::SwapClaim { swap_id, .. } = &transaction.data else {
        return Err(Error::InvalidTransaction(
            "Expected SwapClaim transaction".to_string(),
        ));
    };

    let swap_id = SwapId(*swap_id);

    let swap = state
        .get_swap(rotxn, &swap_id)?
        .ok_or(Error::SwapNotFound { swap_id })?;

    if !matches!(swap.state, SwapState::ReadyToClaim) {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} is not ready to claim (state: {:?})",
            swap_id, swap.state
        )));
    }

    // For open swaps, verify this node has seen the L1 side filled.
    if swap.l2_recipient.is_none() {
        let zero_hash32 = [0u8; 32];
        let has_l1_tx = !matches!(swap.l1_txid, SwapTxId::Hash32(h) if h == zero_hash32)
            && !matches!(swap.l1_txid, SwapTxId::Hash(ref v) if v.is_empty() || v.iter().all(|&b| b == 0));

        if !has_l1_tx {
            return Err(Error::InvalidTransaction(
                "Open swap cannot be claimed until L1 transaction is detected"
                    .to_string(),
            ));
        }
    }

    validate_swap_claim_consensus(state, rotxn, transaction)
}

/// Validate that non-SwapClaim transactions don't spend locked outputs
pub fn validate_no_locked_outputs(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
) -> Result<(), Error> {
    // Skip validation for SwapClaim transactions
    if matches!(transaction.data, TxData::SwapClaim { .. }) {
        return Ok(());
    }

    // Check that no inputs are locked
    for (outpoint, _) in &transaction.inputs {
        if let Some(locked_swap_id) =
            state.is_output_locked_to_swap(rotxn, outpoint)?
        {
            return Err(Error::InvalidTransaction(format!(
                "Cannot spend locked output {} (locked to swap {})",
                outpoint, locked_swap_id
            )));
        }
    }

    Ok(())
}

/// Validate the deterministic swap rules for a `SwapClaim` during block
/// validation.
///
/// Unlike [`validate_swap_claim`], this does not inspect the swap's `state`
/// (`ReadyToClaim`) or whether an L1 transaction has been detected. Those are
/// derived from each node's own parent-chain monitoring (see
/// `two_way_peg_data::query_and_update_swap`) and are not part of consensus, so
/// `connect` trusts the block and advances the local state to match. Enforcing
/// them here would let a node that has not yet observed the L1 fill reject an
/// otherwise valid block.
///
/// What *is* enforced: the claim spends an output locked to its swap, spends
/// nothing locked to another swap, and pays the full `l2_amount` to the address
/// entitled to it — the recipient fixed at creation for a pre-specified swap,
/// or the holder of a live `SwapAccept` reservation for an open one. Both come
/// from block data, so every node reaches the same verdict.
pub fn validate_swap_claim_consensus(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
) -> Result<(), Error> {
    let TxData::SwapClaim {
        swap_id,
        l2_claimer_address,
        ..
    } = &transaction.data
    else {
        return Err(Error::InvalidTransaction(
            "Expected SwapClaim transaction".to_string(),
        ));
    };
    let swap_id = SwapId(*swap_id);

    // The claim must spend at least one output locked to this swap, and must
    // not spend any output locked to a different swap.
    let mut found_locked_input = false;
    for (outpoint, _) in &transaction.inputs {
        if let Some(locked_swap_id) =
            state.is_output_locked_to_swap(rotxn, outpoint)?
        {
            if locked_swap_id != swap_id {
                return Err(Error::InvalidTransaction(format!(
                    "Input {} is locked to different swap {}",
                    outpoint, locked_swap_id
                )));
            }
            found_locked_input = true;
        }
    }
    if !found_locked_input {
        return Err(Error::InvalidTransaction(
            "SwapClaim must spend at least one output locked to the swap"
                .to_string(),
        ));
    }

    let height = validating_height(state, rotxn)?;

    // Fail closed if the swap record cannot be read. `State::get_swap` reports a
    // corrupted record as `Ok(None)`, and skipping the payout rule in that case
    // would let the escrow be spent to anywhere — on whichever nodes happened to
    // hit the bad record, which is also how nodes would come to disagree.
    let swap = state
        .get_swap(rotxn, &swap_id)?
        .ok_or(Error::SwapNotFound { swap_id })?;
    {
        // Who is entitled to the escrow, according to the chain alone.
        let Some(expected_recipient) =
            state.entitled_claimer_at(rotxn, &swap, height)?
        else {
            return Err(Error::InvalidTransaction(format!(
                "Open swap {} has no live reservation at height {}; it must be \
                 reserved by a SwapAccept before it can be claimed",
                swap_id, height
            )));
        };

        // The claim may still carry the legacy `l2_claimer_address` field. If
        // it does, it must agree with the reservation rather than override it.
        if let Some(declared) = l2_claimer_address
            && *declared != expected_recipient
        {
            return Err(Error::InvalidTransaction(format!(
                "SwapClaim declares claimer {} but swap {} is reserved for {}",
                declared, swap_id, expected_recipient
            )));
        }

        // Paying the recipient a token amount and keeping the rest is the same
        // theft with an extra output, so require the full amount.
        let amount_to_recipient =
            amount_paid_to(transaction, &expected_recipient)?;
        if amount_to_recipient < swap.l2_amount {
            return Err(Error::InvalidTransaction(format!(
                "SwapClaim must pay at least {} to {}, but pays {}",
                swap.l2_amount, expected_recipient, amount_to_recipient
            )));
        }
    }

    Ok(())
}

/// Validate swap consensus rules for a single transaction in a block.
///
/// Block validation must enforce the same swap rules as the mempool path in
/// [`State::validate_transaction`]; otherwise a miner could include a swap
/// transaction that no node would accept from the mempool, e.g. a regular
/// transaction spending a swap-locked output, or a claim that pays the attacker
/// instead of the swap recipient. `SwapClaim` uses the consensus subset that
/// ignores node-local L1 state; see [`validate_swap_claim_consensus`].
pub fn validate_block_transaction(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    match &transaction.data {
        TxData::SwapCreate { .. } => {
            crate::state::hash_lock::reject_hash_locked_inputs(
                filled_transaction,
            )?;
            validate_swap_create(state, rotxn, transaction, filled_transaction)
        }
        TxData::SwapClaim { .. } => {
            crate::state::hash_lock::reject_hash_locked_inputs(
                filled_transaction,
            )?;
            validate_swap_claim_consensus(state, rotxn, transaction)
        }
        TxData::SwapAccept { .. } => {
            crate::state::hash_lock::reject_hash_locked_inputs(
                filled_transaction,
            )?;
            validate_swap_accept(state, rotxn, transaction, filled_transaction)
        }
        TxData::HashLockClaim { .. } => {
            crate::state::hash_lock::validate_hash_lock_claim(
                transaction,
                filled_transaction,
            )
        }
        TxData::Regular => {
            // An ordinary transaction may reclaim a hash lock once it has timed
            // out; anything else it spends is governed as before.
            crate::state::hash_lock::validate_hash_lock_refund(
                state,
                rotxn,
                transaction,
                filled_transaction,
            )?;
            validate_no_locked_outputs(state, rotxn, transaction)
        }
    }
}

#[cfg(test)]
mod tests {
    use sneed::Env;

    use super::*;
    use crate::types::{
        Address, OutPoint, Output, OutputContent, ParentChainType, Swap,
        SwapDirection, SwapTxId, Transaction, Txid,
    };

    fn sat(value: u64) -> bitcoin::Amount {
        bitcoin::Amount::from_sat(value)
    }

    /// Build a `State` backed by a fresh temporary LMDB environment.
    fn test_state() -> (temp_dir::TempDir, Env, State) {
        let dir = temp_dir::TempDir::new().unwrap();
        let mut opts = heed::EnvOpenOptions::new();
        opts.map_size(10 * 1024 * 1024).max_dbs(State::NUM_DBS);
        let env = unsafe { Env::open(&opts, dir.path()) }.unwrap();
        let state = State::new(&env).unwrap();
        (dir, env, state)
    }

    /// A regular (non-claim) transaction that spends a swap-locked output must
    /// be rejected by block validation, just as it is in the mempool.
    #[test]
    fn block_validation_rejects_spending_locked_output() {
        let (_dir, env, state) = test_state();
        let swap_id = SwapId([7u8; 32]);
        let outpoint = OutPoint::Regular {
            txid: Txid([9u8; 32]),
            vout: 0,
        };
        let locked_output = Output {
            address: Address([1u8; 20]),
            content: OutputContent::SwapPending {
                value: sat(50_000),
                swap_id: swap_id.0,
            },
        };

        let mut rwtxn = env.write_txn().unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
            .unwrap();
        rwtxn.commit().unwrap();

        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: Address([2u8; 20]),
                content: OutputContent::Value(sat(50_000)),
            }],
            data: TxData::Regular,
        };
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "spending a locked output in a regular tx should be rejected, got {result:?}"
        );
    }

    fn pre_specified_swap_state() -> (
        temp_dir::TempDir,
        Env,
        State,
        SwapId,
        OutPoint,
        Output,
        Address,
    ) {
        let (dir, env, state) = test_state();
        let recipient = Address([3u8; 20]);
        let creator = Address([5u8; 20]);
        let swap_id = SwapId([8u8; 32]);
        let outpoint = OutPoint::Regular {
            txid: Txid([1u8; 32]),
            vout: 0,
        };
        let locked_output = Output {
            address: recipient,
            content: OutputContent::SwapPending {
                value: sat(50_000),
                swap_id: swap_id.0,
            },
        };
        let swap = Swap::new(
            swap_id,
            SwapDirection::L2ToL1,
            ParentChainType::Regtest,
            SwapTxId::Hash32([0u8; 32]),
            None,
            Some(recipient),
            sat(50_000),
            "rbtc-recipient".to_string(),
            sat(40_000),
            0,
            None,
            Some(creator),
        );

        let mut rwtxn = env.write_txn().unwrap();
        state.save_swap(&mut rwtxn, &swap).unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
            .unwrap();
        rwtxn.commit().unwrap();

        (dir, env, state, swap_id, outpoint, locked_output, recipient)
    }

    /// A claim of a pre-specified swap that pays someone other than the
    /// recipient fixed at creation must be rejected by block validation.
    #[test]
    fn block_validation_rejects_claim_to_wrong_recipient() {
        let (_dir, env, state, swap_id, outpoint, locked_output, _recipient) =
            pre_specified_swap_state();

        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: Address([4u8; 20]), // attacker, not the recipient
                content: OutputContent::Value(sat(49_000)),
            }],
            data: TxData::SwapClaim {
                swap_id: swap_id.0,
                l2_claimer_address: None,
                proof_data: None,
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "claim paying the wrong recipient should be rejected, got {result:?}"
        );
    }

    /// A claim that does pay the pre-specified recipient and spends the locked
    /// output must still pass block validation.
    #[test]
    fn block_validation_accepts_claim_to_recipient() {
        let (_dir, env, state, swap_id, outpoint, locked_output, recipient) =
            pre_specified_swap_state();

        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: recipient,
                content: OutputContent::Value(sat(50_000)),
            }],
            data: TxData::SwapClaim {
                swap_id: swap_id.0,
                l2_claimer_address: None,
                proof_data: None,
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "claim paying the recipient should be accepted, got {result:?}"
        );
    }

    /// A claim of a pre-specified swap that pays the recipient less than the
    /// swapped amount must be rejected by block validation; otherwise a claimer
    /// could pay the recipient a token amount and keep the rest of the locked
    /// value.
    #[test]
    fn block_validation_rejects_underpaying_claim() {
        let (_dir, env, state, swap_id, outpoint, locked_output, recipient) =
            pre_specified_swap_state();

        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![
                Output {
                    address: recipient,
                    content: OutputContent::Value(sat(1)),
                },
                Output {
                    address: Address([4u8; 20]), // attacker keeps the rest
                    content: OutputContent::Value(sat(49_999)),
                },
            ],
            data: TxData::SwapClaim {
                swap_id: swap_id.0,
                l2_claimer_address: None,
                proof_data: None,
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "claim underpaying the recipient should be rejected, got {result:?}"
        );
    }

    fn ready_swap_state() -> (
        temp_dir::TempDir,
        Env,
        State,
        SwapId,
        OutPoint,
        Output,
        Address,
    ) {
        let (dir, env, state) = test_state();
        let recipient = Address([6u8; 20]);
        let swap_id = SwapId([11u8; 32]);
        let outpoint = OutPoint::Regular {
            txid: Txid([2u8; 32]),
            vout: 0,
        };
        let locked_output = Output {
            address: recipient,
            content: OutputContent::SwapPending {
                value: sat(50_000),
                swap_id: swap_id.0,
            },
        };
        let mut swap = Swap::new(
            swap_id,
            SwapDirection::L2ToL1,
            ParentChainType::Regtest,
            SwapTxId::Hash32([0u8; 32]),
            None,
            Some(recipient),
            sat(50_000),
            "rbtc-recipient".to_string(),
            sat(40_000),
            0,
            None,
            Some(Address([5u8; 20])),
        );
        swap.state = SwapState::ReadyToClaim;

        let mut rwtxn = env.write_txn().unwrap();
        state.save_swap(&mut rwtxn, &swap).unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
            .unwrap();
        rwtxn.commit().unwrap();

        (dir, env, state, swap_id, outpoint, locked_output, recipient)
    }

    /// The mempool claim validator must also require the recipient to receive
    /// the full amount, not merely some output.
    #[test]
    fn mempool_claim_rejects_underpayment() {
        let (_dir, env, state, swap_id, outpoint, locked_output, recipient) =
            ready_swap_state();

        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: recipient,
                content: OutputContent::Value(sat(10_000)),
            }],
            data: TxData::SwapClaim {
                swap_id: swap_id.0,
                l2_claimer_address: None,
                proof_data: None,
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_claim(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "underpaying claim should be rejected by the mempool validator, got {result:?}"
        );
    }

    /// A claim that pays the recipient the full amount passes the mempool
    /// validator.
    #[test]
    fn mempool_claim_accepts_full_payment() {
        let (_dir, env, state, swap_id, outpoint, locked_output, recipient) =
            ready_swap_state();

        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: recipient,
                content: OutputContent::Value(sat(50_000)),
            }],
            data: TxData::SwapClaim {
                swap_id: swap_id.0,
                l2_claimer_address: None,
                proof_data: None,
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_claim(&state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "full-amount claim should pass the mempool validator, got {result:?}"
        );
    }

    /// Set the tip height, so that `validating_height` reports `height`.
    fn set_height(env: &Env, state: &State, height: u32) {
        let mut rwtxn = env.write_txn().unwrap();
        // Validation looks at the *next* block, so store one below.
        state
            .height
            .put(&mut rwtxn, &(), &height.saturating_sub(1))
            .unwrap();
        rwtxn.commit().unwrap();
    }

    /// An open swap (`l2_recipient == None`) whose L1 fill has been detected
    /// and which is `ReadyToClaim`. `reservation` is an on-chain `SwapAccept`
    /// binding — `(claimer, accepted_at_height)`.
    fn open_swap_state(
        reservation: Option<(Address, u32)>,
    ) -> (temp_dir::TempDir, Env, State, SwapId, OutPoint, Output) {
        let (dir, env, state) = test_state();
        let creator = Address([5u8; 20]);
        let swap_id = SwapId([12u8; 32]);
        let outpoint = OutPoint::Regular {
            txid: Txid([3u8; 32]),
            vout: 0,
        };
        let locked_output = Output {
            address: creator,
            content: OutputContent::SwapPending {
                value: sat(50_000),
                swap_id: swap_id.0,
            },
        };
        let mut swap = Swap::new(
            swap_id,
            SwapDirection::L2ToL1,
            ParentChainType::Regtest,
            // Non-zero: an L1 fill has been observed for this swap.
            SwapTxId::Hash32([0xaa; 32]),
            None,
            None, // open swap: no recipient fixed at creation
            sat(50_000),
            "rbtc-recipient".to_string(),
            sat(40_000),
            0,
            None,
            Some(creator),
        );
        swap.state = SwapState::ReadyToClaim;

        let mut rwtxn = env.write_txn().unwrap();
        state.save_swap(&mut rwtxn, &swap).unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
            .unwrap();
        if let Some((claimer, height)) = reservation {
            state
                .reserve_swap(&mut rwtxn, &swap_id, claimer, height)
                .unwrap();
        }
        rwtxn.commit().unwrap();

        (dir, env, state, swap_id, outpoint, locked_output)
    }

    fn open_claim_tx(
        swap_id: SwapId,
        outpoint: OutPoint,
        declared_claimer: Option<Address>,
        paid_to: Address,
        paid: bitcoin::Amount,
    ) -> Transaction {
        Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: paid_to,
                content: OutputContent::Value(paid),
            }],
            data: TxData::SwapClaim {
                swap_id: swap_id.0,
                l2_claimer_address: declared_claimer,
                proof_data: None,
            },
        }
    }

    fn accept_tx(
        swap_id: SwapId,
        claimer: Address,
        funding_owner: Address,
    ) -> (Transaction, FilledTransaction) {
        let outpoint = OutPoint::Regular {
            txid: Txid([42u8; 32]),
            vout: 0,
        };
        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: funding_owner,
                content: OutputContent::Value(sat(900)),
            }],
            data: TxData::SwapAccept {
                swap_id: swap_id.0,
                l2_claimer_address: claimer,
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![Output {
                address: funding_owner,
                content: OutputContent::Value(sat(1_000)),
            }],
            transaction: tx.clone(),
        };
        (tx, filled)
    }

    /// This is the attack from the bug report: spend an open swap's escrow and
    /// pay yourself. With no reservation on the swap, no address is entitled to
    /// it, so the claim is invalid — on the block path, where it counts.
    #[test]
    fn block_validation_rejects_claim_on_unreserved_open_swap() {
        let (_dir, env, state, swap_id, outpoint, locked_output) =
            open_swap_state(None);
        set_height(&env, &state, 10);
        let attacker = Address([4u8; 20]);

        let tx = open_claim_tx(
            swap_id,
            outpoint,
            Some(attacker),
            attacker,
            sat(50_000),
        );
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "claim on an unreserved open swap must be rejected, got {result:?}"
        );
    }

    /// The same attack against a swap someone else reserved: declaring your own
    /// address no longer helps, because the entitled address comes from the
    /// chain, not from the claim.
    #[test]
    fn block_validation_rejects_claim_paying_attacker_over_reserver() {
        let reserver = Address([7u8; 20]);
        let (_dir, env, state, swap_id, outpoint, locked_output) =
            open_swap_state(Some((reserver, 10)));
        set_height(&env, &state, 12);
        let attacker = Address([4u8; 20]);

        let tx = open_claim_tx(
            swap_id,
            outpoint,
            Some(attacker),
            attacker,
            sat(50_000),
        );
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "claim paying anyone but the reserver must be rejected, got {result:?}"
        );
    }

    /// Paying the reserver a token amount and keeping the rest is the same
    /// theft with an extra output.
    #[test]
    fn block_validation_rejects_claim_underpaying_reserver() {
        let reserver = Address([7u8; 20]);
        let (_dir, env, state, swap_id, outpoint, locked_output) =
            open_swap_state(Some((reserver, 10)));
        set_height(&env, &state, 12);

        let tx =
            open_claim_tx(swap_id, outpoint, Some(reserver), reserver, sat(1));
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "claim underpaying the reserver must be rejected, got {result:?}"
        );
    }

    /// The honest claim: the reserver takes the escrow they paid for on L1.
    #[test]
    fn block_validation_accepts_claim_paying_reserver() {
        let reserver = Address([7u8; 20]);
        let (_dir, env, state, swap_id, outpoint, locked_output) =
            open_swap_state(Some((reserver, 10)));
        set_height(&env, &state, 12);

        let tx = open_claim_tx(
            swap_id,
            outpoint,
            Some(reserver),
            reserver,
            sat(50_000),
        );
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "reserver's claim should pass block validation, got {result:?}"
        );
    }

    /// A claim omitting `l2_claimer_address` entirely still has to pay the
    /// reserver — the field is advisory now, not the source of truth.
    #[test]
    fn block_validation_accepts_claim_paying_reserver_without_declaration() {
        let reserver = Address([7u8; 20]);
        let (_dir, env, state, swap_id, outpoint, locked_output) =
            open_swap_state(Some((reserver, 10)));
        set_height(&env, &state, 12);

        let tx = open_claim_tx(swap_id, outpoint, None, reserver, sat(50_000));
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "undeclared claim paying the reserver should pass, got {result:?}"
        );
    }

    /// Once the reservation lapses the escrow is unassigned again, so the old
    /// reserver cannot claim on the strength of an expired hold.
    #[test]
    fn block_validation_rejects_claim_after_reservation_lapses() {
        let reserver = Address([7u8; 20]);
        let (_dir, env, state, swap_id, outpoint, locked_output) =
            open_swap_state(Some((reserver, 10)));
        let window = ParentChainType::Regtest.accept_expiration_blocks();
        set_height(&env, &state, 10 + window);

        let tx = open_claim_tx(
            swap_id,
            outpoint,
            Some(reserver),
            reserver,
            sat(50_000),
        );
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "claim on a lapsed reservation must be rejected, got {result:?}"
        );
    }

    /// The mempool inherits the consensus rule, so the two paths cannot
    /// disagree about who gets paid.
    #[test]
    fn mempool_rejects_claim_on_unreserved_open_swap() {
        let (_dir, env, state, swap_id, outpoint, locked_output) =
            open_swap_state(None);
        set_height(&env, &state, 10);
        let attacker = Address([4u8; 20]);

        let tx = open_claim_tx(
            swap_id,
            outpoint,
            Some(attacker),
            attacker,
            sat(50_000),
        );
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_claim(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "mempool must reject an unreserved open claim, got {result:?}"
        );
    }

    #[test]
    fn swap_accept_reserves_open_swap() {
        let (_dir, env, state, swap_id, ..) = open_swap_state(None);
        set_height(&env, &state, 10);
        let claimer = Address([7u8; 20]);

        let (tx, filled) = accept_tx(swap_id, claimer, claimer);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(result.is_ok(), "valid accept should pass, got {result:?}");
    }

    /// Proof of control: reserving for an address you do not own would let
    /// anyone park every open swap for free.
    #[test]
    fn swap_accept_rejects_claimer_without_matching_input() {
        let (_dir, env, state, swap_id, ..) = open_swap_state(None);
        set_height(&env, &state, 10);
        let claimer = Address([7u8; 20]);
        let someone_else = Address([9u8; 20]);

        let (tx, filled) = accept_tx(swap_id, claimer, someone_else);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "accept without an input from the claimer must be rejected, got {result:?}"
        );
    }

    /// First reservation wins while it is live.
    #[test]
    fn swap_accept_rejects_double_reservation() {
        let first = Address([7u8; 20]);
        let (_dir, env, state, swap_id, ..) =
            open_swap_state(Some((first, 10)));
        set_height(&env, &state, 12);
        let second = Address([9u8; 20]);

        let (tx, filled) = accept_tx(swap_id, second, second);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "second accept during a live reservation must be rejected, got {result:?}"
        );
    }

    /// ...but a lapsed reservation releases the swap, so a griefer can only
    /// stall the market for one window per fee paid.
    #[test]
    fn swap_accept_allowed_after_reservation_lapses() {
        let first = Address([7u8; 20]);
        let (_dir, env, state, swap_id, ..) =
            open_swap_state(Some((first, 10)));
        let window = ParentChainType::Regtest.accept_expiration_blocks();
        set_height(&env, &state, 10 + window);
        let second = Address([9u8; 20]);

        let (tx, filled) = accept_tx(swap_id, second, second);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "accept after the window should pass, got {result:?}"
        );
    }

    /// Pre-specified swaps already have a recipient; reserving one is
    /// meaningless and must not overwrite it.
    #[test]
    fn swap_accept_rejects_pre_specified_swap() {
        let (_dir, env, state, swap_id, ..) = pre_specified_swap_state();
        set_height(&env, &state, 10);
        let claimer = Address([7u8; 20]);

        let (tx, filled) = accept_tx(swap_id, claimer, claimer);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "accepting a pre-specified swap must be rejected, got {result:?}"
        );
    }

    /// Build a `SwapCreate` declaring `l2_amount`, escrowing `escrowed` in a
    /// `SwapPending` output and keeping `change` as a regular output, funded by
    /// a single input worth `l2_amount` plus a fee.
    fn swap_create_tx(
        l2_amount: u64,
        escrowed: u64,
        change: u64,
    ) -> (Transaction, FilledTransaction) {
        let sender = Address([12u8; 20]);
        let recipient = Address([13u8; 20]);
        let l1_recipient_address = "rbtc-recipient".to_string();
        let l1_amount = sat(40_000);
        let swap_id = SwapId::from_l2_to_l1(
            &l1_recipient_address,
            l1_amount,
            &sender,
            Some(&recipient),
        );
        let funding = Output {
            address: sender,
            content: OutputContent::Value(sat(l2_amount + 1_000)),
        };
        let tx = Transaction {
            inputs: vec![(
                OutPoint::Regular {
                    txid: Txid([3u8; 32]),
                    vout: 0,
                },
                [0u8; 32],
            )],
            proof: Default::default(),
            outputs: vec![
                Output {
                    address: recipient,
                    content: OutputContent::SwapPending {
                        value: sat(escrowed),
                        swap_id: swap_id.0,
                    },
                },
                Output {
                    address: sender,
                    content: OutputContent::Value(sat(change)),
                },
            ],
            data: TxData::SwapCreate {
                swap_id: swap_id.0,
                parent_chain: ParentChainType::Regtest,
                l1_txid_bytes: vec![0u8; 32],
                required_confirmations: 1,
                l2_recipient: Some(recipient),
                l2_amount,
                l1_recipient_address,
                l1_amount: l1_amount.to_sat(),
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![funding],
            transaction: tx.clone(),
        };
        (tx, filled)
    }

    /// A `SwapCreate` that declares an `l2_amount` larger than the value it
    /// actually escrows in `SwapPending` outputs must be rejected. Only those
    /// outputs are locked, so the remainder is change the creator keeps while
    /// the claim is still obliged to pay the recipient the declared amount.
    #[test]
    fn swap_create_rejects_under_escrowed_amount() {
        let (_dir, env, state) = test_state();
        let (tx, filled) = swap_create_tx(10_000, 1_000, 9_000);

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "under-escrowed SwapCreate should be rejected, got {result:?}"
        );
    }

    /// The wallet-shaped `SwapCreate` — the full `l2_amount` in a single
    /// `SwapPending` output plus a change output — must still be accepted.
    #[test]
    fn swap_create_accepts_fully_escrowed_amount() {
        let (_dir, env, state) = test_state();
        let (tx, filled) = swap_create_tx(10_000, 10_000, 500);

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "fully escrowed SwapCreate should be accepted, got {result:?}"
        );
    }

    /// `SwapPending` outputs carrying a different swap's id do not count
    /// towards the escrow, since block connection does not lock them for this
    /// swap.
    #[test]
    fn swap_create_rejects_escrow_tagged_with_other_swap() {
        let (_dir, env, state) = test_state();
        let (mut tx, filled) = swap_create_tx(10_000, 10_000, 500);
        tx.outputs[0].content = OutputContent::SwapPending {
            value: sat(10_000),
            swap_id: [42u8; 32],
        };
        let filled = FilledTransaction {
            spent_utxos: filled.spent_utxos,
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "escrow tagged with another swap should not count, got {result:?}"
        );
    }

    /// Reversing the block that created a swap must always succeed, even when
    /// local L1 monitoring has already advanced the swap past `Pending` (e.g. to
    /// `ReadyToClaim`). `disconnect_tip` deletes the swap via
    /// `delete_swap_unchecked`, so that helper must never refuse based on state;
    /// otherwise a sidechain reorg that removes the creating block aborts and the
    /// node is wedged on the losing branch.
    #[test]
    fn delete_swap_unchecked_deletes_ready_to_claim_swap() {
        let (_dir, env, state, swap_id, ..) = ready_swap_state();

        let mut rwtxn = env.write_txn().unwrap();
        let result = state.delete_swap_unchecked(&mut rwtxn, &swap_id);
        assert!(
            result.is_ok(),
            "rollback deletion of a ReadyToClaim swap must succeed, got {result:?}"
        );
        assert!(
            state.get_swap(&rwtxn, &swap_id).unwrap().is_none(),
            "swap should be gone after rollback deletion"
        );
    }
}
