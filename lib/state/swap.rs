//! Swap validation and processing

use sneed::RoTxn;

use crate::{
    state::{Error, State},
    types::{
        FilledTransaction, SwapId, SwapState, SwapTxId, Transaction, TxData,
    },
};

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
    let computed_swap_id = if let (Some(l1_addr), Some(l1_amt)) =
        (l1_recipient_address.as_ref(), l1_amount)
    {
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
            l1_addr,
            bitcoin::Amount::from_sat(*l1_amt),
            &l2_sender_address,
            l2_recipient.as_ref(), // Now optional
        )
    } else {
        return Err(Error::InvalidTransaction(
            "L2 → L1 swap requires l1_recipient_address and l1_amount"
                .to_string(),
        ));
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
    if l1_recipient_address.is_some() {
        // Check that no inputs are locked to another swap
        for (outpoint, _) in &transaction.inputs {
            if let Some(locked_swap_id) =
                state.is_output_locked_to_swap(rotxn, outpoint)?
            {
                if locked_swap_id.0 != *swap_id {
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
                            let is_deserialization_error = err_str
                                .contains("Decoding")
                                || err_str.contains("InvalidTagEncoding")
                                || err_str.contains("deserialize")
                                || err_str.contains("bincode")
                                || err_str.contains("Borsh")
                                || err_debug.contains("Decoding")
                                || err_debug.contains("InvalidTagEncoding")
                                || err_debug.contains("deserialize");

                            if is_deserialization_error {
                                // Swap is corrupted - orphaned lock
                                return Err(Error::InvalidTransaction(
                                    format!(
                                        "Input {} is locked to corrupted swap {} (orphaned lock). Please run cleanup_orphaned_locks to fix this.",
                                        outpoint, locked_swap_id
                                    ),
                                ));
                            } else {
                                // Other database error - return original error
                                return Err(Error::InvalidTransaction(
                                    format!(
                                        "Input {} is locked to swap {}, but error checking swap: {}",
                                        outpoint, locked_swap_id, err
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Verify transaction spends at least l2_amount
        let total_input_value = filled_transaction
            .spent_utxos
            .iter()
            .map(|utxo| crate::types::GetValue::get_value(utxo))
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
    } else if let Some(claimer_addr) = l2_claimer_address {
        // Open swap: must go to claimer's L2 address
        *claimer_addr
    } else {
        return Err(Error::InvalidTransaction(
            "Open swap claim requires l2_claimer_address".to_string(),
        ));
    };

    let recipient_receives = transaction
        .outputs
        .iter()
        .any(|output| output.address == expected_recipient);

    if !recipient_receives {
        return Err(Error::InvalidTransaction(format!(
            "SwapClaim must have at least one output to {}",
            expected_recipient
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
