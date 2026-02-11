use std::collections::{BTreeMap, HashMap, HashSet};

use bincode;
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
        OutPointKey, Output, ParentChainType, PointedOutput, SpentOutput, Swap,
        SwapId, SwapState, SwapTxId, Transaction, TxData, VERSION, Verify,
        Version, WithdrawalBundle, WithdrawalBundleStatus,
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

/// Diagnostic information about a swap entry in the database
#[derive(Debug, Clone)]
pub struct SwapDiagnostic {
    pub swap_id: SwapId,
    /// Whether the swap entry exists in the database
    pub exists: bool,
    /// Whether the swap data is corrupted (cannot be deserialized)
    pub corrupted: bool,
    /// Whether the swap can be successfully deserialized
    pub can_deserialize: bool,
    /// The swap data if it can be deserialized
    pub swap: Option<Swap>,
    /// Error message if there was an issue
    pub error: Option<String>,
}

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
    pub swaps_by_recipient:
        DatabaseUnique<SerdeBincode<Address>, SerdeBincode<Vec<SwapId>>>,
    /// Tracks which outputs are locked to which swap
    pub locked_swap_outputs: DatabaseUnique<OutPointKey, SerdeBincode<SwapId>>,
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
        tracing::debug!(
            swap_id = %swap.id,
            state = ?swap.state,
            "Saving swap to database"
        );

        // Always delete any existing swap first (even if corrupted)
        // This ensures we start with a clean slate and prevents issues with corrupted data
        // The delete operation works on keys only, so it works even if the value is corrupted
        match self.swaps.delete(rwtxn, &swap.id) {
            Ok(true) => {
                tracing::debug!(
                    swap_id = %swap.id,
                    "Deleted existing swap entry before saving new one"
                );
            }
            Ok(false) => {
                // Entry didn't exist, which is fine for new swaps
                tracing::debug!(
                    swap_id = %swap.id,
                    "No existing swap to delete, continuing with save"
                );
            }
            Err(e) => {
                // Deletion failed for some reason, but we'll continue anyway
                // The put() operation should overwrite anyway
                tracing::warn!(
                    swap_id = %swap.id,
                    error = %e,
                    "Failed to delete existing swap, but continuing with save (put should overwrite)"
                );
            }
        }

        // Verify we can serialize and deserialize the swap before saving to catch issues early
        // This helps catch serialization issues before saving to the database
        // Note: This uses bincode directly, while heed uses SerdeBincode which may have different config
        // But if basic bincode serialization fails, SerdeBincode will also fail
        match bincode::serialize(swap) {
            Ok(bytes) => {
                // Try to deserialize it immediately to verify roundtrip works
                match bincode::deserialize::<Swap>(&bytes) {
                    Ok(_) => {
                        tracing::debug!(
                            swap_id = %swap.id,
                            serialized_size = bytes.len(),
                            "Successfully serialized and deserialized swap with bincode for verification"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            swap_id = %swap.id,
                            error = %e,
                            "Failed to deserialize swap after serialization - this indicates a serialization bug. Swap will not be saved."
                        );
                        return Err(Error::InvalidTransaction(format!(
                            "Swap {} cannot be serialized/deserialized correctly - serialization bug: {}",
                            swap.id, e
                        )));
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    swap_id = %swap.id,
                    error = %e,
                    "Failed to serialize swap with bincode - this indicates a serialization bug. Swap will not be saved."
                );
                return Err(Error::InvalidTransaction(format!(
                    "Swap {} cannot be serialized - serialization bug: {}",
                    swap.id, e
                )));
            }
        }

        // Save to main swaps database
        // Log swap details before saving to help diagnose serialization issues
        tracing::debug!(
            swap_id = %swap.id,
            direction = ?swap.direction,
            parent_chain = ?swap.parent_chain,
            state = ?swap.state,
            l1_txid = ?swap.l1_txid,
            "Saving swap to database"
        );

        self.swaps.put(rwtxn, &swap.id, swap).map_err(|e| {
            tracing::error!(
                swap_id = %swap.id,
                error = %e,
                "Failed to save swap to database"
            );
            DbError::from(e)
        })?;

        // Verify we can read it back immediately to catch serialization issues
        // This checks if heed::SerdeBincode serialization matches our expectations
        let test_serialized = match bincode::serialize(swap) {
            Ok(bytes) => {
                tracing::debug!(
                    swap_id = %swap.id,
                    serialized_size = bytes.len(),
                    "Successfully serialized swap with bincode for verification"
                );
                Some(bytes)
            }
            Err(e) => {
                tracing::error!(
                    swap_id = %swap.id,
                    error = %e,
                    "Failed to serialize swap with bincode - this indicates a serialization bug"
                );
                None
            }
        };

        // Now try to read it back from the database
        match self.swaps.try_get(rwtxn, &swap.id) {
            Ok(Some(read_swap)) => {
                // Verify the read swap matches what we saved
                if read_swap.id != swap.id {
                    tracing::error!(
                        swap_id = %swap.id,
                        read_swap_id = %read_swap.id,
                        "Swap ID mismatch after read - database corruption?"
                    );
                } else {
                    tracing::debug!(
                        swap_id = %swap.id,
                        "Successfully verified swap can be read back after saving"
                    );
                }
            }
            Ok(None) => {
                tracing::warn!(
                    swap_id = %swap.id,
                    "Swap was saved but cannot be read back immediately - possible serialization issue"
                );
                // Try to deserialize our test serialization to see if that works
                if let Some(test_bytes) = &test_serialized {
                    match bincode::deserialize::<Swap>(test_bytes) {
                        Ok(_) => {
                            tracing::warn!(
                                swap_id = %swap.id,
                                "Test serialization/deserialization works, but database read fails - possible database corruption"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                swap_id = %swap.id,
                                error = %e,
                                "Test serialization/deserialization also fails - this confirms a serialization bug"
                            );
                        }
                    }
                }
            }
            Err(e) => {
                // Log the full error chain to help diagnose the issue
                let err_str = format!("{e:#}");
                let err_debug = format!("{e:?}");

                // Check if it's the InvalidTagEncoding error
                let is_invalid_tag = err_str.contains("InvalidTagEncoding")
                    || err_debug.contains("InvalidTagEncoding");

                // Try to get more information about what was actually written
                tracing::error!(
                    swap_id = %swap.id,
                    error = %e,
                    error_display = %err_str,
                    error_debug = ?e,
                    error_chain = %err_debug,
                    is_invalid_tag_encoding = is_invalid_tag,
                    direction = ?swap.direction,
                    parent_chain = ?swap.parent_chain,
                    state = ?swap.state,
                    l1_txid = ?swap.l1_txid,
                    l1_recipient_address = ?swap.l1_recipient_address,
                    l1_claimer_address = ?swap.l1_claimer_address,
                    "Failed to read back swap after saving - serialization/deserialization mismatch. This indicates the swap was saved in an invalid format."
                );

                // If we have test serialization, try to deserialize it to see if the issue is with the database or the serialization itself
                if let Some(test_bytes) = &test_serialized {
                    match bincode::deserialize::<Swap>(test_bytes) {
                        Ok(_) => {
                            tracing::error!(
                                swap_id = %swap.id,
                                "Test serialization/deserialization works, but database read fails - possible database corruption or encoding mismatch"
                            );
                        }
                        Err(deser_err) => {
                            tracing::error!(
                                swap_id = %swap.id,
                                deserialization_error = %deser_err,
                                "Test serialization/deserialization also fails - this confirms a serialization bug in the Swap struct"
                            );
                        }
                    }
                }

                // This is a critical error - the swap was saved but can't be read back
                // Delete it to prevent further issues and avoid crashing the net task
                tracing::warn!(
                    swap_id = %swap.id,
                    "Deleting corrupted swap that was just saved to prevent further errors"
                );
                drop(self.swaps.delete(rwtxn, &swap.id));

                // Return error but don't crash - let the caller handle it
                return Err(Error::InvalidTransaction(format!(
                    "Swap {} was saved but cannot be deserialized - serialization format error: {}. This is likely a bug in the serialization code.",
                    swap.id, err_str
                )));
            }
        }

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
        // Get swap - use get_swap which handles deserialization errors gracefully
        let mut swap = self
            .get_swap(rwtxn, swap_id)?
            .ok_or(Error::SwapNotFound { swap_id: *swap_id })?;

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
        // Use get_swap which handles deserialization errors gracefully
        if let Some(swap) = self.get_swap(rwtxn, swap_id)? {
            // Delete from swaps_by_l1_txid
            let l1_txid_key = (swap.parent_chain, swap.l1_txid.clone());
            self.swaps_by_l1_txid
                .delete(rwtxn, &l1_txid_key)
                .map_err(DbError::from)?;

            // Update swaps_by_recipient index (only for pre-specified swaps)
            if let Some(recipient) = swap.l2_recipient
                && let Some(mut recipient_swaps) = self
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
        } else {
            // Swap not found or corrupted - log warning but still try to delete
            tracing::warn!(
                swap_id = %swap_id,
                "Swap not found or corrupted when deleting, attempting to delete from database and unlock outputs"
            );

            // Even if swap is corrupted, unlock all outputs locked to it
            self.unlock_all_outputs_for_swap(rwtxn, swap_id)?;
        }

        // Delete from main swaps database (even if swap was corrupted/unreadable)
        self.swaps.delete(rwtxn, swap_id).map_err(DbError::from)?;

        Ok(())
    }

    /// Unlock all outputs locked to a specific swap
    /// This is useful when a swap is corrupted and can't be read normally
    pub fn unlock_all_outputs_for_swap(
        &self,
        rwtxn: &mut RwTxn,
        swap_id: &SwapId,
    ) -> Result<u32, Error> {
        let mut unlocked_count = 0;
        let locked_outputs: Vec<(OutPointKey, SwapId)> = self
            .locked_swap_outputs
            .iter(rwtxn)
            .map_err(DbError::from)?
            .map(|(key, locked_swap_id)| Ok((key, locked_swap_id)))
            .collect()?;

        for (outpoint_key, locked_swap_id) in locked_outputs {
            if locked_swap_id == *swap_id {
                let outpoint: OutPoint = outpoint_key.into();
                self.unlock_output_from_swap(rwtxn, &outpoint)?;
                unlocked_count += 1;
            }
        }

        if unlocked_count > 0 {
            tracing::info!(
                swap_id = %swap_id,
                unlocked_outputs = unlocked_count,
                "Unlocked {} outputs for swap",
                unlocked_count
            );
        }

        Ok(unlocked_count)
    }

    /// Find and clean up orphaned locks (outputs locked to swaps that don't exist or are corrupted)
    /// Returns the number of orphaned locks cleaned up
    pub fn cleanup_orphaned_locks(
        &self,
        rwtxn: &mut RwTxn,
    ) -> Result<u32, Error> {
        let mut cleaned_count = 0;
        let locked_outputs: Vec<(OutPointKey, SwapId)> = self
            .locked_swap_outputs
            .iter(rwtxn)
            .map_err(DbError::from)?
            .map(|(key, swap_id)| Ok((key, swap_id)))
            .collect()?;

        for (outpoint_key, swap_id) in locked_outputs {
            // Check if swap exists and can be deserialized
            match self.get_swap(rwtxn, &swap_id) {
                Ok(Some(_)) => {
                    // Swap exists and is valid, keep the lock
                    continue;
                }
                Ok(None) => {
                    // Swap doesn't exist - orphaned lock
                    let outpoint: OutPoint = outpoint_key.into();
                    tracing::warn!(
                        swap_id = %swap_id,
                        outpoint = ?outpoint,
                        "Found orphaned lock: output locked to non-existent swap, unlocking"
                    );
                    self.unlock_output_from_swap(rwtxn, &outpoint)?;
                    cleaned_count += 1;
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
                        let outpoint: OutPoint = outpoint_key.into();
                        tracing::warn!(
                            swap_id = %swap_id,
                            outpoint = ?outpoint,
                            error = %err,
                            "Found orphaned lock: output locked to corrupted swap, unlocking"
                        );
                        self.unlock_output_from_swap(rwtxn, &outpoint)?;
                        cleaned_count += 1;
                    } else {
                        // Other database error - log but don't unlock (might be transient)
                        tracing::error!(
                            swap_id = %swap_id,
                            error = %err,
                            "Error checking swap for orphaned lock cleanup, skipping"
                        );
                    }
                }
            }
        }

        if cleaned_count > 0 {
            tracing::info!(
                cleaned_orphaned_locks = cleaned_count,
                "Cleaned up {} orphaned locks",
                cleaned_count
            );
        } else {
            tracing::debug!("No orphaned locks found");
        }

        Ok(cleaned_count)
    }

    pub fn get_swap(
        &self,
        rotxn: &RoTxn,
        swap_id: &SwapId,
    ) -> Result<Option<Swap>, Error> {
        match self.swaps.try_get(rotxn, swap_id) {
            Ok(swap) => Ok(swap),
            Err(err) => {
                // If deserialization fails (corrupted data), log warning and return None
                // This allows the system to continue working even with corrupted swap entries
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
                    // Log the full error chain to help diagnose the issue
                    let error_chain = format!("{:?}", err);
                    tracing::warn!(
                        swap_id = %swap_id,
                        error = %err,
                        error_debug = ?err,
                        error_display = %err_str,
                        error_chain = %error_chain,
                        "Failed to deserialize swap from database (corrupted data), treating as non-existent"
                    );
                    Ok(None)
                } else {
                    // For other errors, log with more context and propagate them
                    tracing::error!(
                        swap_id = %swap_id,
                        error = %err,
                        error_debug = ?err,
                        error_display = %err_str,
                        "Database error when getting swap (not a deserialization error)"
                    );
                    Err(err.into())
                }
            }
        }
    }

    pub fn get_swap_by_l1_txid(
        &self,
        rotxn: &RoTxn,
        parent_chain: &ParentChainType,
        l1_txid: &SwapTxId,
    ) -> Result<Option<Swap>, Error> {
        let l1_txid_key = (*parent_chain, l1_txid.clone());
        if let Some(swap_id) =
            self.swaps_by_l1_txid.try_get(rotxn, &l1_txid_key)?
        {
            // Use get_swap which handles deserialization errors gracefully
            self.get_swap(rotxn, &swap_id)
        } else {
            Ok(None)
        }
    }

    pub fn get_swaps_by_recipient(
        &self,
        rotxn: &RoTxn,
        recipient: &Address,
    ) -> Result<Vec<Swap>, Error> {
        if let Some(swap_ids) =
            self.swaps_by_recipient.try_get(rotxn, recipient)?
        {
            let mut swaps = Vec::new();
            for swap_id in swap_ids {
                // Use get_swap which handles deserialization errors gracefully
                if let Some(swap) = self.get_swap(rotxn, &swap_id)? {
                    swaps.push(swap);
                }
            }
            Ok(swaps)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn load_all_swaps(&self, rotxn: &RoTxn) -> Result<Vec<Swap>, Error> {
        let mut swaps = Vec::new();
        let mut iter = self.swaps.iter(rotxn)?;
        let mut processed_count = 0;
        let mut corrupted_count = 0;
        let mut consecutive_errors = 0;
        const MAX_CONSECUTIVE_ERRORS: u32 = 10;

        // Try to get all swap IDs first, then try to load each one individually
        // This helps us identify which specific swaps are corrupted
        let mut all_swap_ids = Vec::new();
        loop {
            match iter.next() {
                Ok(Some((swap_id, _swap))) => {
                    // Successfully deserialized - add to both lists
                    all_swap_ids.push(swap_id);
                    swaps.push(_swap);
                    processed_count += 1;
                    consecutive_errors = 0;
                }
                Ok(None) => {
                    // End of iterator
                    break;
                }
                Err(err) => {
                    // Can't deserialize during iteration - try to get the key anyway
                    corrupted_count += 1;
                    consecutive_errors += 1;
                    let err_str = format!("{err:#}");
                    tracing::warn!(
                        error = %err,
                        error_debug = ?err,
                        error_display = %err_str,
                        processed_count = processed_count,
                        corrupted_count = corrupted_count,
                        consecutive_errors = consecutive_errors,
                        loaded_swaps = swaps.len(),
                        "Failed to deserialize swap from database during iteration at position {}. Will try to identify corrupted swap by scanning all swap IDs.",
                        processed_count
                    );

                    // If we have too many consecutive errors, try a different approach
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        tracing::warn!(
                            total_swaps_loaded = swaps.len(),
                            corrupted_swaps_skipped = corrupted_count,
                            consecutive_errors = consecutive_errors,
                            "Too many consecutive errors during swap iteration. Switching to individual swap ID lookup method."
                        );
                        // Break out and try loading by individual IDs
                        break;
                    }

                    // Try to continue - if iterator is stuck, we'll break out above
                    continue;
                }
            }
        }

        // If we had errors, try to load swaps individually by getting all keys first
        // This is slower but can help identify which swaps are corrupted
        if corrupted_count > 0 || consecutive_errors > 0 {
            tracing::info!(
                "Attempting to load swaps individually to identify corrupted entries"
            );
            // Get all swap IDs by trying to iterate keys only
            // Note: This might not work if the key itself is corrupted, but it's worth trying
            let mut key_iter = self.swaps.iter(rotxn)?;
            let mut individual_loaded = 0;
            loop {
                match key_iter.next() {
                    Ok(Some((swap_id, _))) => {
                        // Try to load this swap individually
                        match self.get_swap(rotxn, &swap_id) {
                            Ok(Some(swap)) => {
                                // Only add if not already in the list
                                if !swaps.iter().any(|s| s.id == swap_id) {
                                    swaps.push(swap);
                                    individual_loaded += 1;
                                }
                            }
                            Ok(None) => {
                                // Swap doesn't exist or is corrupted - already logged by get_swap
                            }
                            Err(e) => {
                                tracing::debug!(
                                    swap_id = %swap_id,
                                    error = %e,
                                    "Error loading individual swap"
                                );
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        // Can't even iterate keys - give up
                        break;
                    }
                }
            }
            if individual_loaded > 0 {
                tracing::info!(
                    individually_loaded = individual_loaded,
                    "Loaded {individual_loaded} additional swaps using individual lookup"
                );
            }
        }

        if corrupted_count > 0 {
            tracing::warn!(
                total_swaps_loaded = swaps.len(),
                corrupted_swaps_skipped = corrupted_count,
                "Completed loading swaps: {} loaded, {} corrupted swaps skipped",
                swaps.len(),
                corrupted_count
            );
        }

        Ok(swaps)
    }

    /// Check if a specific swap is corrupted (cannot be deserialized)
    /// Returns Ok(true) if corrupted, Ok(false) if valid, Err if there's a database error
    pub fn check_swap_corrupted(
        &self,
        rotxn: &RoTxn,
        swap_id: &SwapId,
    ) -> Result<bool, Error> {
        match self.swaps.try_get(rotxn, swap_id) {
            Ok(Some(_)) => Ok(false), // Swap exists and can be deserialized
            Ok(None) => Ok(false), // Swap doesn't exist (not corrupted, just missing)
            Err(err) => {
                // Check if it's a deserialization error
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
                    Ok(true) // Swap exists but is corrupted
                } else {
                    // Other database error
                    Err(err.into())
                }
            }
        }
    }

    /// Delete all corrupted swaps from the database
    /// This is useful for cleaning up corrupted data that prevents normal operation
    /// Returns the number of corrupted swaps deleted
    pub fn cleanup_corrupted_swaps(
        &self,
        rwtxn: &mut RwTxn,
    ) -> Result<u32, Error> {
        // First, find all corrupted swaps using a read transaction
        // We need to do this in two steps because we can't mutate while iterating
        let corrupted_swaps = {
            // Create a read-only transaction for finding corrupted swaps
            let rotxn = rwtxn as &RoTxn;
            self.find_corrupted_swaps(rotxn)?
        };

        let mut deleted_count = 0;
        for swap_id in &corrupted_swaps {
            tracing::warn!(
                swap_id = %swap_id,
                "Deleting corrupted swap"
            );
            // Delete directly from database (bypassing get_swap which would fail)
            drop(self.swaps.delete(rwtxn, swap_id));
            deleted_count += 1;
        }

        if deleted_count > 0 {
            tracing::info!(
                deleted_count = deleted_count,
                "Cleaned up {} corrupted swaps from database",
                deleted_count
            );
        }

        Ok(deleted_count)
    }

    /// Find all corrupted swaps by attempting to load each swap individually
    /// This is more thorough than load_all_swaps but slower
    /// Returns a list of swap_ids that are corrupted
    pub fn find_corrupted_swaps(
        &self,
        rotxn: &RoTxn,
    ) -> Result<Vec<SwapId>, Error> {
        let mut corrupted_swaps = Vec::new();
        let mut iter = self.swaps.iter(rotxn)?;
        let mut total_checked = 0;
        let mut total_valid = 0;

        loop {
            match iter.next() {
                Ok(Some((_swap_id, _swap))) => {
                    total_checked += 1;
                    total_valid += 1;
                    // Swap loaded successfully, not corrupted
                }
                Ok(None) => {
                    // End of iterator
                    break;
                }
                Err(err) => {
                    // This is tricky - we can't get the swap_id from the error
                    // But we can try to identify it by attempting to load swaps individually
                    // For now, we'll log the error and try to continue
                    total_checked += 1;
                    let err_str = format!("{err:#}");
                    tracing::warn!(
                        error = %err,
                        error_debug = ?err,
                        error_display = %err_str,
                        total_checked = total_checked,
                        "Encountered error during iteration, will attempt to identify corrupted swap"
                    );
                    // We can't identify the swap_id from the iterator error
                    // The caller should use find_corrupted_swaps_by_scanning for a more thorough check
                }
            }
        }

        // Now try a different approach: iterate through all swaps and try to load each one
        // This is slower but can identify which specific swaps are corrupted
        tracing::info!(
            total_checked = total_checked,
            total_valid = total_valid,
            "Completed initial scan. Starting detailed scan to identify corrupted swaps..."
        );

        // Re-iterate and try to get each swap individually
        let mut detailed_iter = self.swaps.iter(rotxn)?;
        loop {
            match detailed_iter.next() {
                Ok(Some((swap_id, _))) => {
                    // Try to get the swap again to see if it's corrupted
                    match self.check_swap_corrupted(rotxn, &swap_id) {
                        Ok(true) => {
                            tracing::warn!(
                                swap_id = %swap_id,
                                "Found corrupted swap"
                            );
                            corrupted_swaps.push(swap_id);
                        }
                        Ok(false) => {
                            // Valid swap
                        }
                        Err(e) => {
                            tracing::error!(
                                swap_id = %swap_id,
                                error = %e,
                                "Error checking if swap is corrupted"
                            );
                        }
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    // Can't get swap_id from iterator error, skip
                    let err_str = format!("{err:#}");
                    tracing::warn!(
                        error = %err,
                        error_display = %err_str,
                        "Skipping entry that cannot be iterated"
                    );
                    // Try to continue - if iterator is broken, we'll hit this repeatedly
                    // and eventually break out
                    continue;
                }
            }
        }

        if !corrupted_swaps.is_empty() {
            tracing::warn!(
                corrupted_count = corrupted_swaps.len(),
                corrupted_swap_ids = ?corrupted_swaps.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "Found {} corrupted swaps in database",
                corrupted_swaps.len()
            );
        } else {
            tracing::info!("No corrupted swaps found in database");
        }

        Ok(corrupted_swaps)
    }

    /// Get diagnostic information about a swap entry
    /// Returns information about whether the swap exists, is corrupted, or has other issues
    pub fn diagnose_swap(
        &self,
        rotxn: &RoTxn,
        swap_id: &SwapId,
    ) -> Result<SwapDiagnostic, Error> {
        // Check if swap exists in database (even if corrupted)
        let exists_in_db = self.swaps.try_get(rotxn, swap_id);

        match exists_in_db {
            Ok(Some(swap)) => {
                // Swap exists and can be deserialized
                Ok(SwapDiagnostic {
                    swap_id: *swap_id,
                    exists: true,
                    corrupted: false,
                    can_deserialize: true,
                    swap: Some(swap),
                    error: None,
                })
            }
            Ok(None) => {
                // Swap doesn't exist
                Ok(SwapDiagnostic {
                    swap_id: *swap_id,
                    exists: false,
                    corrupted: false,
                    can_deserialize: false,
                    swap: None,
                    error: None,
                })
            }
            Err(err) => {
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

                Ok(SwapDiagnostic {
                    swap_id: *swap_id,
                    exists: true, // Entry exists but can't be deserialized
                    corrupted: is_deserialization_error,
                    can_deserialize: false,
                    swap: None,
                    error: Some(format!("{}", err)),
                })
            }
        }
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
    /// block_hash and block_height are the sidechain block where this update occurs
    #[allow(clippy::too_many_arguments)]
    pub fn update_swap_l1_txid(
        &self,
        rwtxn: &mut RwTxn,
        swap_id: &SwapId,
        l1_txid: SwapTxId,
        confirmations: u32,
        l1_claimer_address: Option<String>, // For open swaps
        block_hash: BlockHash,
        block_height: u32,
    ) -> Result<(), Error> {
        let mut swap = self
            .get_swap(rwtxn, swap_id)?
            .ok_or_else(|| Error::SwapNotFound { swap_id: *swap_id })?;

        // L1 transaction uniqueness: do not allow an L1 tx already used by another swap
        if let Some(existing) =
            self.get_swap_by_l1_txid(rwtxn, &swap.parent_chain, &l1_txid)?
        {
            if existing.id != *swap_id {
                return Err(Error::L1TxidAlreadyUsed {
                    swap_id: *swap_id,
                    existing_swap_id: existing.id,
                });
            }
        }

        // Save the old l1_txid BEFORE updating the swap (needed for index deletion)
        let old_l1_txid = swap.l1_txid.clone();

        // Update L1 txid and claimer address (for open swaps)
        if let Some(claimer_addr) = l1_claimer_address {
            swap.update_l1_transaction(l1_txid.clone(), claimer_addr);
        } else {
            swap.update_l1_txid(l1_txid.clone());
        }

        // Save the sidechain block reference where this validation occurred
        swap.set_l1_txid_validation_block(block_hash, block_height);

        // Update state based on confirmations
        if confirmations >= swap.required_confirmations {
            swap.state = SwapState::ReadyToClaim;
        } else {
            swap.state = SwapState::WaitingConfirmations(
                confirmations,
                swap.required_confirmations,
            );
        }

        // Update indexes - use the old_l1_txid we saved before updating
        let old_l1_txid_key = (swap.parent_chain, old_l1_txid);
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

    /// Reconstruct all swaps from the blockchain by scanning all blocks
    /// This is useful for:
    /// - Recovering from database corruption
    /// - Verifying swap database integrity
    /// - Syncing swap history for new nodes
    ///
    /// This function scans all blocks from genesis to tip and reconstructs
    /// all swaps from SwapCreate and SwapClaim transactions.
    pub fn reconstruct_swaps_from_blockchain(
        &self,
        rwtxn: &mut RwTxn,
        archive: &crate::archive::Archive,
        tip_hash: Option<BlockHash>,
    ) -> Result<u32, Error> {
        tracing::info!("Starting swap reconstruction from blockchain");

        // Get the tip hash
        let tip = tip_hash.or_else(|| self.try_get_tip(rwtxn).ok()?);
        let Some(tip) = tip else {
            tracing::warn!("No tip found, nothing to reconstruct");
            return Ok(0);
        };

        // Get tip height
        let tip_height = archive.get_height(rwtxn, tip).map_err(|e| {
            Error::InvalidTransaction(format!(
                "Failed to get tip height: {}",
                e
            ))
        })?;
        tracing::info!(
            tip_hash = %tip,
            tip_height = tip_height,
            "Reconstructing swaps from genesis to tip"
        );

        // Clear existing swaps database (we'll rebuild from blockchain)
        // But keep indexes - we'll rebuild them too
        tracing::info!("Collecting all block hashes from genesis to tip");

        // First, collect all block hashes from genesis to tip
        let mut block_hashes = Vec::new();
        let mut current_hash = Some(tip);
        while let Some(block_hash) = current_hash {
            block_hashes.push(block_hash);
            let header =
                archive.get_header(rwtxn, block_hash).map_err(|e| {
                    Error::InvalidTransaction(format!(
                        "Failed to get header: {}",
                        e
                    ))
                })?;
            current_hash = header.prev_side_hash;
        }

        // Reverse to process from genesis to tip
        block_hashes.reverse();

        tracing::info!(
            total_blocks = block_hashes.len(),
            "Processing {} blocks to reconstruct swaps",
            block_hashes.len()
        );

        let mut swap_count = 0;

        // Iterate forwards from genesis to tip
        for (height, block_hash) in block_hashes.iter().enumerate() {
            let height = height as u32;

            // Get block header and body
            let _header =
                archive.get_header(rwtxn, *block_hash).map_err(|e| {
                    Error::InvalidTransaction(format!(
                        "Failed to get header: {}",
                        e
                    ))
                })?;
            let body = archive.get_body(rwtxn, *block_hash).map_err(|e| {
                Error::InvalidTransaction(format!("Failed to get body: {}", e))
            })?;

            // Process transactions in reverse order (to handle claims before creates)
            // Actually, we should process in forward order to match how blocks were connected
            for transaction in &body.transactions {
                let txid = transaction.txid();

                // Fill transaction to get spent UTXOs (needed for swap reconstruction)
                let filled = self.fill_transaction(rwtxn, transaction)?;

                match &transaction.data {
                    TxData::SwapCreate {
                        swap_id,
                        parent_chain,
                        l1_txid_bytes,
                        required_confirmations,
                        l2_recipient,
                        l2_amount,
                        l1_recipient_address,
                        l1_amount,
                    } => {
                        let swap_id = SwapId(*swap_id);

                        // Reconstruct L1 txid
                        let l1_txid = SwapTxId::from_bytes(l1_txid_bytes);

                        // Reconstruct swap object
                        let swap = Swap::new(
                            swap_id,
                            crate::types::SwapDirection::L2ToL1,
                            *parent_chain,
                            l1_txid,
                            Some(*required_confirmations),
                            *l2_recipient,
                            bitcoin::Amount::from_sat(*l2_amount),
                            l1_recipient_address.clone(),
                            l1_amount.map(bitcoin::Amount::from_sat),
                            height,
                            None, // TODO: Add expiration support
                        );

                        // Lock outputs for L2 → L1 swaps
                        // Only lock SwapPending outputs (never change outputs)
                        if l1_recipient_address.is_some() {
                            for (vout, output) in
                                filled.transaction.outputs.iter().enumerate()
                            {
                                if matches!(
                                    output.content,
                                    crate::types::OutputContent::SwapPending { .. }
                                ) {
                                    let outpoint = OutPoint::Regular {
                                        txid,
                                        vout: vout as u32,
                                    };
                                    // Only lock if not already locked (to handle reorgs)
                                    if self
                                        .is_output_locked_to_swap(
                                            rwtxn, &outpoint,
                                        )?
                                        .is_none()
                                    {
                                        self.lock_output_to_swap(
                                            rwtxn, &outpoint, &swap_id,
                                        )?;
                                    }
                                }
                            }
                        }

                        // Save swap (will overwrite if exists)
                        self.save_swap(rwtxn, &swap)?;
                        swap_count += 1;

                        if swap_count % 100 == 0 {
                            tracing::debug!(
                                reconstructed_swaps = swap_count,
                                current_height = height,
                                "Reconstructing swaps..."
                            );
                        }
                    }
                    TxData::SwapClaim { swap_id, .. } => {
                        let swap_id = SwapId(*swap_id);

                        // Get swap and update its state
                        if let Some(mut swap) =
                            self.get_swap(rwtxn, &swap_id)?
                        {
                            // Unlock outputs
                            for (outpoint, _) in &filled.transaction.inputs {
                                if self
                                    .is_output_locked_to_swap(rwtxn, outpoint)?
                                    == Some(swap_id)
                                {
                                    self.unlock_output_from_swap(
                                        rwtxn, outpoint,
                                    )?;
                                }
                            }

                            // Mark swap as completed
                            swap.mark_completed();
                            self.save_swap(rwtxn, &swap)?;
                        } else {
                            tracing::warn!(
                                swap_id = %swap_id,
                                block_height = height,
                                "SwapClaim found but swap not found in database (may have been created in a later block)"
                            );
                        }
                    }
                    TxData::Regular => {}
                }
            }

            if height.is_multiple_of(1000) && height > 0 {
                tracing::info!(
                    processed_blocks = height,
                    total_blocks = block_hashes.len(),
                    reconstructed_swaps = swap_count,
                    "Reconstruction progress"
                );
            }
        }

        tracing::info!(
            reconstructed_swaps = swap_count,
            "Completed swap reconstruction from blockchain"
        );

        Ok(swap_count)
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
        rpc_config_getter: Option<
            &dyn Fn(ParentChainType) -> Option<crate::parent_chain_rpc::RpcConfig>,
        >,
        wallet: Option<&crate::wallet::Wallet>,
    ) -> Result<(), Error> {
        two_way_peg_data::connect(
            self,
            rwtxn,
            two_way_peg_data,
            rpc_config_getter,
            wallet,
        )
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
