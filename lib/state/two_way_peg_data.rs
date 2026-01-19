//! Connect and disconnect two-way peg data

use std::collections::{BTreeMap, HashMap};

use fallible_iterator::FallibleIterator;
use sneed::{RoTxn, RwTxn, db::error::Error as DbError};

use crate::bitcoin_rpc::{BitcoinRpcClient, RpcConfig};
use crate::{
    state::{
        Error, State, WITHDRAWAL_BUNDLE_FAILURE_GAP, WithdrawalBundleInfo,
        rollback::RollBack,
    },
    types::{
        AccumulatorDiff, AggregatedWithdrawal, AmountOverflowError, BlockHash,
        GetValue, InPoint, M6id, OutPoint, OutPointKey, Output, OutputContent,
        ParentChainType, PointedOutput, PointedOutputRef, SpentOutput, Swap,
        SwapState, SwapTxId, WithdrawalBundle, WithdrawalBundleEvent,
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
            .iter()
            .map(|(_, output)| GetValue::get_value(&output.content))
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
        // Already applied
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
/// Query L1 blockchain for matching transactions and update swap
/// block_hash and block_height are the sidechain block where this validation occurs
fn query_and_update_swap(
    rpc_config: &RpcConfig,
    swap: &mut Swap,
    l1_recipient: &str,
    l1_amount: bitcoin::Amount,
    block_hash: BlockHash,
    block_height: u32,
) -> Result<bool, crate::bitcoin_rpc::Error> {
    let client = BitcoinRpcClient::new(rpc_config.clone());
    let amount_sats = l1_amount.to_sat();

    // Find transactions matching address and amount
    let matches = client
        .find_transactions_by_address_and_amount(l1_recipient, amount_sats)?;

    if matches.is_empty() {
        return Ok(false);
    }

    // Use the first match (most recent transaction)
    // In a production system, you might want to handle multiple matches differently
    let (txid, confirmations, sender_address) = &matches[0];

    // Convert txid string to SwapTxId
    let txid_bytes = hex::decode(txid)
        .map_err(|_| crate::bitcoin_rpc::Error::InvalidResponse)?;
    let l1_txid = SwapTxId::from_bytes(&txid_bytes);

    // Check if this is an update or new detection
    let zero_hash32 = [0u8; 32];
    let is_new = matches!(swap.l1_txid, SwapTxId::Hash32(h) if h == zero_hash32)
        || matches!(swap.l1_txid, SwapTxId::Hash(ref v) if v.is_empty() || v.iter().all(|&b| b == 0));

    if is_new {
        // New L1 transaction detected
        tracing::info!(
            swap_id = %swap.id,
            l1_txid = %txid,
            confirmations = %confirmations,
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
        if *confirmations >= swap.required_confirmations {
            swap.state = SwapState::ReadyToClaim;
        } else {
            swap.state = SwapState::WaitingConfirmations(
                *confirmations,
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

        if *confirmations > current_confirmations {
            tracing::debug!(
                swap_id = %swap.id,
                old_confirmations = %current_confirmations,
                new_confirmations = %confirmations,
                "Updating swap confirmations"
            );

            if *confirmations >= swap.required_confirmations {
                swap.state = SwapState::ReadyToClaim;
            } else {
                swap.state = SwapState::WaitingConfirmations(
                    *confirmations,
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

    for mut swap in swaps {
        // Only process L2 → L1 swaps that are pending or waiting for confirmations
        if !matches!(
            swap.state,
            SwapState::Pending | SwapState::WaitingConfirmations(..)
        ) {
            continue;
        }

        pending_swaps_count += 1;
        let l1_amount_str = swap
            .l1_amount
            .map(|amt| amt.to_string_in(bitcoin::Denomination::Bitcoin))
            .unwrap_or_else(|| "N/A".to_string());
        let l1_amount_sats =
            swap.l1_amount.map(|amt| amt.to_sat()).unwrap_or(0);
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
        if let Some(expires_at) = swap.expires_at_height {
            if block_height >= expires_at {
                tracing::info!(
                    swap_id = %swap.id,
                    block_height = %block_height,
                    expires_at = %expires_at,
                    "Swap expired, marking as cancelled"
                );
                swap.state = SwapState::Cancelled;
                state.save_swap(rwtxn, &swap)?;
                expired_swaps_count += 1;
                continue;
            }
        }

        // For L2 → L1 swaps, we need to check if the L1 transaction exists
        // on the SWAP TARGET CHAIN (swap.parent_chain), NOT the sidechain's mainchain.
        //
        // Example:
        // - Sidechain mainchain: Regtest (for deposits/withdrawals)
        // - Swap target: Signet (for coinshift transactions)
        // - We query Signet for transactions, not Regtest!
        //
        let l1_recipient_str =
            swap.l1_recipient_address.as_deref().unwrap_or("N/A");
        let l1_amount_str = swap
            .l1_amount
            .map(|amt| amt.to_string_in(bitcoin::Denomination::Bitcoin))
            .unwrap_or_else(|| "N/A".to_string());
        let l1_amount_sats =
            swap.l1_amount.map(|amt| amt.to_sat()).unwrap_or(0);
        tracing::debug!(
            swap_id = %swap.id,
            parent_chain = ?swap.parent_chain,
            l1_recipient_address = ?swap.l1_recipient_address,
            l1_amount_btc = %l1_amount_str,
            l1_amount_sats = %l1_amount_sats,
            "Scanning enforcer on {:?} for transactions to {} with amount {} BTC",
            swap.parent_chain,
            l1_recipient_str,
            l1_amount_str
        );

        // Query L1 blockchain for matching transactions if RPC config is available
        // Clone values to avoid borrow checker issues
        let l1_recipient_clone = swap.l1_recipient_address.clone();
        let l1_amount_clone = swap.l1_amount;
        let parent_chain_clone = swap.parent_chain;
        if let (Some(l1_recipient), Some(l1_amount)) =
            (l1_recipient_clone.as_deref(), l1_amount_clone)
        {
            if let Some(get_rpc_config) = rpc_config_getter {
                if let Some(rpc_config) = get_rpc_config(parent_chain_clone) {
                    match query_and_update_swap(
                        &rpc_config,
                        &mut swap,
                        l1_recipient,
                        l1_amount,
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
                            tracing::debug!(
                                swap_id = %swap.id,
                                error = %e,
                                "Failed to query L1 blockchain for swap (this is normal if RPC is not configured)"
                            );
                        }
                    }
                }
            }
        }

        scanned_swaps_count += 1;
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
    assert_eq!(bundle_status.value, WithdrawalBundleStatus::Submitted);
    assert_eq!(bundle_status.height, block_height);
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
