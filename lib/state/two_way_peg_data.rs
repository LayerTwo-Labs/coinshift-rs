//! Connect and disconnect two-way peg data

use std::collections::{BTreeMap, HashMap};

use fallible_iterator::FallibleIterator;
use sneed::{RoTxn, RwTxn, db::error::Error as DbError};

use crate::parent_chain_rpc::{ParentChainRpcClient, RpcConfig};
use crate::{
    state::{
        Error, State, WITHDRAWAL_BUNDLE_FAILURE_GAP, WithdrawalBundleInfo,
        rollback::RollBack,
    },
    types::{
        AccumulatorDiff, AggregatedWithdrawal, AmountOverflowError, BlockHash,
        GetValue, InPoint, M6id, OutPoint, OutPointKey, Output, OutputContent,
        ParentChainType, PointedOutput, PointedOutputRef, SpentOutput, Swap,
        SwapId, SwapState, SwapTxId, WithdrawalBundle, WithdrawalBundleEvent,
        WithdrawalBundleStatus, hash,
        proto::mainchain::{BlockEvent, TwoWayPegData},
    },
    wallet::Wallet,
};

fn collect_withdrawal_bundle(
    state: &State,
    rotxn: &RoTxn,
    block_height: u32,
) -> Result<Option<WithdrawalBundle>, Error> {
    // Weight of a bundle with 0 outputs.
    const BUNDLE_0_WEIGHT: u64 = 504;
    // Weight of a single output.
    const OUTPUT_WEIGHT: u64 = 128;
    // Turns out to be 3121.
    const MAX_BUNDLE_OUTPUTS: usize =
        ((bitcoin::policy::MAX_STANDARD_TX_WEIGHT as u64 - BUNDLE_0_WEIGHT)
            / OUTPUT_WEIGHT) as usize;

    // Aggregate all outputs by destination.
    // destination -> (value, mainchain fee, spent_utxos)
    let mut address_to_aggregated_withdrawal = HashMap::<
        bitcoin::Address<bitcoin::address::NetworkUnchecked>,
        AggregatedWithdrawal,
    >::new();
    let () = state
        .utxos
        .iter(rotxn)
        .map_err(DbError::from)?
        .map_err(|err| DbError::from(err).into())
        .for_each(|(outpoint_key, output)| {
            let outpoint: OutPoint = outpoint_key.into();
            if let OutputContent::Withdrawal {
                value,
                ref main_address,
                main_fee,
            } = output.content
            {
                let aggregated = address_to_aggregated_withdrawal
                    .entry(main_address.clone())
                    .or_insert(AggregatedWithdrawal {
                        spend_utxos: HashMap::new(),
                        main_address: main_address.clone(),
                        value: bitcoin::Amount::ZERO,
                        main_fee: bitcoin::Amount::ZERO,
                    });
                // Add up all values.
                aggregated.value = aggregated
                    .value
                    .checked_add(value)
                    .ok_or(AmountOverflowError)?;
                aggregated.main_fee = aggregated
                    .main_fee
                    .checked_add(main_fee)
                    .ok_or(AmountOverflowError)?;
                aggregated.spend_utxos.insert(outpoint, output);
            }
            Ok::<_, Error>(())
        })?;
    if address_to_aggregated_withdrawal.is_empty() {
        return Ok(None);
    }
    let mut aggregated_withdrawals: Vec<_> =
        address_to_aggregated_withdrawal.into_values().collect();
    aggregated_withdrawals.sort_by_key(|a| std::cmp::Reverse(a.clone()));
    let mut fee = bitcoin::Amount::ZERO;
    let mut spend_utxos = BTreeMap::<OutPoint, Output>::new();
    let mut bundle_outputs = Vec::with_capacity(MAX_BUNDLE_OUTPUTS);
    for aggregated in &aggregated_withdrawals {
        if bundle_outputs.len() > MAX_BUNDLE_OUTPUTS {
            break;
        }
        let bundle_output = bitcoin::TxOut {
            value: aggregated.value,
            script_pubkey: aggregated
                .main_address
                .assume_checked_ref()
                .script_pubkey(),
        };
        spend_utxos.extend(aggregated.spend_utxos.clone());
        bundle_outputs.push(bundle_output);
        fee += aggregated.main_fee;
    }
    let bundle =
        WithdrawalBundle::new(block_height, fee, spend_utxos, bundle_outputs)?;
    Ok(Some(bundle))
}

fn connect_withdrawal_bundle_submitted(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    event_block_hash: &bitcoin::BlockHash,
    m6id: M6id,
) -> Result<(), Error> {
    if let Some((bundle, bundle_block_height)) = state
        .pending_withdrawal_bundle
        .try_get(rwtxn, &())
        .map_err(DbError::from)?
        && bundle.compute_m6id() == m6id
    {
        assert_eq!(bundle_block_height, block_height - 1);

        // Calculate total withdrawal amount from bundle outputs
        let total_withdrawal_value: bitcoin::Amount = bundle
            .tx()
            .output
            .iter()
            .skip(2) // Skip mainchain_fee_txout and inputs_commitment_txout
            .map(|txout| txout.value)
            .sum();

        // Calculate total value being withdrawn from spend_utxos
        let total_spent_value: bitcoin::Amount = bundle
            .spend_utxos()
            .values()
            .map(|output| GetValue::get_value(&output.content))
            .sum();

        let output_count = bundle.tx().output.len().saturating_sub(2);

        tracing::info!(
            %block_height,
            %event_block_hash,
            %m6id,
            total_withdrawal_btc = %total_withdrawal_value.to_string_in(bitcoin::Denomination::Bitcoin),
            total_withdrawal_sats = %total_withdrawal_value.to_sat(),
            total_spent_btc = %total_spent_value.to_string_in(bitcoin::Denomination::Bitcoin),
            total_spent_sats = %total_spent_value.to_sat(),
            output_count = output_count,
            "Withdrawal bundle submitted to parent chain"
        );

        tracing::debug!(
            %block_height,
            %m6id,
            "Withdrawal bundle successfully submitted"
        );
        for (outpoint, spend_output) in bundle.spend_utxos() {
            let utxo_hash = hash(&PointedOutputRef {
                outpoint: *outpoint,
                output: spend_output,
            });
            accumulator_diff.remove(utxo_hash.into());
            let key = OutPointKey::from(outpoint);
            state.utxos.delete(rwtxn, &key).map_err(DbError::from)?;
            let spent_output = SpentOutput {
                output: spend_output.clone(),
                inpoint: InPoint::Withdrawal { m6id },
            };
            state
                .stxos
                .put(rwtxn, &key, &spent_output)
                .map_err(DbError::from)?;
        }
        state
            .withdrawal_bundles
            .put(
                rwtxn,
                &m6id,
                &(
                    WithdrawalBundleInfo::Known(bundle),
                    RollBack::new(
                        WithdrawalBundleStatus::Submitted,
                        block_height,
                    ),
                ),
            )
            .map_err(DbError::from)?;
        state
            .pending_withdrawal_bundle
            .delete(rwtxn, &())
            .map_err(DbError::from)?;
    } else if let Some((_bundle, bundle_status)) = state
        .withdrawal_bundles
        .try_get(rwtxn, &m6id)
        .map_err(DbError::from)?
    {
        // Already applied: the m6id is already recorded, so this submission is
        // a no-op. `disconnect_withdrawal_bundle_submitted` mirrors this by
        // leaving the stored bundle untouched.
        assert_eq!(
            bundle_status.earliest().value,
            WithdrawalBundleStatus::Submitted
        );
    } else {
        tracing::warn!(
            %event_block_hash,
            %m6id,
            "Unknown withdrawal bundle submitted"
        );
        state
            .withdrawal_bundles
            .put(
                rwtxn,
                &m6id,
                &(
                    WithdrawalBundleInfo::Unknown,
                    RollBack::new(
                        WithdrawalBundleStatus::Submitted,
                        block_height,
                    ),
                ),
            )
            .map_err(DbError::from)?;
    };
    Ok(())
}

fn connect_withdrawal_bundle_confirmed(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    event_block_hash: &bitcoin::BlockHash,
    m6id: M6id,
) -> Result<(), Error> {
    let (mut bundle, mut bundle_status) = state
        .withdrawal_bundles
        .try_get(rwtxn, &m6id)
        .map_err(DbError::from)?
        .ok_or(Error::UnknownWithdrawalBundle { m6id })?;
    if bundle_status.latest().value == WithdrawalBundleStatus::Confirmed {
        // Already applied
        return Ok(());
    }
    assert_eq!(
        bundle_status.latest().value,
        WithdrawalBundleStatus::Submitted
    );

    // Log withdrawal bundle confirmation
    match &bundle {
        WithdrawalBundleInfo::Known(bundle) => {
            let total_withdrawal_value: bitcoin::Amount = bundle
                .tx()
                .output
                .iter()
                .skip(2) // Skip mainchain_fee_txout and inputs_commitment_txout
                .map(|txout| txout.value)
                .sum();

            let output_count = bundle.tx().output.len().saturating_sub(2);

            tracing::info!(
                %block_height,
                %event_block_hash,
                %m6id,
                total_withdrawal_btc = %total_withdrawal_value.to_string_in(bitcoin::Denomination::Bitcoin),
                total_withdrawal_sats = %total_withdrawal_value.to_sat(),
                output_count = output_count,
                "Withdrawal bundle confirmed on parent chain"
            );
        }
        WithdrawalBundleInfo::Unknown
        | WithdrawalBundleInfo::UnknownConfirmed { .. } => {
            tracing::info!(
                %block_height,
                %event_block_hash,
                %m6id,
                "Unknown withdrawal bundle confirmed on parent chain"
            );
        }
    }
    // If an unknown bundle is confirmed, all UTXOs older than the
    // bundle submission are potentially spent.
    // This is only accepted in the case that block height is 0,
    // and so no UTXOs could possibly have been double-spent yet.
    // In this case, ALL UTXOs are considered spent.
    if !bundle.is_known() {
        if block_height == 0 {
            tracing::warn!(
                %event_block_hash,
                %m6id,
                "Unknown withdrawal bundle confirmed, marking all UTXOs as spent"
            );
            let utxos: BTreeMap<OutPoint, Output> = state
                .utxos
                .iter(rwtxn)
                .map_err(DbError::from)?
                .map(|(key, output)| Ok((key.into(), output)))
                .collect()
                .map_err(DbError::from)?;
            for (outpoint, output) in &utxos {
                let spent_output = SpentOutput {
                    output: output.clone(),
                    inpoint: InPoint::Withdrawal { m6id },
                };
                state
                    .stxos
                    .put(rwtxn, &OutPointKey::from(outpoint), &spent_output)
                    .map_err(DbError::from)?;
                let utxo_hash = hash(&PointedOutputRef {
                    outpoint: *outpoint,
                    output: &spent_output.output,
                });
                accumulator_diff.remove(utxo_hash.into());
            }
            state.utxos.clear(rwtxn).map_err(DbError::from)?;
            bundle =
                WithdrawalBundleInfo::UnknownConfirmed { spend_utxos: utxos };
        } else {
            return Err(Error::UnknownWithdrawalBundleConfirmed {
                event_block_hash: *event_block_hash,
                m6id,
            });
        }
    }
    bundle_status
        .push(WithdrawalBundleStatus::Confirmed, block_height)
        .expect("Push confirmed status should be valid");
    state
        .withdrawal_bundles
        .put(rwtxn, &m6id, &(bundle, bundle_status))
        .map_err(DbError::from)?;
    Ok(())
}

fn connect_withdrawal_bundle_failed(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    m6id: M6id,
) -> Result<(), Error> {
    let (bundle, mut bundle_status) = state
        .withdrawal_bundles
        .try_get(rwtxn, &m6id)
        .map_err(DbError::from)?
        .ok_or_else(|| Error::UnknownWithdrawalBundle { m6id })?;
    if bundle_status.latest().value == WithdrawalBundleStatus::Failed {
        // Already applied
        return Ok(());
    }
    assert_eq!(
        bundle_status.latest().value,
        WithdrawalBundleStatus::Submitted
    );

    // Log withdrawal bundle failure
    match &bundle {
        WithdrawalBundleInfo::Known(bundle) => {
            let total_withdrawal_value: bitcoin::Amount = bundle
                .tx()
                .output
                .iter()
                .skip(2) // Skip mainchain_fee_txout and inputs_commitment_txout
                .map(|txout| txout.value)
                .sum();

            let output_count = bundle.tx().output.len().saturating_sub(2);

            tracing::warn!(
                %block_height,
                %m6id,
                total_withdrawal_btc = %total_withdrawal_value.to_string_in(bitcoin::Denomination::Bitcoin),
                total_withdrawal_sats = %total_withdrawal_value.to_sat(),
                output_count = output_count,
                "Withdrawal bundle failed on parent chain"
            );
        }
        WithdrawalBundleInfo::Unknown
        | WithdrawalBundleInfo::UnknownConfirmed { .. } => {
            tracing::warn!(
                %block_height,
                %m6id,
                "Unknown withdrawal bundle failed on parent chain"
            );
        }
    }

    tracing::debug!(
        %block_height,
        %m6id,
        "Handling failed withdrawal bundle");
    bundle_status
        .push(WithdrawalBundleStatus::Failed, block_height)
        .expect("Push failed status should be valid");
    match &bundle {
        WithdrawalBundleInfo::Unknown
        | WithdrawalBundleInfo::UnknownConfirmed { .. } => (),
        WithdrawalBundleInfo::Known(bundle) => {
            for (outpoint, output) in bundle.spend_utxos() {
                let key = OutPointKey::from(outpoint);
                state.stxos.delete(rwtxn, &key).map_err(DbError::from)?;
                state
                    .utxos
                    .put(rwtxn, &key, output)
                    .map_err(DbError::from)?;
                let utxo_hash = hash(&PointedOutput {
                    outpoint: *outpoint,
                    output: output.clone(),
                });
                accumulator_diff.insert(utxo_hash.into());
            }
            let latest_failed_m6id = if let Some(mut latest_failed_m6id) = state
                .latest_failed_withdrawal_bundle
                .try_get(rwtxn, &())
                .map_err(DbError::from)?
            {
                latest_failed_m6id
                    .push(m6id, block_height)
                    .expect("Push latest failed m6id should be valid");
                latest_failed_m6id
            } else {
                RollBack::new(m6id, block_height)
            };
            state
                .latest_failed_withdrawal_bundle
                .put(rwtxn, &(), &latest_failed_m6id)
                .map_err(DbError::from)?;
        }
    }
    state
        .withdrawal_bundles
        .put(rwtxn, &m6id, &(bundle, bundle_status))
        .map_err(DbError::from)?;
    Ok(())
}

fn connect_withdrawal_bundle_event(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    event_block_hash: &bitcoin::BlockHash,
    event: &WithdrawalBundleEvent,
) -> Result<(), Error> {
    match event.status {
        WithdrawalBundleStatus::Submitted => {
            connect_withdrawal_bundle_submitted(
                state,
                rwtxn,
                block_height,
                accumulator_diff,
                event_block_hash,
                event.m6id,
            )
        }
        WithdrawalBundleStatus::Confirmed => {
            connect_withdrawal_bundle_confirmed(
                state,
                rwtxn,
                block_height,
                accumulator_diff,
                event_block_hash,
                event.m6id,
            )
        }
        WithdrawalBundleStatus::Failed => connect_withdrawal_bundle_failed(
            state,
            rwtxn,
            block_height,
            accumulator_diff,
            event.m6id,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn connect_event(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    latest_deposit_block_hash: &mut Option<bitcoin::BlockHash>,
    latest_withdrawal_bundle_event_block_hash: &mut Option<bitcoin::BlockHash>,
    event_block_hash: bitcoin::BlockHash,
    event: &BlockEvent,
    _wallet: Option<&Wallet>,
) -> Result<(), Error> {
    match event {
        BlockEvent::Deposit(deposit) => {
            let outpoint = OutPoint::Deposit(deposit.outpoint);
            let output = &deposit.output;

            // Extract value and address for logging
            let value = output.content.get_value();
            let address = output.address;

            tracing::info!(
                %block_height,
                %event_block_hash,
                outpoint = %outpoint,
                address = %address,
                amount_btc = %value.to_string_in(bitcoin::Denomination::Bitcoin),
                amount_sats = %value.to_sat(),
                "Deposit from parent chain received"
            );

            state
                .utxos
                .put(rwtxn, &OutPointKey::from(&outpoint), output)
                .map_err(DbError::from)?;
            let utxo_hash = hash(&PointedOutputRef { outpoint, output });
            accumulator_diff.insert(utxo_hash.into());
            *latest_deposit_block_hash = Some(event_block_hash);
        }
        BlockEvent::WithdrawalBundle(withdrawal_bundle_event) => {
            let () = connect_withdrawal_bundle_event(
                state,
                rwtxn,
                block_height,
                accumulator_diff,
                &event_block_hash,
                withdrawal_bundle_event,
            )?;
            *latest_withdrawal_bundle_event_block_hash = Some(event_block_hash);
        }
    }
    Ok(())
}

/// Process coinshift transactions - update swap states based on L1 transactions
/// This should be called when connecting 2WPD to check for L1 transactions
/// that match pending swaps.
///
/// IMPORTANT: This queries the SWAP TARGET CHAIN (e.g., Signet), NOT the sidechain's
/// mainchain (e.g., Regtest). The swap target chain is specified in swap.parent_chain.
///
/// Flow:
/// 1. Get all pending swaps
/// 2. For each swap, query swap.parent_chain (e.g., Signet) for transactions
/// 3. Match transactions by: l1_recipient_address and l1_amount
/// 4. Update swap state based on found transactions and confirmations
///
/// **BMM / header chain / merkle proof:** None of these are used for swap L1
/// verification in this repo. L1 presence and confirmations are taken from the
/// configured parent chain RPC only (no BMM reports, no per–parent-chain header
/// chain for swaps, no merkle proof of L1 tx in block).
///
/// Query L1 blockchain for matching transactions and update swap.
///
/// `block_hash` and `block_height` are the sidechain block where this
/// validation occurs.
///
/// Enforces L1 transaction uniqueness: the same (parent_chain, l1_txid) must
/// not be associated with more than one swap. Uses `get_swap_by_l1_txid` before
/// accepting a new L1 tx.
#[allow(clippy::too_many_arguments)]
fn query_and_update_swap(
    state: &State,
    rwtxn: &mut RwTxn,
    rpc_config: &RpcConfig,
    swap: &mut Swap,
    block_hash: BlockHash,
    block_height: u32,
) -> Result<bool, Error> {
    let client = ParentChainRpcClient::new(rpc_config.clone());
    let amount_sats = swap.l1_amount.to_sat();

    // Find transactions matching address and amount
    let matches = client.find_transactions_by_address_and_amount(
        &swap.l1_recipient_address,
        amount_sats,
    )?;

    if matches.is_empty() {
        return Ok(false);
    }

    // Only accept transactions that are confirmed and included in a block,
    // and not older than the chain's max L1 tx age
    let max_age = swap.parent_chain.max_l1_tx_age_blocks();
    let matches: Vec<_> = matches
        .into_iter()
        .filter(|(_, tx_info)| {
            if tx_info.confirmations == 0 || tx_info.blockheight.is_none() {
                return false;
            }
            if tx_info.confirmations > max_age {
                tracing::info!(
                    swap_id = %swap.id,
                    l1_txid = %tx_info.txid,
                    confirmations = %tx_info.confirmations,
                    max_age = %max_age,
                    "Rejecting L1 tx: too old (confirmations exceed max_l1_tx_age_blocks)"
                );
                return false;
            }
            true
        })
        .collect();
    if matches.is_empty() {
        tracing::debug!(
            swap_id = %swap.id,
            "No confirmed or block-included L1 match; rejecting unconfirmed, mempool-only, or too-old tx"
        );
        return Ok(false);
    }

    // Use the first valid match (most recent transaction)
    // In a production system, you might want to handle multiple matches differently
    let (sender_address, tx_info) = &matches[0];

    // Convert txid string from parent chain RPC (RPC byte order) to SwapTxId (canonical storage)
    let l1_txid = SwapTxId::from_hex_rpc(&tx_info.txid)
        .map_err(|_| crate::parent_chain_rpc::Error::InvalidResponse)?;

    // Check if this is an update or new detection
    let zero_hash32 = [0u8; 32];
    let is_new = matches!(swap.l1_txid, SwapTxId::Hash32(h) if h == zero_hash32)
        || matches!(swap.l1_txid, SwapTxId::Hash(ref v) if v.is_empty() || v.iter().all(|&b| b == 0));

    if is_new {
        // L1 transaction uniqueness: do not accept an L1 tx already used by another swap
        if let Some(existing) =
            state.get_swap_by_l1_txid(rwtxn, &swap.parent_chain, &l1_txid)?
            && existing.id != swap.id
        {
            tracing::info!(
                swap_id = %swap.id,
                existing_swap_id = %existing.id,
                l1_txid = %tx_info.txid,
                "Rejecting L1 tx already associated with another swap"
            );
            return Ok(false);
        }

        // New L1 transaction detected
        tracing::info!(
            swap_id = %swap.id,
            l1_txid = %tx_info.txid,
            confirmations = %tx_info.confirmations,
            sender = %sender_address,
            is_open_swap = %swap.l2_recipient.is_none(),
            "Detected new L1 transaction for swap"
        );

        // Update swap with L1 transaction
        // For open swaps, we don't store the sender address here - the claimer will provide
        // their L2 address when claiming, and we'll verify they sent the L1 transaction
        swap.update_l1_txid(l1_txid);

        // Save the sidechain block reference where this validation occurred
        swap.set_l1_txid_validation_block(block_hash, block_height);

        // Update state based on confirmations
        if tx_info.confirmations >= swap.required_confirmations {
            swap.state = SwapState::ReadyToClaim;
        } else {
            swap.state = SwapState::WaitingConfirmations(
                tx_info.confirmations,
                swap.required_confirmations,
            );
        }

        Ok(true)
    } else {
        // Update confirmations for existing transaction
        let current_confirmations = match swap.state {
            SwapState::WaitingConfirmations(current, _) => current,
            _ => 0,
        };

        if tx_info.confirmations > current_confirmations {
            tracing::debug!(
                swap_id = %swap.id,
                old_confirmations = %current_confirmations,
                new_confirmations = %tx_info.confirmations,
                "Updating swap confirmations"
            );

            if tx_info.confirmations >= swap.required_confirmations {
                swap.state = SwapState::ReadyToClaim;
            } else {
                swap.state = SwapState::WaitingConfirmations(
                    tx_info.confirmations,
                    swap.required_confirmations,
                );
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }
}

fn process_coinshift_transactions(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    block_hash: BlockHash,
    rpc_config_getter: Option<&dyn Fn(ParentChainType) -> Option<RpcConfig>>,
) -> Result<(), Error> {
    tracing::debug!(%block_height, "Starting to scan enforcer for coinshift transactions");

    // Get all pending swaps
    let swaps = state.load_all_swaps(rwtxn)?;
    let total_swaps_count = swaps.len();
    tracing::debug!(
        %block_height,
        swap_count = total_swaps_count,
        "Loaded swaps from state, scanning enforcer for matching transactions"
    );

    let mut pending_swaps_count = 0;
    let mut expired_swaps_count = 0;
    let mut scanned_swaps_count = 0;

    // Reversal data for any swaps expired at this height, so a 2WPD disconnect
    // can restore each swap's pre-expiry state and re-lock its outputs.
    let mut expired_swap_reversals: Vec<(SwapId, SwapState, Vec<OutPoint>)> =
        Vec::new();

    for mut swap in swaps {
        // Expiry has to be decided from block data alone, because it releases
        // the swap's escrow locks and `locked_swap_outputs` is consensus: it
        // gates `validate_swap_claim_consensus` (a claim must spend an output
        // locked to its swap) and `validate_no_locked_outputs` (an ordinary
        // transaction must not).
        //
        // `Completed` and `Cancelled` are the only two swap states `connect`
        // writes deterministically — the first when a `SwapClaim` connects, the
        // second here. Every other state (`Pending`, `WaitingConfirmations`,
        // `ReadyToClaim`) records what *this node* saw on a parent chain, so
        // branching expiry on one makes two honest nodes release different
        // locks and reject each other's blocks. Skip the deterministic pair and
        // nothing else.
        if matches!(swap.state, SwapState::Completed | SwapState::Cancelled) {
            continue;
        }

        pending_swaps_count += 1;
        let l1_amount_str =
            swap.l1_amount.to_string_in(bitcoin::Denomination::Bitcoin);
        let l1_amount_sats = swap.l1_amount.to_sat();
        tracing::debug!(
            swap_id = %swap.id,
            parent_chain = ?swap.parent_chain,
            l1_recipient_address = ?swap.l1_recipient_address,
            l1_amount_btc = %l1_amount_str,
            l1_amount_sats = %l1_amount_sats,
            swap_state = ?swap.state,
            "Checking swap for matching L1 transactions"
        );

        // Check if swap has expired
        if let Some(expires_at) = swap.expires_at_height
            && block_height >= expires_at
        {
            // Unlock all outputs locked to this swap so the creator can spend them again
            let previous_state = swap.state.clone();
            let mut unlocked_outpoints: Vec<OutPoint> = Vec::new();
            let locked_outputs: Vec<(OutPointKey, SwapId)> = state
                .locked_swap_outputs
                .iter(rwtxn)
                .map_err(DbError::from)?
                .map(|(key, sid)| Ok((key, sid)))
                .collect()?;
            for (outpoint_key, locked_swap_id) in locked_outputs {
                if locked_swap_id == swap.id {
                    let outpoint: OutPoint = outpoint_key.into();
                    state.unlock_output_from_swap(rwtxn, &outpoint)?;
                    unlocked_outpoints.push(outpoint);
                }
            }

            tracing::info!(
                swap_id = %swap.id,
                block_height = %block_height,
                expires_at = %expires_at,
                unlocked_outputs = %unlocked_outpoints.len(),
                "Swap expired, unlocking outputs and marking as cancelled"
            );
            swap.state = SwapState::Cancelled;
            state.save_swap(rwtxn, &swap)?;
            expired_swap_reversals.push((
                swap.id,
                previous_state,
                unlocked_outpoints,
            ));
            expired_swaps_count += 1;
            continue;
        }

        // Everything below is node-local L1 observation rather than consensus,
        // and only a swap still waiting on its payment has any use for it. A
        // `ReadyToClaim` swap reaches this loop so that expiry can see it; it
        // has nothing left to detect.
        if !matches!(
            swap.state,
            SwapState::Pending | SwapState::WaitingConfirmations(..)
        ) {
            continue;
        }

        // For L2 → L1 swaps, we need to check if the L1 transaction exists
        // on the SWAP TARGET CHAIN (swap.parent_chain), NOT the sidechain's mainchain.
        //
        // Example:
        // - Sidechain mainchain: Regtest (for deposits/withdrawals)
        // - Swap target: Signet (for coinshift transactions)
        // - We query Signet for transactions, not Regtest!
        //
        let l1_amount_str =
            swap.l1_amount.to_string_in(bitcoin::Denomination::Bitcoin);
        let l1_amount_sats = swap.l1_amount.to_sat();
        tracing::debug!(
            swap_id = %swap.id,
            parent_chain = ?swap.parent_chain,
            l1_recipient_address = ?swap.l1_recipient_address,
            l1_amount_btc = %l1_amount_str,
            l1_amount_sats = %l1_amount_sats,
            "Scanning enforcer on {:?} for transactions to {} with amount {} BTC",
            swap.parent_chain,
            swap.l1_recipient_address,
            l1_amount_str
        );

        // Swap L1 presence and confirmation count rely on the configured RPC for
        // the swap target chain (swap.parent_chain). If no RPC is configured here,
        // we skip L1 lookup and the swap stays Pending until RPC is set or the user
        // manually updates via update_swap_l1_txid.
        // Query L1 blockchain for matching transactions if RPC config is available.
        // URL is chosen by swap.parent_chain: we look up that chain in l1_rpc_configs.json.
        // If no RPC config exists for this chain, we skip L1 lookup and the swap stays
        // Pending until config is set or the user updates via update_swap_l1_txid.
        let parent_chain_clone = swap.parent_chain;
        if let Some(get_rpc_config) = rpc_config_getter
            && let Some(rpc_config) = get_rpc_config(parent_chain_clone)
        {
            tracing::info!(
                swap_id = %swap.id,
                parent_chain = ?parent_chain_clone,
                l1_recipient = %swap.l1_recipient_address,
                l1_amount_sats = %swap.l1_amount.to_sat(),
                url = %rpc_config.url,
                "Querying L1 for swap"
            );
            match query_and_update_swap(
                state,
                rwtxn,
                &rpc_config,
                &mut swap,
                block_hash,
                block_height,
            ) {
                Ok(updated) => {
                    if updated {
                        tracing::info!(
                            swap_id = %swap.id,
                            l1_txid = ?swap.l1_txid,
                            state = ?swap.state,
                            "Updated swap with L1 transaction"
                        );
                        state.save_swap(rwtxn, &swap)?;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        swap_id = %swap.id,
                        parent_chain = ?parent_chain_clone,
                        l1_recipient = %swap.l1_recipient_address,
                        url = %rpc_config.url,
                        error = %e,
                        "Failed to query L1 for swap; swap will stay pending until RPC succeeds or l1_txid is set manually"
                    );
                }
            }
        } else {
            tracing::debug!(
                swap_id = %swap.id,
                parent_chain = ?parent_chain_clone,
                "Skipping L1 lookup: no RPC config for this parent chain (swap stays Pending until config is set or l1_txid is set manually)"
            );
        }

        scanned_swaps_count += 1;
    }

    // Record the expiry reversal data so a 2WPD disconnect of this block can
    // restore the pre-expiry swap/lock state (see `disconnect`).
    if !expired_swap_reversals.is_empty() {
        state
            .expired_swaps
            .put(rwtxn, &block_height, &expired_swap_reversals)
            .map_err(DbError::from)?;
    }

    tracing::debug!(
        %block_height,
        total_swaps = total_swaps_count,
        pending_swaps = pending_swaps_count,
        expired_swaps = expired_swaps_count,
        scanned_swaps = scanned_swaps_count,
        "Finished scanning enforcer for coinshift transactions"
    );

    Ok(())
}

pub fn connect(
    state: &State,
    rwtxn: &mut RwTxn,
    two_way_peg_data: &TwoWayPegData,
    rpc_config_getter: Option<&dyn Fn(ParentChainType) -> Option<RpcConfig>>,
    wallet: Option<&Wallet>,
) -> Result<(), Error> {
    let block_height = state.try_get_height(rwtxn)?.ok_or(Error::NoTip)?;
    tracing::trace!(%block_height, "Connecting 2WPD...");
    let mut accumulator = state
        .utreexo_accumulator
        .try_get(rwtxn, &())
        .map_err(DbError::from)?
        .unwrap_or_default();
    let mut accumulator_diff = AccumulatorDiff::default();
    let mut latest_deposit_block_hash = None;
    let mut latest_withdrawal_bundle_event_block_hash = None;
    for (event_block_hash, event_block_info) in &two_way_peg_data.block_info {
        for event in &event_block_info.events {
            let () = connect_event(
                state,
                rwtxn,
                block_height,
                &mut accumulator_diff,
                &mut latest_deposit_block_hash,
                &mut latest_withdrawal_bundle_event_block_hash,
                *event_block_hash,
                event,
                wallet,
            )?;
        }
    }

    // Process coinshift transactions after processing deposits/withdrawals
    let block_hash = state.try_get_tip(rwtxn)?.ok_or(Error::NoTip)?;
    process_coinshift_transactions(
        state,
        rwtxn,
        block_height,
        block_hash,
        rpc_config_getter,
    )?;
    // Handle deposits.
    if let Some(latest_deposit_block_hash) = latest_deposit_block_hash {
        let deposit_block_seq_idx = state
            .deposit_blocks
            .last(rwtxn)
            .map_err(DbError::from)?
            .map_or(0, |(seq_idx, _)| seq_idx + 1);
        state
            .deposit_blocks
            .put(
                rwtxn,
                &deposit_block_seq_idx,
                &(latest_deposit_block_hash, block_height),
            )
            .map_err(DbError::from)?;
    }
    // Handle withdrawals
    if let Some(latest_withdrawal_bundle_event_block_hash) =
        latest_withdrawal_bundle_event_block_hash
    {
        let withdrawal_bundle_event_block_seq_idx = state
            .withdrawal_bundle_event_blocks
            .last(rwtxn)
            .map_err(DbError::from)?
            .map_or(0, |(seq_idx, _)| seq_idx + 1);
        state
            .withdrawal_bundle_event_blocks
            .put(
                rwtxn,
                &withdrawal_bundle_event_block_seq_idx,
                &(latest_withdrawal_bundle_event_block_hash, block_height),
            )
            .map_err(DbError::from)?;
    }
    let last_withdrawal_bundle_failure_height = state
        .get_latest_failed_withdrawal_bundle(rwtxn)
        .map_err(DbError::from)?
        .map(|(height, _bundle)| height)
        .unwrap_or_default();
    if block_height - last_withdrawal_bundle_failure_height
        >= WITHDRAWAL_BUNDLE_FAILURE_GAP
        && state
            .pending_withdrawal_bundle
            .try_get(rwtxn, &())
            .map_err(DbError::from)?
            .is_none()
        && let Some(bundle) =
            collect_withdrawal_bundle(state, rwtxn, block_height)?
    {
        let m6id = bundle.compute_m6id();
        state
            .pending_withdrawal_bundle
            .put(rwtxn, &(), &(bundle, block_height))
            .map_err(DbError::from)?;
        tracing::trace!(
            %block_height,
            %m6id,
            "Stored pending withdrawal bundle"
        );
    }
    let () = accumulator.apply_diff(accumulator_diff)?;
    state
        .utreexo_accumulator
        .put(rwtxn, &(), &accumulator)
        .map_err(DbError::from)?;
    Ok(())
}

fn disconnect_withdrawal_bundle_submitted(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    m6id: M6id,
) -> Result<(), Error> {
    let Some((bundle, bundle_status)) = state
        .withdrawal_bundles
        .try_get(rwtxn, &m6id)
        .map_err(DbError::from)?
    else {
        if let Some((bundle, _)) = state
            .pending_withdrawal_bundle
            .try_get(rwtxn, &())
            .map_err(DbError::from)?
            && bundle.compute_m6id() == m6id
        {
            // Already applied
            return Ok(());
        } else {
            return Err(Error::UnknownWithdrawalBundle { m6id });
        }
    };
    let bundle_status = bundle_status.latest();
    if bundle_status.height > block_height {
        // The stored status was recorded by a block later than the one being
        // disconnected, so it must have been disconnected already.
        return Err(Error::UnexpectedWithdrawalBundleStatus {
            m6id,
            status: bundle_status.value,
            status_height: bundle_status.height,
            block_height,
        });
    }
    if bundle_status.value != WithdrawalBundleStatus::Submitted
        || bundle_status.height != block_height
    {
        // The bundle was recorded by an earlier block, so connecting this
        // event took the "Already applied" branch of
        // `connect_withdrawal_bundle_submitted` and did not modify any state.
        // This happens when the parent chain proposes the same m6id again
        // after it expired or was paid out. Disconnecting must not modify any
        // state either.
        tracing::debug!(
            %block_height,
            %m6id,
            status = ?bundle_status.value,
            status_height = %bundle_status.height,
            "Withdrawal bundle submission was applied by an earlier block, nothing to disconnect"
        );
        return Ok(());
    }
    match bundle {
        WithdrawalBundleInfo::Unknown
        | WithdrawalBundleInfo::UnknownConfirmed { .. } => (),
        WithdrawalBundleInfo::Known(bundle) => {
            for (outpoint, output) in bundle.spend_utxos().iter().rev() {
                if !state
                    .stxos
                    .delete(rwtxn, &OutPointKey::from(outpoint))
                    .map_err(DbError::from)?
                {
                    return Err(Error::NoStxo {
                        outpoint: *outpoint,
                    });
                };
                state
                    .utxos
                    .put(rwtxn, &OutPointKey::from(outpoint), output)
                    .map_err(DbError::from)?;
                let utxo_hash = hash(&PointedOutput {
                    outpoint: *outpoint,
                    output: output.clone(),
                });
                accumulator_diff.insert(utxo_hash.into());
            }
            state
                .pending_withdrawal_bundle
                .put(rwtxn, &(), &(bundle, bundle_status.height - 1))
                .map_err(DbError::from)?;
        }
    }
    state
        .withdrawal_bundles
        .delete(rwtxn, &m6id)
        .map_err(DbError::from)?;
    Ok(())
}

fn disconnect_withdrawal_bundle_confirmed(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    m6id: M6id,
) -> Result<(), Error> {
    let (mut bundle, bundle_status) = state
        .withdrawal_bundles
        .try_get(rwtxn, &m6id)
        .map_err(DbError::from)?
        .ok_or_else(|| Error::UnknownWithdrawalBundle { m6id })?;
    let (prev_bundle_status, latest_bundle_status) = bundle_status.pop();
    if latest_bundle_status.value == WithdrawalBundleStatus::Submitted {
        // Already applied
        return Ok(());
    }
    assert_eq!(
        latest_bundle_status.value,
        WithdrawalBundleStatus::Confirmed
    );
    assert_eq!(latest_bundle_status.height, block_height);
    let prev_bundle_status = prev_bundle_status
        .expect("Pop confirmed bundle status should be valid");
    assert_eq!(
        prev_bundle_status.latest().value,
        WithdrawalBundleStatus::Submitted
    );
    match bundle {
        WithdrawalBundleInfo::Known(_) | WithdrawalBundleInfo::Unknown => (),
        WithdrawalBundleInfo::UnknownConfirmed { spend_utxos } => {
            for (outpoint, output) in spend_utxos {
                state
                    .utxos
                    .put(rwtxn, &OutPointKey::from(&outpoint), &output)
                    .map_err(DbError::from)?;
                if !state
                    .stxos
                    .delete(rwtxn, &OutPointKey::from(&outpoint))
                    .map_err(DbError::from)?
                {
                    return Err(Error::NoStxo { outpoint });
                };
                let utxo_hash = hash(&PointedOutput { outpoint, output });
                accumulator_diff.insert(utxo_hash.into());
            }
            bundle = WithdrawalBundleInfo::Unknown;
        }
    }
    state
        .withdrawal_bundles
        .put(rwtxn, &m6id, &(bundle, prev_bundle_status))
        .map_err(DbError::from)?;
    Ok(())
}

fn disconnect_withdrawal_bundle_failed(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    m6id: M6id,
) -> Result<(), Error> {
    let (bundle, bundle_status) = state
        .withdrawal_bundles
        .try_get(rwtxn, &m6id)
        .map_err(DbError::from)?
        .ok_or_else(|| Error::UnknownWithdrawalBundle { m6id })?;
    let (prev_bundle_status, latest_bundle_status) = bundle_status.pop();
    if latest_bundle_status.value == WithdrawalBundleStatus::Submitted {
        // Already applied
        return Ok(());
    } else {
        assert_eq!(latest_bundle_status.value, WithdrawalBundleStatus::Failed);
    }
    assert_eq!(latest_bundle_status.height, block_height);
    let prev_bundle_status =
        prev_bundle_status.expect("Pop failed bundle status should be valid");
    assert_eq!(
        prev_bundle_status.latest().value,
        WithdrawalBundleStatus::Submitted
    );
    match &bundle {
        WithdrawalBundleInfo::Unknown
        | WithdrawalBundleInfo::UnknownConfirmed { .. } => (),
        WithdrawalBundleInfo::Known(bundle) => {
            for (outpoint, output) in bundle.spend_utxos().iter().rev() {
                let spent_output = SpentOutput {
                    output: output.clone(),
                    inpoint: InPoint::Withdrawal { m6id },
                };
                state
                    .stxos
                    .put(rwtxn, &OutPointKey::from(outpoint), &spent_output)
                    .map_err(DbError::from)?;
                if state
                    .utxos
                    .delete(rwtxn, &OutPointKey::from(outpoint))
                    .map_err(DbError::from)?
                {
                    return Err(Error::NoUtxo {
                        outpoint: *outpoint,
                    });
                };
                let utxo_hash = hash(&PointedOutput {
                    outpoint: *outpoint,
                    output: output.clone(),
                });
                accumulator_diff.remove(utxo_hash.into());
            }
            let (prev_latest_failed_m6id, latest_failed_m6id) = state
                .latest_failed_withdrawal_bundle
                .try_get(rwtxn, &())
                .map_err(DbError::from)?
                .expect("latest failed withdrawal bundle should exist")
                .pop();
            assert_eq!(latest_failed_m6id.value, m6id);
            assert_eq!(latest_failed_m6id.height, block_height);
            if let Some(prev_latest_failed_m6id) = prev_latest_failed_m6id {
                state
                    .latest_failed_withdrawal_bundle
                    .put(rwtxn, &(), &prev_latest_failed_m6id)
                    .map_err(DbError::from)?;
            } else {
                state
                    .latest_failed_withdrawal_bundle
                    .delete(rwtxn, &())
                    .map_err(DbError::from)?;
            }
        }
    }
    state
        .withdrawal_bundles
        .put(rwtxn, &m6id, &(bundle, prev_bundle_status))
        .map_err(DbError::from)?;
    Ok(())
}

fn disconnect_withdrawal_bundle_event(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    event: &WithdrawalBundleEvent,
) -> Result<(), Error> {
    match event.status {
        WithdrawalBundleStatus::Submitted => {
            disconnect_withdrawal_bundle_submitted(
                state,
                rwtxn,
                block_height,
                accumulator_diff,
                event.m6id,
            )
        }
        WithdrawalBundleStatus::Confirmed => {
            disconnect_withdrawal_bundle_confirmed(
                state,
                rwtxn,
                block_height,
                accumulator_diff,
                event.m6id,
            )
        }
        WithdrawalBundleStatus::Failed => disconnect_withdrawal_bundle_failed(
            state,
            rwtxn,
            block_height,
            accumulator_diff,
            event.m6id,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn disconnect_event(
    state: &State,
    rwtxn: &mut RwTxn,
    block_height: u32,
    accumulator_diff: &mut AccumulatorDiff,
    latest_deposit_block_hash: &mut Option<bitcoin::BlockHash>,
    latest_withdrawal_bundle_event_block_hash: &mut Option<bitcoin::BlockHash>,
    event_block_hash: bitcoin::BlockHash,
    event: &BlockEvent,
) -> Result<(), Error> {
    match event {
        BlockEvent::Deposit(deposit) => {
            let outpoint = OutPoint::Deposit(deposit.outpoint);
            let output = deposit.output.clone();
            if !state
                .utxos
                .delete(rwtxn, &OutPointKey::from(&outpoint))
                .map_err(DbError::from)?
            {
                return Err(Error::NoUtxo { outpoint });
            }
            let utxo_hash = hash(&PointedOutput { outpoint, output });
            accumulator_diff.remove(utxo_hash.into());
            *latest_deposit_block_hash = Some(event_block_hash);
        }
        BlockEvent::WithdrawalBundle(withdrawal_bundle_event) => {
            let () = disconnect_withdrawal_bundle_event(
                state,
                rwtxn,
                block_height,
                accumulator_diff,
                withdrawal_bundle_event,
            )?;
            *latest_withdrawal_bundle_event_block_hash = Some(event_block_hash);
        }
    }
    Ok(())
}

pub fn disconnect(
    state: &State,
    rwtxn: &mut RwTxn,
    two_way_peg_data: &TwoWayPegData,
) -> Result<(), Error> {
    let block_height = state
        .try_get_height(rwtxn)?
        .expect("Height should not be None");
    let mut accumulator = state
        .utreexo_accumulator
        .try_get(rwtxn, &())
        .map_err(DbError::from)?
        .unwrap_or_default();
    let mut accumulator_diff = AccumulatorDiff::default();
    let mut latest_deposit_block_hash = None;
    let mut latest_withdrawal_bundle_event_block_hash = None;
    // Reverse any swap expiries applied by `process_coinshift_transactions`
    // when this block was connected: restore each swap's pre-expiry state and
    // re-lock the outputs that were unlocked. Symmetric with the connect path,
    // which keys the reversal data by the same block height.
    if let Some(expired_swap_reversals) = state
        .expired_swaps
        .try_get(rwtxn, &block_height)
        .map_err(DbError::from)?
    {
        for (swap_id, previous_state, unlocked_outpoints) in
            expired_swap_reversals
        {
            if let Some(mut swap) = state.get_swap(rwtxn, &swap_id)? {
                swap.state = previous_state;
                state.save_swap(rwtxn, &swap)?;
            }
            for outpoint in unlocked_outpoints {
                state.lock_output_to_swap(rwtxn, &outpoint, &swap_id)?;
            }
        }
        state
            .expired_swaps
            .delete(rwtxn, &block_height)
            .map_err(DbError::from)?;
    }
    // Restore pending withdrawal bundle
    for (event_block_hash, event_block_info) in
        two_way_peg_data.block_info.iter().rev()
    {
        for event in event_block_info.events.iter().rev() {
            let () = disconnect_event(
                state,
                rwtxn,
                block_height,
                &mut accumulator_diff,
                &mut latest_deposit_block_hash,
                &mut latest_withdrawal_bundle_event_block_hash,
                *event_block_hash,
                event,
            )?;
        }
    }
    // Handle withdrawals
    if let Some(latest_withdrawal_bundle_event_block_hash) =
        latest_withdrawal_bundle_event_block_hash
    {
        let (
            last_withdrawal_bundle_event_block_seq_idx,
            (
                last_withdrawal_bundle_event_block_hash,
                last_withdrawal_bundle_event_block_height,
            ),
        ) = state
            .withdrawal_bundle_event_blocks
            .last(rwtxn)
            .map_err(DbError::from)?
            .ok_or(Error::NoWithdrawalBundleEventBlock)?;
        assert_eq!(
            latest_withdrawal_bundle_event_block_hash,
            last_withdrawal_bundle_event_block_hash
        );
        assert_eq!(block_height - 1, last_withdrawal_bundle_event_block_height);
        if !state
            .deposit_blocks
            .delete(rwtxn, &last_withdrawal_bundle_event_block_seq_idx)
            .map_err(DbError::from)?
        {
            return Err(Error::NoWithdrawalBundleEventBlock);
        };
    }
    let last_withdrawal_bundle_failure_height = state
        .get_latest_failed_withdrawal_bundle(rwtxn)
        .map_err(DbError::from)?
        .map(|(height, _bundle)| height)
        .unwrap_or_default();
    if block_height - last_withdrawal_bundle_failure_height
        > WITHDRAWAL_BUNDLE_FAILURE_GAP
        && let Some((_bundle, bundle_height)) = state
            .pending_withdrawal_bundle
            .try_get(rwtxn, &())
            .map_err(DbError::from)?
        && bundle_height == block_height - 1
    {
        state
            .pending_withdrawal_bundle
            .delete(rwtxn, &())
            .map_err(DbError::from)?;
    }
    // Handle deposits.
    if let Some(latest_deposit_block_hash) = latest_deposit_block_hash {
        let (
            last_deposit_block_seq_idx,
            (last_deposit_block_hash, last_deposit_block_height),
        ) = state
            .deposit_blocks
            .last(rwtxn)
            .map_err(DbError::from)?
            .ok_or(Error::NoDepositBlock)?;
        assert_eq!(latest_deposit_block_hash, last_deposit_block_hash);
        assert_eq!(block_height - 1, last_deposit_block_height);
        if !state
            .deposit_blocks
            .delete(rwtxn, &last_deposit_block_seq_idx)
            .map_err(DbError::from)?
        {
            return Err(Error::NoDepositBlock);
        };
    }
    let () = accumulator.apply_diff(accumulator_diff)?;
    state
        .utreexo_accumulator
        .put(rwtxn, &(), &accumulator)
        .map_err(DbError::from)?;
    Ok(())
}

#[cfg(test)]
mod expiry_reversal_tests {
    use sneed::Env;

    use super::*;
    use crate::types::{Address, SwapDirection, Txid};

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

    /// Expiry must not depend on what this node saw on the parent chain.
    ///
    /// Regression test. Expiry used to run only for `Pending` and
    /// `WaitingConfirmations` swaps, which skipped any swap the local L1
    /// observer had already advanced to `ReadyToClaim`. That state is written
    /// from a parent-chain RPC — by `query_and_update_swap`, or by the
    /// `update_swap_l1_txid` RPC — so it differs between nodes whenever one
    /// endpoint is down, is slower, or is simply polled a moment later.
    ///
    /// The consequence was a chain split, because expiry releases escrow locks
    /// and `locked_swap_outputs` is consensus state. A node that skipped expiry
    /// kept the output locked and accepted the claim that spent it; a node that
    /// expired the swap rejected that same block, and accepted the creator's
    /// refund spend that the first node rejected. Two honest nodes, one block,
    /// opposite verdicts, in both directions.
    ///
    /// So: same swap, same height, two nodes that disagree about L1 only. Every
    /// consensus-visible outcome must match.
    #[test]
    fn expiry_ignores_node_local_l1_observation() {
        let outpoint = OutPoint::Regular {
            txid: Txid([99u8; 32]),
            vout: 1,
        };
        let swap_id = SwapId([42u8; 32]);

        // One node, distinguished only by the swap state its own parent-chain
        // observation arrived at before the expiry block connected.
        let connect_expiry_block_with = |observed: SwapState| {
            let (dir, env, state) = test_state();
            let mut swap = Swap::new(
                swap_id,
                SwapDirection::L2ToL1,
                ParentChainType::Regtest,
                SwapTxId::Hash32([0u8; 32]),
                None,
                Some(Address([3u8; 20])),
                sat(50_000),
                "rbtc-recipient".to_string(),
                sat(40_000),
                0,
                Some(5), // expires_at_height
                Some(Address([5u8; 20])),
            );
            swap.state = observed;

            let mut rwtxn = env.write_txn().unwrap();
            state.height.put(&mut rwtxn, &(), &5u32).unwrap();
            state.save_swap(&mut rwtxn, &swap).unwrap();
            state
                .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
                .unwrap();

            // Both nodes connect the same block, at the expiry height, with no
            // parent-chain RPC configured on either.
            process_coinshift_transactions(
                &state,
                &mut rwtxn,
                5,
                BlockHash([0u8; 32]),
                None,
            )
            .unwrap();
            rwtxn.commit().unwrap();
            (dir, env, state)
        };

        // Node A's endpoint answered in time; node B's did not.
        let (_dir_a, env_a, state_a) =
            connect_expiry_block_with(SwapState::ReadyToClaim);
        let (_dir_b, env_b, state_b) =
            connect_expiry_block_with(SwapState::Pending);

        let rotxn_a = env_a.read_txn().unwrap();
        let rotxn_b = env_b.read_txn().unwrap();

        // The lock set is the consensus-visible part, and it has to agree.
        let locked_a = state_a
            .is_output_locked_to_swap(&rotxn_a, &outpoint)
            .unwrap();
        let locked_b = state_b
            .is_output_locked_to_swap(&rotxn_b, &outpoint)
            .unwrap();
        assert_eq!(
            locked_a, locked_b,
            "nodes disagreeing about L1 must still agree about which outputs \
             are locked; they are what block validation reads"
        );
        assert_eq!(
            locked_a, None,
            "the expiry height passed, so the escrow is released on both"
        );

        // And the swap itself lands in the same state on both.
        assert_eq!(
            state_a.get_swap(&rotxn_a, &swap_id).unwrap().unwrap().state,
            SwapState::Cancelled,
        );
        assert_eq!(
            state_b.get_swap(&rotxn_b, &swap_id).unwrap().unwrap().state,
            SwapState::Cancelled,
        );
    }

    /// Connecting the block that expires a swap unlocks its output and marks it
    /// `Cancelled`; a 2WPD disconnect of that block must reverse the expiry,
    /// restoring the swap to `Pending` and re-locking the output, so the
    /// connect/disconnect of an expiry block is invertible.
    #[test]
    fn disconnect_reverses_swap_expiry() {
        let (_dir, env, state) = test_state();
        let swap_id = SwapId([42u8; 32]);
        let outpoint = OutPoint::Regular {
            txid: Txid([99u8; 32]),
            vout: 1,
        };
        let swap = Swap::new(
            swap_id,
            SwapDirection::L2ToL1,
            ParentChainType::Regtest,
            SwapTxId::Hash32([0u8; 32]),
            None,
            Some(Address([3u8; 20])),
            sat(50_000),
            "rbtc-recipient".to_string(),
            sat(40_000),
            0,
            Some(5), // expires_at_height
            Some(Address([5u8; 20])),
        );

        let mut rwtxn = env.write_txn().unwrap();
        state.height.put(&mut rwtxn, &(), &5u32).unwrap();
        state.save_swap(&mut rwtxn, &swap).unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
            .unwrap();

        // Sanity: swap is Pending and its output is locked.
        assert_eq!(
            state.get_swap(&rwtxn, &swap_id).unwrap().unwrap().state,
            SwapState::Pending
        );
        assert_eq!(
            state.is_output_locked_to_swap(&rwtxn, &outpoint).unwrap(),
            Some(swap_id)
        );

        // Connect: process expiry at the expiry height.
        process_coinshift_transactions(
            &state,
            &mut rwtxn,
            5,
            BlockHash([0u8; 32]),
            None,
        )
        .unwrap();
        assert_eq!(
            state.get_swap(&rwtxn, &swap_id).unwrap().unwrap().state,
            SwapState::Cancelled,
            "expiry should cancel the swap"
        );
        assert_eq!(
            state.is_output_locked_to_swap(&rwtxn, &outpoint).unwrap(),
            None,
            "expiry should unlock the output"
        );

        // Disconnect: the expiry mutation must be reversed.
        disconnect(&state, &mut rwtxn, &TwoWayPegData::default()).unwrap();
        assert_eq!(
            state.get_swap(&rwtxn, &swap_id).unwrap().unwrap().state,
            SwapState::Pending,
            "disconnect must restore the pre-expiry swap state"
        );
        assert_eq!(
            state.is_output_locked_to_swap(&rwtxn, &outpoint).unwrap(),
            Some(swap_id),
            "disconnect must re-lock the output to the swap"
        );
    }
}

#[cfg(test)]
mod withdrawal_bundle_reversal_tests {
    use bitcoin::hashes::Hash as _;
    use sneed::Env;

    use super::*;
    use crate::types::{Address, Txid};

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

    fn bundle_event(
        m6id: M6id,
        status: WithdrawalBundleStatus,
    ) -> WithdrawalBundleEvent {
        WithdrawalBundleEvent { m6id, status }
    }

    /// The parent chain only refuses a bundle proposal while its m6id is still
    /// pending, so once an m6id has expired the very same m6id can be proposed
    /// again, producing the event sequence `Submitted(X)`, `Failed(X)`,
    /// `Submitted(X)`. Connecting the repeated `Submitted` leaves the stored
    /// bundle untouched ("Already applied"), so disconnecting it must leave the
    /// stored bundle untouched too, instead of assuming that the latest stored
    /// status is a `Submitted` recorded by the disconnected block.
    #[test]
    fn disconnect_repeated_bundle_submission_is_noop() {
        let (_dir, env, state) = test_state();
        let outpoint = OutPoint::Regular {
            txid: Txid([9u8; 32]),
            vout: 0,
        };
        let key = OutPointKey::from(&outpoint);
        let output = Output {
            address: Address([1u8; 20]),
            content: OutputContent::Value(sat(30_000)),
        };
        let bundle_output = bitcoin::TxOut {
            value: sat(29_000),
            script_pubkey: bitcoin::ScriptBuf::new(),
        };
        let bundle = WithdrawalBundle::new(
            9,
            sat(1_000),
            BTreeMap::from([(outpoint, output.clone())]),
            vec![bundle_output],
        )
        .unwrap();
        let m6id = bundle.compute_m6id();
        let event_block_hash = bitcoin::BlockHash::all_zeros();

        let mut rwtxn = env.write_txn().unwrap();
        state.utxos.put(&mut rwtxn, &key, &output).unwrap();
        state
            .pending_withdrawal_bundle
            .put(&mut rwtxn, &(), &(bundle, 9))
            .unwrap();
        let mut accumulator_diff = AccumulatorDiff::default();

        // Submitted at height 10, then expired at height 11, which restores the
        // spent UTXO.
        connect_withdrawal_bundle_event(
            &state,
            &mut rwtxn,
            10,
            &mut accumulator_diff,
            &event_block_hash,
            &bundle_event(m6id, WithdrawalBundleStatus::Submitted),
        )
        .unwrap();
        connect_withdrawal_bundle_event(
            &state,
            &mut rwtxn,
            11,
            &mut accumulator_diff,
            &event_block_hash,
            &bundle_event(m6id, WithdrawalBundleStatus::Failed),
        )
        .unwrap();
        // The same m6id is proposed again at height 12.
        connect_withdrawal_bundle_event(
            &state,
            &mut rwtxn,
            12,
            &mut accumulator_diff,
            &event_block_hash,
            &bundle_event(m6id, WithdrawalBundleStatus::Submitted),
        )
        .unwrap();
        let (_bundle, bundle_status) = state
            .withdrawal_bundles
            .try_get(&rwtxn, &m6id)
            .unwrap()
            .unwrap();
        assert_eq!(
            bundle_status.latest().value,
            WithdrawalBundleStatus::Failed,
            "connecting the repeated submission should not change the status"
        );

        // Disconnecting the block that connected the repeated submission must
        // reverse nothing, since connecting it applied nothing.
        disconnect_withdrawal_bundle_event(
            &state,
            &mut rwtxn,
            12,
            &mut accumulator_diff,
            &bundle_event(m6id, WithdrawalBundleStatus::Submitted),
        )
        .unwrap();
        let (_bundle, bundle_status) = state
            .withdrawal_bundles
            .try_get(&rwtxn, &m6id)
            .unwrap()
            .unwrap();
        assert_eq!(
            bundle_status.latest().value,
            WithdrawalBundleStatus::Failed,
            "disconnect must leave the failed bundle in place"
        );
        assert_eq!(bundle_status.latest().height, 11);
        assert_eq!(
            state.utxos.try_get(&rwtxn, &key).unwrap().as_ref(),
            Some(&output),
            "disconnect must not touch the UTXO restored by the expiry"
        );
        assert!(
            state.stxos.try_get(&rwtxn, &key).unwrap().is_none(),
            "disconnect must not re-spend the UTXO restored by the expiry"
        );
    }
}
