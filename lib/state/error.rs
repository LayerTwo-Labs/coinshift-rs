use sneed::{db::error as db, env::error as env, rwtxn::error as rwtxn};
use thiserror::Error;
use transitive::Transitive;

use crate::types::{
    AmountOverflowError, AmountUnderflowError, BlockHash,
    ComputeMerkleRootError, M6id, MerkleRoot, OutPoint, SwapId, Txid,
    UtreexoError, WithdrawalBundleError, WithdrawalBundleStatus,
};

#[derive(Debug, Error)]
pub enum InvalidHeader {
    #[error("expected block hash {expected}, but computed {computed}")]
    BlockHash {
        expected: BlockHash,
        computed: BlockHash,
    },
    #[error(
        "expected previous sidechain block hash {expected:?}, but received {received:?}"
    )]
    PrevSideHash {
        expected: Option<BlockHash>,
        received: Option<BlockHash>,
    },
}

#[allow(clippy::duplicated_attributes)]
#[derive(Debug, Error, Transitive)]
#[transitive(from(db::Clear, db::Error))]
#[transitive(from(db::Delete, db::Error))]
#[transitive(from(db::Error, sneed::Error))]
#[transitive(from(db::IterInit, db::Error))]
#[transitive(from(db::IterItem, db::Error))]
#[transitive(from(db::Last, db::Error))]
#[transitive(from(db::Put, db::Error))]
#[transitive(from(db::TryGet, db::Error))]
#[transitive(from(env::CreateDb, env::Error))]
#[transitive(from(env::Error, sneed::Error))]
#[transitive(from(env::WriteTxn, env::Error))]
#[transitive(from(rwtxn::Commit, rwtxn::Error))]
#[transitive(from(rwtxn::Error, sneed::Error))]
pub enum Error {
    #[error("failed to verify authorization")]
    Authorization,
    #[error(
        "wrong number of authorizations: {inputs} inputs need {inputs} \
         authorizations, transaction carries {authorizations}"
    )]
    WrongAuthorizationCount {
        inputs: usize,
        authorizations: usize,
    },
    #[error(transparent)]
    AmountOverflow(#[from] AmountOverflowError),
    #[error(transparent)]
    AmountUnderflow(#[from] AmountUnderflowError),
    #[error("body too large")]
    BodyTooLarge,
    #[error(transparent)]
    BorshSerialize(borsh::io::Error),
    #[error(transparent)]
    ComputeMerkleRoot(#[from] ComputeMerkleRootError),
    #[error(transparent)]
    Db(#[from] sneed::Error),
    #[error(
        "invalid body: expected merkle root {expected}, but computed {computed}"
    )]
    InvalidBody {
        expected: MerkleRoot,
        computed: MerkleRoot,
    },
    #[error("invalid header: {0}")]
    InvalidHeader(InvalidHeader),
    #[error("deposit block doesn't exist")]
    NoDepositBlock,
    #[error("total fees less than coinbase value")]
    NotEnoughFees,
    #[error("no tip")]
    NoTip,
    #[error("stxo {outpoint} doesn't exist")]
    NoStxo { outpoint: OutPoint },
    #[error("value in is less than value out")]
    NotEnoughValueIn,
    #[error("utxo {outpoint} doesn't exist")]
    NoUtxo { outpoint: OutPoint },
    #[error("withdrawal output cannot be spent by a transaction")]
    SpendWithdrawalOutput,
    #[error("Withdrawal bundle event block doesn't exist")]
    NoWithdrawalBundleEventBlock,
    #[error(
        "{table} bookkeeping mismatch while disconnecting block at height \
         {block_height}: expected ({expected_hash}, {block_height}), found \
         ({found_hash}, {found_height})"
    )]
    EventBlockMismatch {
        table: &'static str,
        block_height: u32,
        expected_hash: bitcoin::BlockHash,
        found_hash: bitcoin::BlockHash,
        found_height: u32,
    },
    #[error(
        "Withdrawal bundle {m6id} event at height {block_height} is older \
         than its latest recorded status at height {latest_height}"
    )]
    WithdrawalBundleEventOutOfOrder {
        m6id: M6id,
        block_height: u32,
        latest_height: u32,
    },
    #[error(
        "Inconsistent DBs: latest failed withdrawal bundle {m6id} is missing \
         or not recorded as failed"
    )]
    InconsistentLatestFailedWithdrawalBundle { m6id: M6id },
    #[error(
        "Latest failed withdrawal bundle does not match: expected {expected} \
         at height {block_height}, found {found} at height {found_height}"
    )]
    LatestFailedWithdrawalBundleMismatch {
        expected: M6id,
        block_height: u32,
        found: M6id,
        found_height: u32,
    },
    #[error(transparent)]
    Utreexo(#[from] UtreexoError),
    #[error("Utreexo proof verification failed for tx {txid}")]
    UtreexoProofFailed { txid: Txid },
    #[error("Computed Utreexo roots do not match the header roots")]
    UtreexoRootsMismatch,
    #[error("utxo double spent")]
    UtxoDoubleSpent,
    #[error("too many sigops")]
    TooManySigops,
    #[error(
        "Unexpected status for withdrawal bundle {m6id}: {status:?} at height {status_height}, while disconnecting block at height {block_height}"
    )]
    UnexpectedWithdrawalBundleStatus {
        m6id: M6id,
        status: WithdrawalBundleStatus,
        status_height: u32,
        block_height: u32,
    },
    #[error("Unknown withdrawal bundle: {m6id}")]
    UnknownWithdrawalBundle { m6id: M6id },
    #[error(
        "Unknown withdrawal bundle confirmed in {event_block_hash}: {m6id}"
    )]
    UnknownWithdrawalBundleConfirmed {
        event_block_hash: bitcoin::BlockHash,
        m6id: M6id,
    },
    #[error("wrong public key for address")]
    WrongPubKeyForAddress,
    #[error(transparent)]
    WithdrawalBundle(#[from] WithdrawalBundleError),
    #[error("Swap not found: {swap_id}")]
    SwapNotFound { swap_id: SwapId },
    #[error(
        "Input {outpoint} is locked to swap {swap_id}, which does not exist \
         or cannot be read (orphaned lock)"
    )]
    OrphanedLock { outpoint: OutPoint, swap_id: SwapId },
    #[error("Only the swap creator can cancel or delete this swap")]
    SwapNotCreator,
    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),
    #[error(
        "L1 txid already used by another swap: {existing_swap_id} (requested for {swap_id})"
    )]
    L1TxidAlreadyUsed {
        swap_id: SwapId,
        existing_swap_id: SwapId,
    },
    #[error(transparent)]
    ParentChainRpc(#[from] crate::parent_chain_rpc::Error),
}
