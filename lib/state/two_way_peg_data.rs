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

/// The bundle's latest recorded status is not the one this event expects.
fn unexpected_status(
    m6id: M6id,
    bundle_status: &RollBack<WithdrawalBundleStatus>,
    block_height: u32,
) -> Error {
    let latest = bundle_status.latest();
    Error::UnexpectedWithdrawalBundleStatus {
        m6id,
        status: latest.value,
        status_height: latest.height,
        block_height,
    }
}

/// A status could not be pushed because the event is older than the latest
/// recorded status.
fn out_of_order(
    m6id: M6id,
    bundle_status: &RollBack<WithdrawalBundleStatus>,
    block_height: u32,
) -> Error {
    Error::WithdrawalBundleEventOutOfOrder {
        m6id,
        block_height,
        latest_height: bundle_status.latest().height,
    }
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
        // The m6id commits to the bundle's contents, which is what ties the
        // event to the pending bundle. The bundle is usually submitted in the
        // parent-chain block right after it was collected, but a slow
        // enforcer, a delayed L1 block or a restart in between can push the
        // Submitted event out by one or more sidechain blocks. That is
        // ordinary timing, not corruption.
        if bundle_block_height + 1 != block_height {
            tracing::debug!(
                %block_height,
                %bundle_block_height,
                %m6id,
                "Withdrawal bundle submitted later than the block after it was collected"
            );
        }

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
        let earliest = bundle_status.earliest();
        if earliest.value != WithdrawalBundleStatus::Submitted {
            return Err(Error::UnexpectedWithdrawalBundleStatus {
                m6id,
                status: earliest.value,
                status_height: earliest.height,
                block_height,
            });
        }
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
    if bundle_status.latest().value != WithdrawalBundleStatus::Submitted {
        return Err(unexpected_status(m6id, &bundle_status, block_height));
    }

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
        .map_err(|_| out_of_order(m6id, &bundle_status, block_height))?;
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
    if bundle_status.latest().value != WithdrawalBundleStatus::Submitted {
        return Err(unexpected_status(m6id, &bundle_status, block_height));
    }

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
        .map_err(|_| out_of_order(m6id, &bundle_status, block_height))?;
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
                let latest_height = latest_failed_m6id.latest().height;
                latest_failed_m6id.push(m6id, block_height).map_err(|_| {
                    Error::WithdrawalBundleEventOutOfOrder {
                        m6id,
                        block_height,
                        latest_height,
                    }
                })?;
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

    // Check if this is an update or new detection
    let zero_hash32 = [0u8; 32];
    let is_new = matches!(swap.l1_txid, SwapTxId::Hash32(h) if h == zero_hash32)
        || matches!(swap.l1_txid, SwapTxId::Hash(ref v) if v.is_empty() || v.iter().all(|&b| b == 0));

    // Once the fill's txid is known, track it directly. Discovery only sees
    // outputs that are still unspent, so a fill the creator has already
    // spent would otherwise vanish from view before it reached the required
    // confirmations and the swap would never become claimable.
    let matches = if is_new {
        client.find_transactions_by_address_and_amount(
            &swap.l1_recipient_address,
            amount_sats,
        )?
    } else {
        let tx_info = client.get_transaction(&swap.l1_txid.to_hex_rpc())?;
        vec![(String::new(), tx_info)]
    };

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
        // Only process L2 → L1 swaps that are pending or waiting for confirmations
        if !matches!(
            swap.state,
            SwapState::Pending | SwapState::WaitingConfirmations(..)
        ) {
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
        .get_latest_failed_withdrawal_bundle(rwtxn)?
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
    if latest_bundle_status.value != WithdrawalBundleStatus::Confirmed
        || latest_bundle_status.height != block_height
    {
        return Err(Error::UnexpectedWithdrawalBundleStatus {
            m6id,
            status: latest_bundle_status.value,
            status_height: latest_bundle_status.height,
            block_height,
        });
    }
    // A Confirmed status is only ever pushed on top of a Submitted one, so
    // there must be a previous status and it must be Submitted.
    let prev_bundle_status = match prev_bundle_status {
        Some(prev)
            if prev.latest().value == WithdrawalBundleStatus::Submitted =>
        {
            prev
        }
        _ => {
            return Err(Error::UnexpectedWithdrawalBundleStatus {
                m6id,
                status: latest_bundle_status.value,
                status_height: latest_bundle_status.height,
                block_height,
            });
        }
    };
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
    }
    if latest_bundle_status.value != WithdrawalBundleStatus::Failed
        || latest_bundle_status.height != block_height
    {
        return Err(Error::UnexpectedWithdrawalBundleStatus {
            m6id,
            status: latest_bundle_status.value,
            status_height: latest_bundle_status.height,
            block_height,
        });
    }
    // A Failed status is only ever pushed on top of a Submitted one.
    let prev_bundle_status = match prev_bundle_status {
        Some(prev)
            if prev.latest().value == WithdrawalBundleStatus::Submitted =>
        {
            prev
        }
        _ => {
            return Err(Error::UnexpectedWithdrawalBundleStatus {
                m6id,
                status: latest_bundle_status.value,
                status_height: latest_bundle_status.height,
                block_height,
            });
        }
    };
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
                .ok_or(Error::InconsistentLatestFailedWithdrawalBundle {
                    m6id,
                })?
                .pop();
            if latest_failed_m6id.value != m6id
                || latest_failed_m6id.height != block_height
            {
                return Err(Error::LatestFailedWithdrawalBundleMismatch {
                    expected: m6id,
                    block_height,
                    found: latest_failed_m6id.value,
                    found_height: latest_failed_m6id.height,
                });
            }
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
    let block_height = state.try_get_height(rwtxn)?.ok_or(Error::NoTip)?;
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
        // `connect` records the height of the sidechain tip at the time the
        // event block was applied, and `disconnect` runs before `disconnect_tip`
        // lowers the height, so the two must match exactly.
        if latest_withdrawal_bundle_event_block_hash
            != last_withdrawal_bundle_event_block_hash
            || block_height != last_withdrawal_bundle_event_block_height
        {
            return Err(Error::EventBlockMismatch {
                table: "withdrawal_bundle_event_blocks",
                block_height,
                expected_hash: latest_withdrawal_bundle_event_block_hash,
                found_hash: last_withdrawal_bundle_event_block_hash,
                found_height: last_withdrawal_bundle_event_block_height,
            });
        }
        if !state
            .withdrawal_bundle_event_blocks
            .delete(rwtxn, &last_withdrawal_bundle_event_block_seq_idx)
            .map_err(DbError::from)?
        {
            return Err(Error::NoWithdrawalBundleEventBlock);
        };
    }
    let last_withdrawal_bundle_failure_height = state
        .get_latest_failed_withdrawal_bundle(rwtxn)?
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
        if latest_deposit_block_hash != last_deposit_block_hash
            || block_height != last_deposit_block_height
        {
            return Err(Error::EventBlockMismatch {
                table: "deposit_blocks",
                block_height,
                expected_hash: latest_deposit_block_hash,
                found_hash: last_deposit_block_hash,
                found_height: last_deposit_block_height,
            });
        }
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

    /// A bundle collected at height N is normally reported as Submitted by
    /// the 2WPD of block N+1, but a slow enforcer, a delayed L1 block or a
    /// restart in between can push the event to a later block. The m6id
    /// identifies the bundle; the height gap is timing, not corruption, and
    /// must not kill the net task.
    #[test]
    fn connect_accepts_delayed_bundle_submission() {
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
        let bundle = WithdrawalBundle::new(
            9,
            sat(1_000),
            BTreeMap::from([(outpoint, output.clone())]),
            vec![bitcoin::TxOut {
                value: sat(29_000),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        )
        .unwrap();
        let m6id = bundle.compute_m6id();

        let mut rwtxn = env.write_txn().unwrap();
        state.utxos.put(&mut rwtxn, &key, &output).unwrap();
        state
            .pending_withdrawal_bundle
            .put(&mut rwtxn, &(), &(bundle, 9))
            .unwrap();
        let mut accumulator_diff = AccumulatorDiff::default();

        // Collected at 9, submitted at 12 rather than 10.
        connect_withdrawal_bundle_event(
            &state,
            &mut rwtxn,
            12,
            &mut accumulator_diff,
            &bitcoin::BlockHash::all_zeros(),
            &bundle_event(m6id, WithdrawalBundleStatus::Submitted),
        )
        .expect("a late Submitted event must be applied, not panic");
        let (_bundle, bundle_status) = state
            .withdrawal_bundles
            .try_get(&rwtxn, &m6id)
            .unwrap()
            .unwrap();
        assert_eq!(
            bundle_status.latest().value,
            WithdrawalBundleStatus::Submitted
        );
        assert_eq!(bundle_status.latest().height, 12);
        assert!(
            state.utxos.try_get(&rwtxn, &key).unwrap().is_none(),
            "the bundle's input must be spent"
        );
        assert!(
            state
                .pending_withdrawal_bundle
                .try_get(&rwtxn, &())
                .unwrap()
                .is_none(),
            "the pending bundle must be consumed"
        );
    }

    /// A Confirmed event for a bundle whose latest status is not Submitted is
    /// a malformed event sequence. It must surface as an error the caller can
    /// handle, not a panic that takes the net task down.
    #[test]
    fn connect_confirmed_without_submitted_is_an_error() {
        let (_dir, env, state) = test_state();
        let m6id = M6id(bitcoin::Txid::all_zeros());
        let mut rwtxn = env.write_txn().unwrap();
        state
            .withdrawal_bundles
            .put(
                &mut rwtxn,
                &m6id,
                &(
                    WithdrawalBundleInfo::Unknown,
                    RollBack::new(WithdrawalBundleStatus::Failed, 5),
                ),
            )
            .unwrap();
        let mut accumulator_diff = AccumulatorDiff::default();
        let result = connect_withdrawal_bundle_event(
            &state,
            &mut rwtxn,
            6,
            &mut accumulator_diff,
            &bitcoin::BlockHash::all_zeros(),
            &bundle_event(m6id, WithdrawalBundleStatus::Confirmed),
        );
        assert!(
            matches!(
                result,
                Err(Error::UnexpectedWithdrawalBundleStatus {
                    status: WithdrawalBundleStatus::Failed,
                    status_height: 5,
                    block_height: 6,
                    ..
                })
            ),
            "expected UnexpectedWithdrawalBundleStatus, got {result:?}"
        );
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

#[cfg(test)]
mod event_block_bookkeeping_tests {
    use bitcoin::hashes::Hash as _;
    use sneed::Env;

    use super::*;
    use crate::types::{
        Accumulator, Address, Txid,
        proto::mainchain::{BlockInfo, Deposit},
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

    /// 2WPD carrying one deposit and one `Submitted` withdrawal-bundle event in
    /// the same parent-chain block.
    fn two_way_peg_data(
        event_block_hash: bitcoin::BlockHash,
        deposit: Deposit,
        m6id: M6id,
    ) -> TwoWayPegData {
        let block_info = BlockInfo {
            bmm_commitment: None,
            events: vec![
                BlockEvent::Deposit(deposit),
                BlockEvent::WithdrawalBundle(WithdrawalBundleEvent {
                    m6id,
                    status: WithdrawalBundleStatus::Submitted,
                }),
            ],
        };
        let mut two_way_peg_data = TwoWayPegData::default();
        two_way_peg_data
            .block_info
            .insert(event_block_hash, block_info);
        two_way_peg_data
    }

    /// Connecting 2WPD records the event block in `deposit_blocks` and in
    /// `withdrawal_bundle_event_blocks`; disconnecting the same 2WPD must
    /// remove exactly those two rows, each from its own table. The withdrawal
    /// branch used to delete from `deposit_blocks` by the withdrawal sequence
    /// index, leaving the withdrawal row behind and removing a deposit row it
    /// did not own, so reorg bookkeeping drifted after the first reorg over a
    /// bundle event.
    #[test]
    fn disconnect_removes_rows_from_their_own_tables() {
        let (_dir, env, state) = test_state();
        let event_block_hash = bitcoin::BlockHash::from_byte_array([7u8; 32]);
        let earlier_block_hash = bitcoin::BlockHash::from_byte_array([6u8; 32]);

        // A withdrawal waiting to be bundled, collected at height 9.
        let withdrawal_outpoint = OutPoint::Regular {
            txid: Txid([9u8; 32]),
            vout: 0,
        };
        let withdrawal_output = Output {
            address: Address([1u8; 20]),
            content: OutputContent::Value(sat(30_000)),
        };
        let bundle = WithdrawalBundle::new(
            9,
            sat(1_000),
            BTreeMap::from([(withdrawal_outpoint, withdrawal_output.clone())]),
            vec![bitcoin::TxOut {
                value: sat(29_000),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        )
        .unwrap();
        let m6id = bundle.compute_m6id();

        let deposit = Deposit {
            tx_index: 0,
            outpoint: bitcoin::OutPoint {
                txid: bitcoin::Txid::from_byte_array([8u8; 32]),
                vout: 0,
            },
            output: Output {
                address: Address([2u8; 20]),
                content: OutputContent::Value(sat(10_000)),
            },
        };
        let two_way_peg_data =
            two_way_peg_data(event_block_hash, deposit, m6id);

        let mut rwtxn = env.write_txn().unwrap();
        // Sidechain block 10 is the tip; its 2WPD is being connected.
        state.height.put(&mut rwtxn, &(), &10u32).unwrap();
        state
            .tip
            .put(&mut rwtxn, &(), &BlockHash([10u8; 32]))
            .unwrap();
        state
            .utxos
            .put(
                &mut rwtxn,
                &OutPointKey::from(&withdrawal_outpoint),
                &withdrawal_output,
            )
            .unwrap();
        state
            .pending_withdrawal_bundle
            .put(&mut rwtxn, &(), &(bundle, 9))
            .unwrap();
        // The bundle's input must be in the accumulator for connect to spend it.
        let mut accumulator = Accumulator::default();
        let mut seed_diff = AccumulatorDiff::default();
        seed_diff.insert(
            hash(&PointedOutput {
                outpoint: withdrawal_outpoint,
                output: withdrawal_output.clone(),
            })
            .into(),
        );
        accumulator.apply_diff(seed_diff).unwrap();
        state
            .utreexo_accumulator
            .put(&mut rwtxn, &(), &accumulator)
            .unwrap();
        // Rows recorded by an earlier block; they must survive the reorg.
        state
            .deposit_blocks
            .put(&mut rwtxn, &0, &(earlier_block_hash, 3))
            .unwrap();
        state
            .withdrawal_bundle_event_blocks
            .put(&mut rwtxn, &0, &(earlier_block_hash, 3))
            .unwrap();

        connect(&state, &mut rwtxn, &two_way_peg_data, None, None).unwrap();
        assert_eq!(
            state.deposit_blocks.last(&rwtxn).unwrap(),
            Some((1, (event_block_hash, 10))),
            "connect must record the deposit block"
        );
        assert_eq!(
            state.withdrawal_bundle_event_blocks.last(&rwtxn).unwrap(),
            Some((1, (event_block_hash, 10))),
            "connect must record the withdrawal bundle event block"
        );

        disconnect(&state, &mut rwtxn, &two_way_peg_data).unwrap();
        assert_eq!(
            state.deposit_blocks.last(&rwtxn).unwrap(),
            Some((0, (earlier_block_hash, 3))),
            "disconnect must remove only the disconnected deposit row"
        );
        assert_eq!(
            state.withdrawal_bundle_event_blocks.last(&rwtxn).unwrap(),
            Some((0, (earlier_block_hash, 3))),
            "disconnect must remove the disconnected withdrawal event row"
        );
    }
}
