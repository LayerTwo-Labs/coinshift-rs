//! Swap data structures and types

use bitcoin::{self, hashes::Hash as _};
use blake3;
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{Address, BlockHash};

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
    utoipa::ToSchema,
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
    utoipa::ToSchema,
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
    utoipa::ToSchema,
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

    /// Get default swap expiration in L2 blocks for this chain.
    ///
    /// After this many L2 blocks, an unclaimed swap is automatically cancelled
    /// and its locked outputs are returned to the creator.
    pub fn default_swap_expiration_blocks(&self) -> u32 {
        match self {
            // ~1 week at 10min L2 blocks
            Self::BTC => 1008,
            // ~3 days for faster chains / testnets
            Self::BCH | Self::LTC | Self::Signet => 432,
            // Short expiration for testing
            Self::Regtest => 50,
        }
    }

    /// Maximum L1 confirmation age (in L1 blocks) for an L1 transaction
    /// to be accepted as a swap fill.
    ///
    /// Prevents using old, unrelated L1 transactions that happen to match
    /// the swap's address and amount.
    pub fn max_l1_tx_age_blocks(&self) -> u32 {
        match self {
            // ~2 weeks of Bitcoin blocks
            Self::BTC => 2016,
            // ~2 weeks equivalent for other chains
            Self::BCH => 2016,
            Self::LTC => 8064,
            Self::Signet => 2016,
            // Generous for testing
            Self::Regtest => 500,
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

    /// Get the default RPC port for this chain
    ///
    /// These are the standard mainnet RPC ports. Testnet/regtest ports differ.
    pub fn default_rpc_port(&self) -> u16 {
        match self {
            Self::BTC => 8332,
            Self::BCH => 8332, // Bitcoin Cash ABC/BCHN default
            Self::LTC => 9332, // Litecoin Core default
            Self::Signet => 38332, // Bitcoin Signet default
            Self::Regtest => 18443, // Bitcoin Regtest default
        }
    }

    /// Get the human-readable coin name for display
    pub fn coin_name(&self) -> &'static str {
        match self {
            Self::BTC => "Bitcoin",
            Self::BCH => "Bitcoin Cash",
            Self::LTC => "Litecoin",
            Self::Signet => "Bitcoin Signet",
            Self::Regtest => "Bitcoin Regtest",
        }
    }

    /// Get the number of satoshis (smallest unit) per coin
    ///
    /// All Bitcoin-derivative chains use 100,000,000 satoshis per coin.
    pub fn sats_per_coin(&self) -> u64 {
        100_000_000
    }

    /// Get the ticker symbol for this chain
    pub fn ticker(&self) -> &'static str {
        match self {
            Self::BTC => "BTC",
            Self::BCH => "BCH",
            Self::LTC => "LTC",
            Self::Signet => "sBTC",
            Self::Regtest => "rBTC",
        }
    }

    /// Get the default RPC URL hint for this chain
    pub fn default_rpc_url_hint(&self) -> &'static str {
        match self {
            Self::BTC => "http://localhost:8332",
            Self::BCH => "http://localhost:8332",
            Self::LTC => "http://localhost:9332",
            Self::Signet => "http://localhost:38332",
            Self::Regtest => "http://localhost:18443",
        }
    }

    /// Get all supported parent chain types
    pub fn all() -> &'static [ParentChainType] {
        &[Self::BTC, Self::BCH, Self::LTC, Self::Signet, Self::Regtest]
    }
}

/// Swap state
///
/// Note: Using tuple variants instead of named fields for better bincode compatibility
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Debug,
    Deserialize,
    Eq,
    PartialEq,
    Serialize,
    utoipa::ToSchema,
)]
pub enum SwapState {
    /// Swap created, waiting for L1 transaction
    Pending,
    /// L1 transaction detected, waiting for confirmations
    /// Tuple format: (current_confirmations, required_confirmations)
    WaitingConfirmations(u32, u32),
    /// Required confirmations reached, L2 coins can be claimed
    ReadyToClaim,
    /// L2 coins claimed, swap finished
    Completed,
    /// Swap expired or cancelled
    Cancelled,
}

impl SwapState {
    /// Get current confirmations if in WaitingConfirmations state
    pub fn current_confirmations(&self) -> Option<u32> {
        match self {
            Self::WaitingConfirmations(current, _) => Some(*current),
            _ => None,
        }
    }

    /// Get required confirmations if in WaitingConfirmations state
    pub fn required_confirmations(&self) -> Option<u32> {
        match self {
            Self::WaitingConfirmations(_, required) => Some(*required),
            _ => None,
        }
    }
}

/// Swap transaction ID representation
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Debug,
    Deserialize,
    Eq,
    Hash,
    PartialEq,
    Serialize,
    utoipa::ToSchema,
)]
pub enum SwapTxId {
    /// 32-byte transaction ID (for BTC, BCH, LTC)
    Hash32([u8; 32]),
    /// Variable-length transaction ID (for other chains)
    Hash(Vec<u8>),
}

/// Reverse bytes in place (used for Bitcoin txid RPC order ↔ canonical order).
fn reverse_32(buf: &mut [u8; 32]) {
    buf.reverse();
}

fn reversed_32(bytes: &[u8; 32]) -> [u8; 32] {
    let mut out = *bytes;
    reverse_32(&mut out);
    out
}

impl SwapTxId {
    /// Create from a bitcoin::Txid (RPC/internal byte order). Stored in canonical order.
    pub fn from_bitcoin_txid(txid: &bitcoin::Txid) -> Self {
        let rpc_order = *txid.as_ref();
        Self::Hash32(reversed_32(&rpc_order))
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

    /// Parse L1 txid from a hex string in **canonical** order (natural hash order,
    /// e.g. as shown by block explorers). Requires exactly 64 hex characters (32 bytes).
    pub fn from_hex(hex_str: &str) -> Result<Self, String> {
        let s = hex_str.trim();
        if s.len() != 64 {
            return Err(format!(
                "L1 txid must be exactly 64 hex characters (32 bytes), got {}",
                s.len()
            ));
        }
        let bytes = hex::decode(s).map_err(|e| format!("Invalid hex: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!(
                "Decoded L1 txid must be 32 bytes, got {}",
                bytes.len()
            ));
        }
        Ok(Self::from_bytes(&bytes))
    }

    /// Parse L1 txid from a hex string in **RPC** order (e.g. as returned by Bitcoin Core
    /// getrawtransaction / listunspent). Converts to canonical order for storage.
    pub fn from_hex_rpc(hex_str: &str) -> Result<Self, String> {
        let s = hex_str.trim();
        if s.len() != 64 {
            return Err(format!(
                "L1 txid must be exactly 64 hex characters (32 bytes), got {}",
                s.len()
            ));
        }
        let bytes = hex::decode(s).map_err(|e| format!("Invalid hex: {e}"))?;
        if bytes.len() != 32 {
            return Err(format!(
                "Decoded L1 txid must be 32 bytes, got {}",
                bytes.len()
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        reverse_32(&mut arr);
        Ok(Self::Hash32(arr))
    }

    pub fn to_bitcoin_txid(&self) -> Option<bitcoin::Txid> {
        match self {
            Self::Hash32(hash) => {
                Some(bitcoin::Txid::from_byte_array(reversed_32(hash)))
            }
            Self::Hash(_) => None,
        }
    }

    /// Hex encoding in **canonical** order (for display and user-facing APIs).
    pub fn to_hex(&self) -> String {
        match self {
            Self::Hash32(hash) => hex::encode(hash),
            Self::Hash(bytes) => hex::encode(bytes),
        }
    }

    /// Hex encoding in **RPC** order (for Bitcoin Core getrawtransaction, etc.).
    pub fn to_hex_rpc(&self) -> String {
        match self {
            Self::Hash32(hash) => hex::encode(reversed_32(hash)),
            Self::Hash(bytes) if bytes.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(bytes);
                reverse_32(&mut arr);
                hex::encode(arr)
            }
            Self::Hash(bytes) => hex::encode(bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_hex_requires_64_chars() {
        // Integration tests use "aa".repeat(32) etc. — must remain valid
        let valid = "aa".repeat(32);
        let txid = SwapTxId::from_hex(&valid).unwrap();
        assert_eq!(txid.to_hex(), valid);

        // Reject too short (e.g. 32 chars = 16 bytes)
        assert!(SwapTxId::from_hex("aa").is_err());
        assert!(SwapTxId::from_hex(&"a".repeat(32)).is_err());

        // Reject too long
        assert!(SwapTxId::from_hex(&"a".repeat(65)).is_err());

        // Trim and roundtrip
        let with_spaces = format!("  {}  ", valid);
        let txid2 = SwapTxId::from_hex(&with_spaces).unwrap();
        assert_eq!(txid2.to_hex(), valid);
    }

    #[test]
    fn txid_canonical_vs_rpc_byte_order() {
        // Canonical (block explorer) vs RPC (Bitcoin Core) are byte-reversed
        let canonical =
            "ceaa5bbe14a2fe2658115f32ea90a11c073a5028df5713adbcdb35c70c3e9127";
        let rpc_order =
            "27913e0cc735dbbcad1357df28503a071ca190ea325f115826fea214be5baace";
        let from_canonical = SwapTxId::from_hex(canonical).unwrap();
        let from_rpc = SwapTxId::from_hex_rpc(rpc_order).unwrap();
        assert_eq!(from_canonical.to_hex(), from_rpc.to_hex());
        assert_eq!(from_canonical.to_hex(), canonical);
        assert_eq!(from_canonical.to_hex_rpc(), rpc_order);
        assert_eq!(from_rpc.to_hex(), canonical);
        assert_eq!(from_rpc.to_hex_rpc(), rpc_order);
    }

    fn make_swap(
        chain: ParentChainType,
        created_at: u32,
        expires_at: Option<u32>,
    ) -> Swap {
        Swap::new(
            SwapId([0u8; 32]),
            SwapDirection::L2ToL1,
            chain,
            SwapTxId::Hash32([0u8; 32]),
            None,
            None,
            bitcoin::Amount::from_sat(1_000_000),
            "tb1qtest".to_string(),
            bitcoin::Amount::from_sat(500_000),
            created_at,
            expires_at,
            None,
        )
    }

    #[test]
    fn swap_creation_sets_expiration() {
        let height = 100;
        let chain = ParentChainType::Regtest;
        let expires = Some(height + chain.default_swap_expiration_blocks());
        let swap = make_swap(chain, height, expires);

        assert_eq!(swap.created_at_height, height);
        assert_eq!(swap.expires_at_height, Some(height + 50));
        assert_eq!(swap.state, SwapState::Pending);
    }

    #[test]
    fn swap_without_expiration_has_none() {
        let swap = make_swap(ParentChainType::BTC, 100, None);
        assert_eq!(swap.expires_at_height, None);
    }

    #[test]
    fn default_swap_expiration_blocks_per_chain() {
        assert_eq!(ParentChainType::BTC.default_swap_expiration_blocks(), 1008);
        assert_eq!(ParentChainType::BCH.default_swap_expiration_blocks(), 432);
        assert_eq!(ParentChainType::LTC.default_swap_expiration_blocks(), 432);
        assert_eq!(
            ParentChainType::Signet.default_swap_expiration_blocks(),
            432
        );
        assert_eq!(
            ParentChainType::Regtest.default_swap_expiration_blocks(),
            50
        );
    }

    #[test]
    fn max_l1_tx_age_blocks_per_chain() {
        assert_eq!(ParentChainType::BTC.max_l1_tx_age_blocks(), 2016);
        assert_eq!(ParentChainType::BCH.max_l1_tx_age_blocks(), 2016);
        assert_eq!(ParentChainType::LTC.max_l1_tx_age_blocks(), 8064);
        assert_eq!(ParentChainType::Signet.max_l1_tx_age_blocks(), 2016);
        assert_eq!(ParentChainType::Regtest.max_l1_tx_age_blocks(), 500);
    }

    #[test]
    fn swap_expiration_is_relative_to_creation_height() {
        let chain = ParentChainType::BTC;
        let created_at = 50_000;
        let expected_expiry =
            created_at + chain.default_swap_expiration_blocks();
        let swap = make_swap(chain, created_at, Some(expected_expiry));

        assert_eq!(swap.expires_at_height, Some(51_008));
    }

    #[test]
    fn max_l1_tx_age_exceeds_expiration_for_all_chains() {
        // max_l1_tx_age should be >= expiration blocks so that a valid swap
        // can always be filled before expiring
        for chain in ParentChainType::all() {
            assert!(
                chain.max_l1_tx_age_blocks()
                    >= chain.default_swap_expiration_blocks(),
                "{:?}: max_l1_tx_age ({}) should be >= expiration_blocks ({})",
                chain,
                chain.max_l1_tx_age_blocks(),
                chain.default_swap_expiration_blocks(),
            );
        }
    }
}

/// Swap data structure
#[derive(
    Clone, Debug, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema,
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
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    #[schema(value_type = u64)]
    pub l2_amount: bitcoin::Amount,
    pub l1_recipient_address: String,
    #[serde(with = "bitcoin::amount::serde::as_sat")]
    #[schema(value_type = u64)]
    pub l1_amount: bitcoin::Amount,
    /// Address of the person who sent the L1 transaction (the claimer)
    /// Set when L1 transaction is detected
    pub l1_claimer_address: Option<String>,
    /// L2 address that the filler (Bob) declared when providing L1 tx details.
    /// For open swaps, the claim is only valid if it pays this address.
    #[serde(default)]
    pub l2_claimer_address: Option<Address>,
    pub created_at_height: u32,
    pub expires_at_height: Option<u32>,
    /// Sidechain block hash where L1 txid was validated via parent chain RPC
    pub l1_txid_validated_at_block_hash: Option<BlockHash>,
    /// Sidechain block height where L1 txid was validated via parent chain RPC
    pub l1_txid_validated_at_height: Option<u32>,
    /// L2 address that created the swap (first input of SwapCreate). Used to restrict cancel/delete to creator.
    #[serde(default)]
    pub l2_creator_address: Option<Address>,
}

// Custom Borsh serialization for Swap (needed for integration tests)
// Amount fields are serialized as u64 for compatibility
impl BorshSerialize for Swap {
    fn serialize<W: std::io::Write>(
        &self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.id, writer)?;
        BorshSerialize::serialize(&self.direction, writer)?;
        BorshSerialize::serialize(&self.parent_chain, writer)?;
        BorshSerialize::serialize(&self.l1_txid, writer)?;
        BorshSerialize::serialize(&self.required_confirmations, writer)?;
        BorshSerialize::serialize(&self.state, writer)?;
        BorshSerialize::serialize(&self.l2_recipient, writer)?;
        // Serialize Amount as u64
        BorshSerialize::serialize(&self.l2_amount.to_sat(), writer)?;
        BorshSerialize::serialize(&self.l1_recipient_address, writer)?;
        // Serialize Amount as u64
        BorshSerialize::serialize(&self.l1_amount.to_sat(), writer)?;
        BorshSerialize::serialize(&self.l1_claimer_address, writer)?;
        BorshSerialize::serialize(&self.l2_claimer_address, writer)?;
        BorshSerialize::serialize(&self.created_at_height, writer)?;
        BorshSerialize::serialize(&self.expires_at_height, writer)?;
        BorshSerialize::serialize(
            &self.l1_txid_validated_at_block_hash,
            writer,
        )?;
        BorshSerialize::serialize(&self.l1_txid_validated_at_height, writer)?;
        BorshSerialize::serialize(&self.l2_creator_address, writer)?;
        Ok(())
    }
}

impl BorshDeserialize for Swap {
    fn deserialize_reader<R: std::io::Read>(
        reader: &mut R,
    ) -> std::io::Result<Self> {
        Ok(Self {
            id: BorshDeserialize::deserialize_reader(reader)?,
            direction: BorshDeserialize::deserialize_reader(reader)?,
            parent_chain: BorshDeserialize::deserialize_reader(reader)?,
            l1_txid: BorshDeserialize::deserialize_reader(reader)?,
            required_confirmations: BorshDeserialize::deserialize_reader(
                reader,
            )?,
            state: BorshDeserialize::deserialize_reader(reader)?,
            l2_recipient: BorshDeserialize::deserialize_reader(reader)?,
            // Deserialize u64 and convert to Amount
            l2_amount: bitcoin::Amount::from_sat(
                BorshDeserialize::deserialize_reader(reader)?,
            ),
            l1_recipient_address: BorshDeserialize::deserialize_reader(reader)?,
            // Deserialize u64 and convert to Amount
            l1_amount: bitcoin::Amount::from_sat(
                BorshDeserialize::deserialize_reader(reader)?,
            ),
            l1_claimer_address: BorshDeserialize::deserialize_reader(reader)?,
            l2_claimer_address: BorshDeserialize::deserialize_reader(reader)?,
            created_at_height: BorshDeserialize::deserialize_reader(reader)?,
            expires_at_height: BorshDeserialize::deserialize_reader(reader)?,
            l1_txid_validated_at_block_hash:
                BorshDeserialize::deserialize_reader(reader)?,
            l1_txid_validated_at_height: BorshDeserialize::deserialize_reader(
                reader,
            )?,
            l2_creator_address: BorshDeserialize::deserialize_reader(reader)?,
        })
    }
}

impl Swap {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: SwapId,
        direction: SwapDirection,
        parent_chain: ParentChainType,
        l1_txid: SwapTxId,
        required_confirmations: Option<u32>,
        l2_recipient: Option<Address>,
        l2_amount: bitcoin::Amount,
        l1_recipient_address: String,
        l1_amount: bitcoin::Amount,
        created_at_height: u32,
        expires_at_height: Option<u32>,
        l2_creator_address: Option<Address>,
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
            l2_claimer_address: None,
            created_at_height,
            expires_at_height,
            l1_txid_validated_at_block_hash: None,
            l1_txid_validated_at_height: None,
            l2_creator_address,
        }
    }

    pub fn mark_completed(&mut self) {
        self.state = SwapState::Completed;
    }

    pub fn update_l1_txid(&mut self, l1_txid: SwapTxId) {
        self.l1_txid = l1_txid;
    }

    /// Update swap with L1 transaction and claimer address (L1 address)
    pub fn update_l1_transaction(
        &mut self,
        l1_txid: SwapTxId,
        l1_claimer_address: String,
    ) {
        self.l1_txid = l1_txid;
        self.l1_claimer_address = Some(l1_claimer_address);
    }

    /// Set the L2 address that the claimer declared when filling L1 tx details.
    /// The claim will only be valid if it pays this address.
    pub fn set_l2_claimer_address(&mut self, l2_address: Address) {
        self.l2_claimer_address = Some(l2_address);
    }

    /// Set the sidechain block reference where L1 txid was validated
    pub fn set_l1_txid_validation_block(
        &mut self,
        block_hash: BlockHash,
        block_height: u32,
    ) {
        self.l1_txid_validated_at_block_hash = Some(block_hash);
        self.l1_txid_validated_at_height = Some(block_height);
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
