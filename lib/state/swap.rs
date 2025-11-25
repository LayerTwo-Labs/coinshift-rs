//! Swap validation and processing

use sneed::{RoTxn, RwTxn, db::error::Error as DbError};

use crate::{
    state::{Error, State},
    types::{
        Address, FilledTransaction, OutPoint, OutPointKey, Swap, SwapId,
        SwapState, SwapTxId, Transaction, TxData,
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
        parent_chain,
        l1_txid_bytes,
        required_confirmations,
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
                    return Err(Error::InvalidTransaction(format!(
                        "Input {} is locked to swap {}",
                        outpoint, locked_swap_id
                    )));
                }
            }
        }

        // Verify transaction spends at least l2_amount
        let total_input_value: bitcoin::Amount = filled_transaction
            .spent_utxos
            .iter()
            .map(|utxo| utxo.get_value())
            .sum::<Result<_, _>>()
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
    filled_transaction: &FilledTransaction,
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

    // 3. Verify at least one input is locked to this swap
    let mut found_locked_input = false;
    for (outpoint, _) in &transaction.inputs {
        if let Some(locked_swap_id) = state.is_output_locked_to_swap(rotxn, outpoint)? {
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
            "SwapClaim must spend at least one output locked to the swap".to_string(),
        ));
    }

    // 4. Verify output goes to correct recipient
    let TxData::SwapClaim { l2_claimer_address, .. } = &transaction.data else {
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
        if let Some(locked_swap_id) = state.is_output_locked_to_swap(rotxn, outpoint)? {
            return Err(Error::InvalidTransaction(format!(
                "Cannot spend locked output {} (locked to swap {})",
                outpoint, locked_swap_id
            )));
        }
    }

    Ok(())
}

