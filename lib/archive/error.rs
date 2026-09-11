use std::path::PathBuf;

use sneed::{
    EnvError, db::error::Error as DbError, rwtxn::Error as RwTxnError,
};

use super::side_tips;
use crate::types::{BlockHash, Version};

#[allow(clippy::duplicated_attributes)]
#[derive(Debug, thiserror::Error, transitive::Transitive)]
#[transitive(
    from(sneed::db::error::Delete, DbError),
    from(sneed::db::error::Get, DbError),
    from(sneed::db::error::Last, DbError),
    from(sneed::db::error::Put, DbError),
    from(sneed::db::error::TryGet, DbError),
    from(side_tips::error::DisconnectMainchainTip, side_tips::Error),
    from(side_tips::error::DisconnectSidechainTip, side_tips::Error)
)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("Database env error")]
    DbEnv(#[from] EnvError),
    #[error("Database write error")]
    DbWrite(#[from] RwTxnError),
    #[error(
        "Incompatible DB version ({}). Please clear the DB (`{}`) and re-sync",
        .version,
        .db_path.display()
    )]
    IncompatibleVersion { version: Version, db_path: PathBuf },
    #[error("invalid merkle root")]
    InvalidMerkleRoot,
    #[error("invalid previous side hash")]
    InvalidPrevSideHash,
    #[error("no accumulator for block {0}")]
    NoAccumulator(BlockHash),
    #[error("no ancestor with depth {depth} for block {block_hash}")]
    NoAncestor { block_hash: BlockHash, depth: u32 },
    #[error("no mainchain ancestor with depth {depth} for block {block_hash}")]
    NoMainAncestor {
        block_hash: bitcoin::BlockHash,
        depth: u32,
    },
    #[error("unknown block hash: {0}")]
    NoBlockHash(BlockHash),
    #[error("no BMM result with block {0}")]
    NoBmmResult(BlockHash),
    #[error("no block body with hash {0}")]
    NoBody(BlockHash),
    #[error("no deposits info for block {0}")]
    NoDepositsInfo(bitcoin::BlockHash),
    #[error("no header with hash {0}")]
    NoHeader(BlockHash),
    #[error("no height info for block hash {0}")]
    NoHeight(BlockHash),
    #[error("unknown mainchain block hash: {0}")]
    NoMainBlockHash(bitcoin::BlockHash),
    #[error("no mainchain block info for block hash {0}")]
    NoMainBlockInfo(bitcoin::BlockHash),
    #[error("no mainchain header info for block hash {0}")]
    NoMainHeaderInfo(bitcoin::BlockHash),
    #[error("no height info for mainchain block hash {0}")]
    NoMainHeight(bitcoin::BlockHash),
    #[error(transparent)]
    SideTips(Box<side_tips::Error>),
}

impl From<side_tips::Error> for Error {
    fn from(err: side_tips::Error) -> Self {
        Self::SideTips(Box::new(err))
    }
}
