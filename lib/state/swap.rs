//! Swap validation and processing

use sneed::RoTxn;

use crate::{
    state::{Error, State},
    types::{
        Address, FilledTransaction, GetValue, SwapId, SwapState, SwapTxId,
        Transaction, TxData,
    },
};

/// Total value of the outputs paying `recipient`.
fn amount_paid_to(
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

    Ok(())
}

/// Validate a SwapClaim transaction
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

    // 1. Verify swap exists
    let swap = state
        .get_swap(rotxn, &swap_id)?
        .ok_or_else(|| Error::SwapNotFound { swap_id })?;

    // 2. Verify swap is in ReadyToClaim state
    if !matches!(swap.state, SwapState::ReadyToClaim) {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} is not ready to claim (state: {:?})",
            swap_id, swap.state
        )));
    }

    // 2.5. For open swaps, verify L1 transaction exists (someone filled it)
    if swap.l2_recipient.is_none() {
        // Open swap - verify L1 transaction was detected
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

    // 3. Verify at least one input is locked to this swap
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

    // 4. Verify output goes to correct recipient
    let TxData::SwapClaim {
        l2_claimer_address, ..
    } = &transaction.data
    else {
        unreachable!()
    };

    let expected_recipient = if let Some(recipient) = swap.l2_recipient {
        // Pre-specified swap: must go to specified recipient
        recipient
    } else {
        // Open swap
        if let Some(stored_l2) = swap.l2_claimer_address {
            // Claim only valid for the L2 address the filler declared when providing L1 tx details
            if l2_claimer_address.as_ref() != Some(&stored_l2) {
                return Err(Error::InvalidTransaction(
                    "Open swap claim must use the L2 address declared when the L1 transaction was submitted".to_string(),
                ));
            }
            stored_l2
        } else if let Some(claimer_addr) = l2_claimer_address {
            // No stored L2 (e.g. auto-detected L1 tx): accept claimer address from tx
            *claimer_addr
        } else {
            return Err(Error::InvalidTransaction(
                "Open swap claim requires l2_claimer_address".to_string(),
            ));
        }
    };

    // The recipient must receive the full swapped amount. Checking only that
    // some output pays the recipient lets a claimer spend the locked output
    // while paying the recipient a token amount and keeping the rest.
    let amount_to_recipient = amount_paid_to(transaction, &expected_recipient)?;
    if amount_to_recipient < swap.l2_amount {
        return Err(Error::InvalidTransaction(format!(
            "SwapClaim must pay at least {} to {}, but pays {}",
            swap.l2_amount, expected_recipient, amount_to_recipient
        )));
    }

    Ok(())
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
/// otherwise valid block. Only the rules that follow deterministically from
/// on-chain data are checked: that the claim spends an output locked to its
/// swap, spends nothing locked to another swap, and, for pre-specified swaps,
/// pays the recipient fixed at creation.
pub fn validate_swap_claim_consensus(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
) -> Result<(), Error> {
    let TxData::SwapClaim { swap_id, .. } = &transaction.data else {
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

    // For pre-specified swaps both the recipient and the swapped amount are
    // fixed at creation, so consensus can require the claim to pay the recipient
    // the full amount. Open swaps derive their recipient from node-local L1
    // monitoring, so that binding is left to the mempool check.
    if let Some(swap) = state.get_swap(rotxn, &swap_id)?
        && let Some(recipient) = swap.l2_recipient
    {
        let amount_to_recipient = amount_paid_to(transaction, &recipient)?;
        if amount_to_recipient < swap.l2_amount {
            return Err(Error::InvalidTransaction(format!(
                "SwapClaim must pay at least {} to {}, but pays {}",
                swap.l2_amount, recipient, amount_to_recipient
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
            validate_swap_create(state, rotxn, transaction, filled_transaction)
        }
        TxData::SwapClaim { .. } => {
            validate_swap_claim_consensus(state, rotxn, transaction)
        }
        TxData::Regular => {
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
}
