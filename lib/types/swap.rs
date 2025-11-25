//! Swap data structures and types

use blake3;
use bitcoin::{self, hashes::Hash as _};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Address, Hash, Txid};

/// 32-byte swap identifier
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
pub struct SwapId(pub [u8; 32]);

impl SwapId {
    /// Generate swap ID for L2 → L1 swaps
    /// If l2_recipient_address is None, creates an open swap ID
    pub fn from_l2_to_l1(
        l1_recipient_address: &str,
        l1_amount: bitcoin::Amount,
        l2_sender_address: &Address,
        l2_recipient_address: Option<&Address>,
    ) -> Self {
        let mut id_data = Vec::new();
        id_data.extend_from_slice(l1_recipient_address.as_bytes());
        id_data.extend_from_slice(&l1_amount.to_sat().to_le_bytes());
        id_data.extend_from_slice(&l2_sender_address.0);
        // Only include recipient if specified (for backward compatibility)
        if let Some(recipient) = l2_recipient_address {
            id_data.extend_from_slice(&recipient.0);
        } else {
            // For open swaps, use a fixed marker
            id_data.extend_from_slice(b"OPEN_SWAP");
        }
        let hash = blake3::hash(&id_data);
        Self(*hash.as_bytes())
    }

    /// Generate swap ID for L1 → L2 swaps (for future use)
    pub fn from_l1_to_l2(
        l1_txid: &bitcoin::Txid,
        l2_recipient_address: &Address,
    ) -> Self {
        let mut id_data = Vec::new();
        id_data.extend_from_slice(l1_txid.as_ref());
        id_data.extend_from_slice(&l2_recipient_address.0);
        let hash = blake3::hash(&id_data);
        Self(*hash.as_bytes())
    }
}

impl std::fmt::Display for SwapId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

/// Swap direction
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    PartialEq,
    Serialize,
)]
pub enum SwapDirection {
    L1ToL2,
    L2ToL1,
}

/// Parent chain type for swaps
/// Note: This can be different from the sidechain's mainchain network.
/// For example, sidechain may be on Regtest, but swaps can target Signet, Mainnet, etc.
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    Serialize,
)]
pub enum ParentChainType {
    /// Bitcoin Mainnet
    BTC,
    /// Bitcoin Cash
    BCH,
    /// Litecoin
    LTC,
    /// Bitcoin Signet (for cross-chain swaps)
    Signet,
    /// Bitcoin Regtest (for testing)
    Regtest,
}

impl ParentChainType {
    /// Get default required confirmations for this chain
    pub fn default_confirmations(&self) -> u32 {
        match self {
            Self::BTC => 6,
            Self::BCH | Self::LTC | Self::Signet | Self::Regtest => 3,
        }
    }

    /// Get the Bitcoin network enum for this chain type
    pub fn to_bitcoin_network(&self) -> bitcoin::Network {
        match self {
            Self::BTC => bitcoin::Network::Bitcoin,
            Self::Signet => bitcoin::Network::Signet,
            Self::Regtest => bitcoin::Network::Regtest,
            Self::BCH | Self::LTC => {
                // BCH and LTC are separate networks, would need separate handling
                bitcoin::Network::Bitcoin // Placeholder
            }
        }
    }
}

/// Swap state
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Debug,
    Deserialize,
    Eq,
    PartialEq,
    Serialize,
)]
pub enum SwapState {
    /// Swap created, waiting for L1 transaction
    Pending,
    /// L1 transaction detected, waiting for confirmations
    WaitingConfirmations {
        current_confirmations: u32,
        required_confirmations: u32,
    },
    /// Required confirmations reached, L2 coins can be claimed
    ReadyToClaim,
    /// L2 coins claimed, swap finished
    Completed,
    /// Swap expired or cancelled
    Cancelled,
}

/// Swap transaction ID representation
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    Serialize,
)]
pub enum SwapTxId {
    /// 32-byte transaction ID (for BTC, BCH, LTC)
    Hash32([u8; 32]),
    /// Variable-length transaction ID (for other chains)
    Hash(Vec<u8>),
}

impl SwapTxId {
    pub fn from_bitcoin_txid(txid: &bitcoin::Txid) -> Self {
        Self::Hash32(*txid.as_ref())
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.len() == 32 {
            let mut hash32 = [0u8; 32];
            hash32.copy_from_slice(bytes);
            Self::Hash32(hash32)
        } else {
            Self::Hash(bytes.to_vec())
        }
    }

    pub fn to_bitcoin_txid(&self) -> Option<bitcoin::Txid> {
        match self {
            Self::Hash32(hash) => Some(bitcoin::Txid::from_byte_array(*hash)),
            Self::Hash(_) => None,
        }
    }
}

/// Swap data structure
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Debug,
    Deserialize,
    Eq,
    PartialEq,
    Serialize,
)]
pub struct Swap {
    pub id: SwapId,
    pub direction: SwapDirection,
    pub parent_chain: ParentChainType,
    pub l1_txid: SwapTxId,
    pub required_confirmations: u32,
    pub state: SwapState,
    /// L2 recipient address. None means open swap (anyone can fill)
    pub l2_recipient: Option<Address>,
    pub l2_amount: bitcoin::Amount,
    pub l1_recipient_address: Option<String>,
    pub l1_amount: Option<bitcoin::Amount>,
    /// Address of the person who sent the L1 transaction (the claimer)
    /// Set when L1 transaction is detected
    pub l1_claimer_address: Option<String>,
    pub created_at_height: u32,
    pub expires_at_height: Option<u32>,
}

impl Swap {
    pub fn new(
        id: SwapId,
        direction: SwapDirection,
        parent_chain: ParentChainType,
        l1_txid: SwapTxId,
        required_confirmations: Option<u32>,
        l2_recipient: Option<Address>,
        l2_amount: bitcoin::Amount,
        l1_recipient_address: Option<String>,
        l1_amount: Option<bitcoin::Amount>,
        created_at_height: u32,
        expires_at_height: Option<u32>,
    ) -> Self {
        let required_confirmations = required_confirmations
            .unwrap_or_else(|| parent_chain.default_confirmations());
        Self {
            id,
            direction,
            parent_chain,
            l1_txid,
            required_confirmations,
            state: SwapState::Pending,
            l2_recipient,
            l2_amount,
            l1_recipient_address,
            l1_amount,
            l1_claimer_address: None,
            created_at_height,
            expires_at_height,
        }
    }

    pub fn mark_completed(&mut self) {
        self.state = SwapState::Completed;
    }

    pub fn update_l1_txid(&mut self, l1_txid: SwapTxId) {
        self.l1_txid = l1_txid;
    }

    /// Update swap with L1 transaction and claimer address
    pub fn update_l1_transaction(
        &mut self,
        l1_txid: SwapTxId,
        l1_claimer_address: String,
    ) {
        self.l1_txid = l1_txid;
        self.l1_claimer_address = Some(l1_claimer_address);
    }
}

/// Swap error types
#[derive(Debug, Error)]
pub enum SwapError {
    #[error("Chain not configured: {0:?}")]
    ChainNotConfigured(ParentChainType),
    #[error("Client error: {0}")]
    ClientError(String),
    #[error("Transaction disappeared")]
    TransactionDisappeared,
    #[error("Invalid state transition")]
    InvalidStateTransition,
    #[error("Swap not found")]
    SwapNotFound,
    #[error("Swap expired")]
    SwapExpired,
}

