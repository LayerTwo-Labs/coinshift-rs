use std::collections::{BTreeMap, HashMap, HashSet};

use fallible_iterator::FallibleIterator;
use futures::Stream;
use heed::types::SerdeBincode;
use rustreexo::accumulator::{node_hash::BitcoinNodeHash, proof::Proof};
use serde::{Deserialize, Serialize};
use sneed::{
    DatabaseUnique, RoTxn, RwTxn, UnitKey,
    db::error::{self as db_error, Error as DbError},
    env::Error as EnvError,
    rwtxn::Error as RwTxnError,
};

use crate::{
    authorization::Authorization,
    types::{
        Accumulator, Address, AmountOverflowError, AmountUnderflowError,
        Authorized, AuthorizedTransaction, BlockHash, Body, FilledTransaction,
        GetAddress, GetValue, Header, InPoint, M6id, MerkleRoot, OutPoint,
        OutPointKey, Output, ParentChainType, PointedOutput, SpentOutput,
        Swap, SwapId, SwapState, SwapTxId, Transaction, TxData, VERSION, Verify, Version,
        WithdrawalBundle, WithdrawalBundleStatus,
        proto::mainchain::TwoWayPegData,
    },
    util::Watchable,
};

mod block;
mod error;
mod rollback;
mod swap;
mod two_way_peg_data;

pub use error::Error;
use rollback::RollBack;

pub const WITHDRAWAL_BUNDLE_FAILURE_GAP: u32 = 4;

/// Prevalidated block data containing computed values from validation
/// to avoid redundant computation during connection
#[derive(Clone, Debug)]
pub struct PrevalidatedBlock {
    pub filled_transactions: Vec<FilledTransaction>,
    pub computed_merkle_root: MerkleRoot,
    pub total_fees: bitcoin::Amount,
    pub coinbase_value: bitcoin::Amount,
    /// Precomputed next height to avoid DB read in write txn
    pub next_height: u32,
    pub accumulator_diff: crate::types::AccumulatorDiff,
}

/// Information we have regarding a withdrawal bundle
#[derive(Debug, Deserialize, Serialize)]
enum WithdrawalBundleInfo {
    /// Withdrawal bundle is known
    Known(WithdrawalBundle),
    /// Withdrawal bundle is unknown but unconfirmed / failed
    Unknown,
    /// If an unknown withdrawal bundle is confirmed, ALL UTXOs are
    /// considered spent.
    UnknownConfirmed {
        spend_utxos: BTreeMap<OutPoint, Output>,
    },
}

impl WithdrawalBundleInfo {
    fn is_known(&self) -> bool {
        match self {
            Self::Known(_) => true,
            Self::Unknown | Self::UnknownConfirmed { .. } => false,
        }
    }
}

#[derive(Clone)]
pub struct State {
    /// Current tip
    tip: DatabaseUnique<UnitKey, SerdeBincode<BlockHash>>,
    /// Current height
    height: DatabaseUnique<UnitKey, SerdeBincode<u32>>,
    pub utxos: DatabaseUnique<OutPointKey, SerdeBincode<Output>>,
    pub stxos: DatabaseUnique<OutPointKey, SerdeBincode<SpentOutput>>,
    /// Pending withdrawal bundle and block height
    pub pending_withdrawal_bundle:
        DatabaseUnique<UnitKey, SerdeBincode<(WithdrawalBundle, u32)>>,
    /// Latest failed (known) withdrawal bundle
    latest_failed_withdrawal_bundle:
        DatabaseUnique<UnitKey, SerdeBincode<RollBack<M6id>>>,
    /// Withdrawal bundles and their status.
    /// Some withdrawal bundles may be unknown.
    /// in which case they are `None`.
    withdrawal_bundles: DatabaseUnique<
        SerdeBincode<M6id>,
        SerdeBincode<(WithdrawalBundleInfo, RollBack<WithdrawalBundleStatus>)>,
    >,
    /// deposit blocks and the height at which they were applied, keyed sequentially
    pub deposit_blocks: DatabaseUnique<
        SerdeBincode<u32>,
        SerdeBincode<(bitcoin::BlockHash, u32)>,
    >,
    /// withdrawal bundle event blocks and the height at which they were applied, keyed sequentially
    pub withdrawal_bundle_event_blocks: DatabaseUnique<
        SerdeBincode<u32>,
        SerdeBincode<(bitcoin::BlockHash, u32)>,
    >,
    pub utreexo_accumulator: DatabaseUnique<UnitKey, SerdeBincode<Accumulator>>,
    /// All swaps
    pub swaps: DatabaseUnique<SerdeBincode<SwapId>, SerdeBincode<Swap>>,
    /// Lookup swap by parent chain and L1 transaction ID
    pub swaps_by_l1_txid: DatabaseUnique<
        SerdeBincode<(ParentChainType, SwapTxId)>,
        SerdeBincode<SwapId>,
    >,
    /// Lookup all swaps for a recipient address
    pub swaps_by_recipient: DatabaseUnique<
        SerdeBincode<Address>,
        SerdeBincode<Vec<SwapId>>,
    >,
    /// Tracks which outputs are locked to which swap
    pub locked_swap_outputs: DatabaseUnique<
        OutPointKey,
        SerdeBincode<SwapId>,
    >,
    _version: DatabaseUnique<UnitKey, SerdeBincode<Version>>,
}

impl State {
    pub const NUM_DBS: u32 = 15;

    pub fn new(env: &sneed::Env) -> Result<Self, Error> {
        let mut rwtxn = env.write_txn().map_err(EnvError::from)?;
        let tip = DatabaseUnique::create(env, &mut rwtxn, "tip")
            .map_err(EnvError::from)?;
        let height = DatabaseUnique::create(env, &mut rwtxn, "height")
            .map_err(EnvError::from)?;
        let utxos = DatabaseUnique::create(env, &mut rwtxn, "utxos")
            .map_err(EnvError::from)?;
        let stxos = DatabaseUnique::create(env, &mut rwtxn, "stxos")
            .map_err(EnvError::from)?;
        let pending_withdrawal_bundle = DatabaseUnique::create(
            env,
            &mut rwtxn,
            "pending_withdrawal_bundle",
        )
        .map_err(EnvError::from)?;
        let latest_failed_withdrawal_bundle = DatabaseUnique::create(
            env,
            &mut rwtxn,
            "latest_failed_withdrawal_bundle",
        )
        .map_err(EnvError::from)?;
        let withdrawal_bundles =
            DatabaseUnique::create(env, &mut rwtxn, "withdrawal_bundles")
                .map_err(EnvError::from)?;
        let deposit_blocks =
            DatabaseUnique::create(env, &mut rwtxn, "deposit_blocks")
                .map_err(EnvError::from)?;
        let withdrawal_bundle_event_blocks = DatabaseUnique::create(
            env,
            &mut rwtxn,
            "withdrawal_bundle_event_blocks",
        )
        .map_err(EnvError::from)?;
        let utreexo_accumulator =
            DatabaseUnique::create(env, &mut rwtxn, "utreexo_accumulator")
                .map_err(EnvError::from)?;
        let swaps = DatabaseUnique::create(env, &mut rwtxn, "swaps")
            .map_err(EnvError::from)?;
        let swaps_by_l1_txid =
            DatabaseUnique::create(env, &mut rwtxn, "swaps_by_l1_txid")
                .map_err(EnvError::from)?;
        let swaps_by_recipient =
            DatabaseUnique::create(env, &mut rwtxn, "swaps_by_recipient")
                .map_err(EnvError::from)?;
        let locked_swap_outputs =
            DatabaseUnique::create(env, &mut rwtxn, "locked_swap_outputs")
                .map_err(EnvError::from)?;
        let version = DatabaseUnique::create(env, &mut rwtxn, "state_version")
            .map_err(EnvError::from)?;
        if version
            .try_get(&rwtxn, &())
            .map_err(DbError::from)?
            .is_none()
        {
            version
                .put(&mut rwtxn, &(), &*VERSION)
                .map_err(DbError::from)?;
        }
        rwtxn.commit().map_err(RwTxnError::from)?;
        Ok(Self {
            tip,
            height,
            utxos,
            stxos,
            pending_withdrawal_bundle,
            latest_failed_withdrawal_bundle,
            withdrawal_bundles,
            deposit_blocks,
            withdrawal_bundle_event_blocks,
            utreexo_accumulator,
            swaps,
            swaps_by_l1_txid,
            swaps_by_recipient,
            locked_swap_outputs,
            _version: version,
        })
    }

    pub fn try_get_tip(
        &self,
        rotxn: &RoTxn,
    ) -> Result<Option<BlockHash>, Error> {
        let tip = self.tip.try_get(rotxn, &())?;
        Ok(tip)
    }

    pub fn try_get_height(&self, rotxn: &RoTxn) -> Result<Option<u32>, Error> {
        let height = self.height.try_get(rotxn, &())?;
        Ok(height)
    }

    pub fn get_utxos(
        &self,
        rotxn: &RoTxn,
    ) -> Result<HashMap<OutPoint, Output>, db_error::Iter> {
        let utxos: HashMap<OutPoint, Output> = self
            .utxos
            .iter(rotxn)?
            .map(|(key, output)| Ok((key.into(), output)))
            .collect()?;
        Ok(utxos)
    }

    pub fn get_utxos_by_addresses(
        &self,
        rotxn: &RoTxn,
        addresses: &HashSet<Address>,
    ) -> Result<HashMap<OutPoint, Output>, db_error::Iter> {
        let utxos: HashMap<OutPoint, Output> = self
            .utxos
            .iter(rotxn)?
            .filter(|(_, output)| Ok(addresses.contains(&output.address)))
            .map(|(key, output)| Ok((key.into(), output)))
            .collect()?;
        Ok(utxos)
    }

    /// Get the latest failed withdrawal bundle, and the height at which it failed
    pub fn get_latest_failed_withdrawal_bundle(
        &self,
        rotxn: &RoTxn,
    ) -> Result<Option<(u32, M6id)>, db_error::TryGet> {
        let Some(latest_failed_m6id) =
            self.latest_failed_withdrawal_bundle.try_get(rotxn, &())?
        else {
            return Ok(None);
        };
        let latest_failed_m6id = latest_failed_m6id.latest().value;
        let (_bundle, bundle_status) = self.withdrawal_bundles.try_get(rotxn, &latest_failed_m6id)?
            .expect("Inconsistent DBs: latest failed m6id should exist in withdrawal_bundles");
        let bundle_status = bundle_status.latest();
        assert_eq!(bundle_status.value, WithdrawalBundleStatus::Failed);
        Ok(Some((bundle_status.height, latest_failed_m6id)))
    }

    /// Get the current Utreexo accumulator
    pub fn get_accumulator(&self, rotxn: &RoTxn) -> Result<Accumulator, Error> {
        let accumulator = self
            .utreexo_accumulator
            .try_get(rotxn, &())
            .map_err(DbError::from)?
            .unwrap_or_default();
        Ok(accumulator)
    }

    /// Regenerate utreexo proof for a tx
    pub fn regenerate_proof(
        &self,
        rotxn: &RoTxn,
        tx: &mut Transaction,
    ) -> Result<(), Error> {
        let accumulator = self.get_accumulator(rotxn)?;
        let targets: Vec<_> = tx
            .inputs
            .iter()
            .map(|(_, utxo_hash)| utxo_hash.into())
            .collect();
        tx.proof = accumulator.prove(&targets)?;
        Ok(())
    }

    /// Get a Utreexo proof for the provided utxos
    pub fn get_utreexo_proof<'a, Utxos>(
        &self,
        rotxn: &RoTxn,
        utxos: Utxos,
    ) -> Result<Proof, Error>
    where
        Utxos: IntoIterator<Item = &'a PointedOutput>,
    {
        let accumulator = self.get_accumulator(rotxn)?;
        let targets: Vec<BitcoinNodeHash> =
            utxos.into_iter().map(BitcoinNodeHash::from).collect();
        let proof = accumulator.prove(&targets)?;
        Ok(proof)
    }

    fn fill_transaction(
        &self,
        rotxn: &RoTxn,
        transaction: &Transaction,
    ) -> Result<FilledTransaction, Error> {
        let mut spent_utxos = Vec::with_capacity(transaction.inputs.len());
        for (outpoint, _) in &transaction.inputs {
            let key = OutPointKey::from(outpoint);
            let utxo =
                self.utxos.try_get(rotxn, &key)?.ok_or(Error::NoUtxo {
                    outpoint: *outpoint,
                })?;
            spent_utxos.push(utxo);
        }
        Ok(FilledTransaction {
            spent_utxos,
            transaction: transaction.clone(),
        })
    }

    pub fn fill_authorized_transaction(
        &self,
        rotxn: &RoTxn,
        transaction: AuthorizedTransaction,
    ) -> Result<Authorized<FilledTransaction>, Error> {
        let filled_tx =
            self.fill_transaction(rotxn, &transaction.transaction)?;
        let authorizations = transaction.authorizations;
        Ok(Authorized {
            transaction: filled_tx,
            authorizations,
        })
    }

    /// Get pending withdrawal bundle and block height
    pub fn get_pending_withdrawal_bundle(
        &self,
        txn: &RoTxn,
    ) -> Result<Option<(WithdrawalBundle, u32)>, Error> {
        Ok(self
            .pending_withdrawal_bundle
            .try_get(txn, &())
            .map_err(DbError::from)?)
    }

    pub fn validate_filled_transaction(
        &self,
        transaction: &FilledTransaction,
    ) -> Result<bitcoin::Amount, Error> {
        let mut value_in = bitcoin::Amount::ZERO;
        let mut value_out = bitcoin::Amount::ZERO;
        for utxo in &transaction.spent_utxos {
            value_in = value_in
                .checked_add(utxo.get_value())
                .ok_or(AmountOverflowError)?;
        }
        for output in &transaction.transaction.outputs {
            value_out = value_out
                .checked_add(output.get_value())
                .ok_or(AmountOverflowError)?;
        }
        if value_out > value_in {
            return Err(Error::NotEnoughValueIn);
        }
        value_in
            .checked_sub(value_out)
            .ok_or_else(|| AmountUnderflowError.into())
    }

    pub fn validate_transaction(
        &self,
        rotxn: &RoTxn,
        transaction: &AuthorizedTransaction,
    ) -> Result<bitcoin::Amount, Error> {
        let filled_transaction =
            self.fill_transaction(rotxn, &transaction.transaction)?;

        // Validate swap transactions
        match &transaction.transaction.data {
            TxData::SwapCreate { .. } => {
                swap::validate_swap_create(
                    self,
                    rotxn,
                    &transaction.transaction,
                    &filled_transaction,
                )?;
            }
            TxData::SwapClaim { .. } => {
                swap::validate_swap_claim(
                    self,
                    rotxn,
                    &transaction.transaction,
                    &filled_transaction,
                )?;
            }
            TxData::Regular => {
                // Validate that regular transactions don't spend locked outputs
                swap::validate_no_locked_outputs(
                    self,
                    rotxn,
                    &transaction.transaction,
                )?;
            }
        }

        for (authorization, spent_utxo) in transaction
            .authorizations
            .iter()
            .zip(filled_transaction.spent_utxos.iter())
        {
            if authorization.get_address() != spent_utxo.address {
                return Err(Error::WrongPubKeyForAddress);
            }
        }
        if Authorization::verify_transaction(transaction).is_err() {
            return Err(Error::Authorization);
        }
        let fee = self.validate_filled_transaction(&filled_transaction)?;
        Ok(fee)
    }

    const LIMIT_GROWTH_EXPONENT: f64 = 1.04;

    pub fn body_sigops_limit(height: u32) -> usize {
        // Starting body size limit is 8MB = 8 * 1024 * 1024 B
        // 2 input 2 output transaction is 392 B
        // 2 * ceil(8 * 1024 * 1024 B / 392 B) = 42800
        const START: usize = 42800;
        let month = height / (6 * 24 * 30);
        if month < 120 {
            (START as f64 * Self::LIMIT_GROWTH_EXPONENT.powi(month as i32))
                .floor() as usize
        } else {
            // 1.04 ** 120 = 110.6625
            // So we are rounding up.
            START * 111
        }
    }

    // in bytes
    pub fn body_size_limit(height: u32) -> usize {
        // 8MB starting body size limit.
        const START: usize = 8 * 1024 * 1024;
        let month = height / (6 * 24 * 30);
        if month < 120 {
            (START as f64 * Self::LIMIT_GROWTH_EXPONENT.powi(month as i32))
                .floor() as usize
        } else {
            // 1.04 ** 120 = 110.6625
            // So we are rounding up.
            START * 111
        }
    }

    pub fn get_last_deposit_block_hash(
        &self,
        rotxn: &RoTxn,
    ) -> Result<Option<bitcoin::BlockHash>, Error> {
        let block_hash = self
            .deposit_blocks
            .last(rotxn)
            .map_err(DbError::from)?
            .map(|(_, (block_hash, _))| block_hash);
        Ok(block_hash)
    }

    pub fn get_last_withdrawal_bundle_event_block_hash(
        &self,
        rotxn: &RoTxn,
    ) -> Result<Option<bitcoin::BlockHash>, Error> {
        let block_hash = self
            .withdrawal_bundle_event_blocks
            .last(rotxn)
            .map_err(DbError::from)?
            .map(|(_, (block_hash, _))| block_hash);
        Ok(block_hash)
    }

    /// Get total sidechain wealth in Bitcoin
    pub fn sidechain_wealth(
        &self,
        rotxn: &RoTxn,
    ) -> Result<bitcoin::Amount, Error> {
        let mut total_deposit_utxo_value = bitcoin::Amount::ZERO;
        self.utxos
            .iter(rotxn)
            .map_err(DbError::from)?
            .map_err(|err| DbError::from(err).into())
            .for_each(|(outpoint_key, output)| {
                let outpoint: OutPoint = outpoint_key.into();
                if let OutPoint::Deposit(_) = outpoint {
                    total_deposit_utxo_value = total_deposit_utxo_value
                        .checked_add(output.get_value())
                        .ok_or(AmountOverflowError)?;
                }
                Ok::<_, Error>(())
            })?;
        let mut total_deposit_stxo_value = bitcoin::Amount::ZERO;
        let mut total_withdrawal_stxo_value = bitcoin::Amount::ZERO;
        self.stxos
            .iter(rotxn)
            .map_err(DbError::from)?
            .map_err(|err| DbError::from(err).into())
            .for_each(|(outpoint_key, spent_output)| {
                let outpoint: OutPoint = outpoint_key.into();
                if let OutPoint::Deposit(_) = outpoint {
                    total_deposit_stxo_value = total_deposit_stxo_value
                        .checked_add(spent_output.output.get_value())
                        .ok_or(AmountOverflowError)?;
                }
                if let InPoint::Withdrawal { .. } = spent_output.inpoint {
                    total_withdrawal_stxo_value = total_deposit_stxo_value
                        .checked_add(spent_output.output.get_value())
                        .ok_or(AmountOverflowError)?;
                }
                Ok::<_, Error>(())
            })?;

        let total_wealth: bitcoin::Amount = total_deposit_utxo_value
            .checked_add(total_deposit_stxo_value)
            .ok_or(AmountOverflowError)?
            .checked_sub(total_withdrawal_stxo_value)
            .ok_or(AmountOverflowError)?;
        Ok(total_wealth)
    }

    // Swap persistence methods
    pub fn save_swap(
        &self,
        rwtxn: &mut RwTxn,
        swap: &Swap,
    ) -> Result<(), Error> {
        // Save to main swaps database
        self.swaps
            .put(rwtxn, &swap.id, swap)
            .map_err(DbError::from)?;

        // Update swaps_by_l1_txid index
        let l1_txid_key = (swap.parent_chain, swap.l1_txid.clone());
        self.swaps_by_l1_txid
            .put(rwtxn, &l1_txid_key, &swap.id)
            .map_err(DbError::from)?;

        // Update swaps_by_recipient index (only for pre-specified swaps)
        if let Some(recipient) = swap.l2_recipient {
            let mut recipient_swaps = self
                .swaps_by_recipient
                .try_get(rwtxn, &recipient)
                .map_err(DbError::from)?
                .unwrap_or_default();
            if !recipient_swaps.contains(&swap.id) {
                recipient_swaps.push(swap.id);
                self.swaps_by_recipient
                    .put(rwtxn, &recipient, &recipient_swaps)
                    .map_err(DbError::from)?;
            }
        }

        Ok(())
    }

    /// Cancel a swap (unlock outputs and mark as cancelled)
    /// Only allowed for Pending swaps (before L1 transaction is detected)
    pub fn cancel_swap(
        &self,
        rwtxn: &mut RwTxn,
        swap_id: &SwapId,
    ) -> Result<(), Error> {
        // Get swap
        let mut swap = self
            .swaps
            .try_get(rwtxn, swap_id)
            .map_err(DbError::from)?
            .ok_or(Error::SwapNotFound {
                swap_id: *swap_id,
            })?;

        // Only allow cancellation for Pending swaps
        if !matches!(swap.state, SwapState::Pending) {
            return Err(Error::InvalidTransaction(format!(
                "Swap {} cannot be cancelled (state: {:?}). Only Pending swaps can be cancelled.",
                swap_id, swap.state
            )));
        }

        // Find and unlock all outputs locked to this swap
        let mut unlocked_count = 0;
        let locked_outputs: Vec<(OutPointKey, SwapId)> = self
            .locked_swap_outputs
            .iter(rwtxn)
            .map_err(DbError::from)?
            .map(|(key, swap_id)| Ok((key, swap_id)))
            .collect()?;
        
        for (outpoint_key, locked_swap_id) in locked_outputs {
            if locked_swap_id == *swap_id {
                let outpoint: OutPoint = outpoint_key.into();
                self.unlock_output_from_swap(rwtxn, &outpoint)?;
                unlocked_count += 1;
            }
        }

        tracing::info!(
            swap_id = %swap_id,
            unlocked_outputs = %unlocked_count,
            "Cancelling swap and unlocking outputs"
        );

        // Mark swap as cancelled
        swap.state = SwapState::Cancelled;

        // Save updated swap
        self.save_swap(rwtxn, &swap)?;

        Ok(())
    }

    pub fn delete_swap(
        &self,
        rwtxn: &mut RwTxn,
        swap_id: &SwapId,
    ) -> Result<(), Error> {
        // Get swap to update indexes
        let swap = self
            .swaps
            .try_get(rwtxn, swap_id)
            .map_err(DbError::from)?
            .ok_or(Error::SwapNotFound {
                swap_id: *swap_id,
            })?;

        // Delete from swaps_by_l1_txid
        let l1_txid_key = (swap.parent_chain, swap.l1_txid.clone());
        self.swaps_by_l1_txid
            .delete(rwtxn, &l1_txid_key)
            .map_err(DbError::from)?;

        // Update swaps_by_recipient index (only for pre-specified swaps)
        if let Some(recipient) = swap.l2_recipient {
            if let Some(mut recipient_swaps) = self
                .swaps_by_recipient
                .try_get(rwtxn, &recipient)
                .map_err(DbError::from)?
            {
                recipient_swaps.retain(|id| *id != swap.id);
                if recipient_swaps.is_empty() {
                    self.swaps_by_recipient
                        .delete(rwtxn, &recipient)
                        .map_err(DbError::from)?;
                } else {
                    self.swaps_by_recipient
                        .put(rwtxn, &recipient, &recipient_swaps)
                        .map_err(DbError::from)?;
                }
            }
        }

        // Delete from main swaps database
        self.swaps.delete(rwtxn, swap_id).map_err(DbError::from)?;

        Ok(())
    }

    pub fn get_swap(
        &self,
        rotxn: &RoTxn,
        swap_id: &SwapId,
    ) -> Result<Option<Swap>, Error> {
        let swap = self.swaps.try_get(rotxn, swap_id)?;
        Ok(swap)
    }

    pub fn get_swap_by_l1_txid(
        &self,
        rotxn: &RoTxn,
        parent_chain: &ParentChainType,
        l1_txid: &SwapTxId,
    ) -> Result<Option<Swap>, Error> {
        let l1_txid_key = (*parent_chain, l1_txid.clone());
        if let Some(swap_id) = self.swaps_by_l1_txid.try_get(rotxn, &l1_txid_key)? {
            let swap = self
                .swaps
                .try_get(rotxn, &swap_id)?
                .ok_or(Error::SwapNotFound { swap_id })?;
            Ok(Some(swap))
        } else {
            Ok(None)
        }
    }

    pub fn get_swaps_by_recipient(
        &self,
        rotxn: &RoTxn,
        recipient: &Address,
    ) -> Result<Vec<Swap>, Error> {
        if let Some(swap_ids) = self.swaps_by_recipient.try_get(rotxn, recipient)? {
            let mut swaps = Vec::new();
            for swap_id in swap_ids {
                if let Some(swap) = self.swaps.try_get(rotxn, &swap_id)? {
                    swaps.push(swap);
                }
            }
            Ok(swaps)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn load_all_swaps(&self, rotxn: &RoTxn) -> Result<Vec<Swap>, Error> {
        let swaps: Vec<Swap> = self
            .swaps
            .iter(rotxn)?
            .map(|(_, swap)| Ok(swap))
            .collect()?;
        Ok(swaps)
    }

    // Output locking methods
    pub fn lock_output_to_swap(
        &self,
        rwtxn: &mut RwTxn,
        outpoint: &OutPoint,
        swap_id: &SwapId,
    ) -> Result<(), Error> {
        let key = OutPointKey::from(outpoint);
        self.locked_swap_outputs
            .put(rwtxn, &key, swap_id)
            .map_err(DbError::from)?;
        Ok(())
    }

    pub fn unlock_output_from_swap(
        &self,
        rwtxn: &mut RwTxn,
        outpoint: &OutPoint,
    ) -> Result<(), Error> {
        let key = OutPointKey::from(outpoint);
        self.locked_swap_outputs
            .delete(rwtxn, &key)
            .map_err(DbError::from)?;
        Ok(())
    }

    pub fn is_output_locked_to_swap(
        &self,
        rotxn: &RoTxn,
        outpoint: &OutPoint,
    ) -> Result<Option<SwapId>, Error> {
        let key = OutPointKey::from(outpoint);
        let swap_id = self.locked_swap_outputs.try_get(rotxn, &key)?;
        Ok(swap_id)
    }

    /// Update swap L1 transaction ID and state
    /// Called when a coinshift transaction is detected on L1
    /// For open swaps, l1_claimer_address should be the address of the person who sent the L1 transaction
    pub fn update_swap_l1_txid(
        &self,
        rwtxn: &mut RwTxn,
        swap_id: &SwapId,
        l1_txid: SwapTxId,
        confirmations: u32,
        l1_claimer_address: Option<String>,  // For open swaps
    ) -> Result<(), Error> {
        let mut swap = self
            .get_swap(rwtxn, swap_id)?
            .ok_or_else(|| Error::SwapNotFound { swap_id: *swap_id })?;

        // Update L1 txid and claimer address (for open swaps)
        if let Some(claimer_addr) = l1_claimer_address {
            swap.update_l1_transaction(l1_txid.clone(), claimer_addr);
        } else {
            swap.update_l1_txid(l1_txid.clone());
        }

        // Update state based on confirmations
        if confirmations >= swap.required_confirmations {
            swap.state = SwapState::ReadyToClaim;
        } else {
            swap.state = SwapState::WaitingConfirmations {
                current_confirmations: confirmations,
                required_confirmations: swap.required_confirmations,
            };
        }

        // Update indexes
        let old_l1_txid_key = (swap.parent_chain, swap.l1_txid.clone());
        self.swaps_by_l1_txid
            .delete(rwtxn, &old_l1_txid_key)
            .map_err(DbError::from)?;

        let new_l1_txid_key = (swap.parent_chain, l1_txid);
        self.swaps_by_l1_txid
            .put(rwtxn, &new_l1_txid_key, swap_id)
            .map_err(DbError::from)?;

        // Save updated swap
        self.save_swap(rwtxn, &swap)?;

        Ok(())
    }

    pub fn validate_block(
        &self,
        rotxn: &RoTxn,
        header: &Header,
        body: &Body,
    ) -> Result<(bitcoin::Amount, MerkleRoot), Error> {
        block::validate(self, rotxn, header, body)
    }

    pub fn connect_block(
        &self,
        rwtxn: &mut RwTxn,
        header: &Header,
        body: &Body,
    ) -> Result<MerkleRoot, Error> {
        block::connect(self, rwtxn, header, body)
    }

    /// Prevalidate a block under a read transaction, computing values reused on connect.
    pub fn prevalidate_block(
        &self,
        rotxn: &RoTxn,
        header: &Header,
        body: &Body,
    ) -> Result<PrevalidatedBlock, Error> {
        block::prevalidate(self, rotxn, header, body)
    }

    /// Connect a block using prevalidated data to avoid recomputation.
    pub fn connect_prevalidated_block(
        &self,
        rwtxn: &mut RwTxn,
        header: &Header,
        body: &Body,
        prevalidated: PrevalidatedBlock,
    ) -> Result<MerkleRoot, Error> {
        block::connect_prevalidated(self, rwtxn, header, body, prevalidated)
    }

    /// Convenience: prevalidate then connect using the same write transaction.
    pub fn apply_block(
        &self,
        rwtxn: &mut RwTxn,
        header: &Header,
        body: &Body,
    ) -> Result<(), Error> {
        let pre = self.prevalidate_block(rwtxn, header, body)?;
        let _: MerkleRoot =
            self.connect_prevalidated_block(rwtxn, header, body, pre)?;
        Ok(())
    }

    pub fn disconnect_tip(
        &self,
        rwtxn: &mut RwTxn,
        header: &Header,
        body: &Body,
    ) -> Result<(), Error> {
        block::disconnect_tip(self, rwtxn, header, body)
    }

    pub fn connect_two_way_peg_data(
        &self,
        rwtxn: &mut RwTxn,
        two_way_peg_data: &TwoWayPegData,
        rpc_config_getter: Option<&dyn Fn(ParentChainType) -> Option<crate::bitcoin_rpc::RpcConfig>>,
    ) -> Result<(), Error> {
        two_way_peg_data::connect(self, rwtxn, two_way_peg_data, rpc_config_getter)
    }

    pub fn disconnect_two_way_peg_data(
        &self,
        rwtxn: &mut RwTxn,
        two_way_peg_data: &TwoWayPegData,
    ) -> Result<(), Error> {
        two_way_peg_data::disconnect(self, rwtxn, two_way_peg_data)
    }
}

impl Watchable<()> for State {
    type WatchStream = impl Stream<Item = ()>;

    /// Get a signal that notifies whenever the tip changes
    fn watch(&self) -> Self::WatchStream {
        tokio_stream::wrappers::WatchStream::new(self.tip.watch().clone())
    }
}
