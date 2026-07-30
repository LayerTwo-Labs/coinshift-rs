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

/// How a parent chain expresses finality.
///
/// Bitcoin-family chains report a monotonically increasing block depth. Other
/// chains (e.g. Solana) report a categorical commitment level instead, which the
/// adapter maps onto a synthetic depth so that [`SwapState::WaitingConfirmations`]
/// keeps its existing on-disk encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationModel {
    /// `confirmations = tip - height + 1`.
    BlockDepth,
    /// Finality is categorical; the adapter synthesizes a depth.
    CommitmentLadder,
}

/// How a parent chain encodes transaction IDs at the user-input boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxidEncoding {
    /// 32 bytes, hex, displayed in canonical (block-explorer) byte order.
    BitcoinHex,
    /// Raw base58 (no checksum), e.g. a 64-byte Solana signature.
    Base58,
}

/// Parent chain type for swaps
/// Note: This can be different from the sidechain's mainchain network.
/// For example, sidechain may be on Regtest, but swaps can target Signet, Mainnet, etc.
///
/// # Format stability
///
/// This enum is Borsh-serialized **by variant index** inside
/// [`crate::types::TxData::SwapCreate`], which is part of the block body and
/// therefore of the sidechain merkle root. It is also a bincode database key and
/// a serde map key in `l1_rpc_configs.json`. Consequently:
///
/// **New variants must be appended at the end, and existing variants must never
/// be reordered or renamed.** See `borsh_discriminants_are_stable` in the tests
/// below, which fails if this is violated.
#[derive(
    BorshSerialize,
    BorshDeserialize,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Hash,
    // Ord/PartialOrd only give the config file a stable key order; neither
    // affects the Borsh or serde encoding pinned by the tests below.
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    strum::Display,
    strum::EnumCount,
    strum::EnumIter,
    strum::EnumString,
    utoipa::ToSchema,
)]
#[strum(ascii_case_insensitive)]
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

    /// Maximum age for an L1 transaction to be accepted as a swap fill.
    ///
    /// Prevents using old, unrelated L1 transactions that happen to match
    /// the swap's address and amount.
    ///
    /// The **unit is chain-defined**: L1 blocks for the Bitcoin family, and the
    /// chain's own age unit (e.g. slots) for others. Compare it against
    /// `L1Payment::age`, never against a confirmation count — for a
    /// [`ConfirmationModel::CommitmentLadder`] chain the two are unrelated.
    pub fn max_l1_tx_age(&self) -> u32 {
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

    /// The `bitcoin::Network` this chain corresponds to, if any.
    ///
    /// Returns `None` for chains the `bitcoin` crate cannot model: BCH uses
    /// CashAddr and LTC uses its own address prefixes and bech32 HRP, so neither
    /// can be parsed or validated with `bitcoin::Address`.
    pub fn bitcoin_network(&self) -> Option<bitcoin::Network> {
        match self {
            Self::BTC => Some(bitcoin::Network::Bitcoin),
            Self::Signet => Some(bitcoin::Network::Signet),
            Self::Regtest => Some(bitcoin::Network::Regtest),
            Self::BCH | Self::LTC => None,
        }
    }

    /// How this chain reports finality.
    pub fn confirmation_model(&self) -> ConfirmationModel {
        match self {
            Self::BTC
            | Self::BCH
            | Self::LTC
            | Self::Signet
            | Self::Regtest => ConfirmationModel::BlockDepth,
        }
    }

    /// How this chain encodes transaction IDs at the user-input boundary.
    pub fn txid_encoding(&self) -> TxidEncoding {
        match self {
            Self::BTC
            | Self::BCH
            | Self::LTC
            | Self::Signet
            | Self::Regtest => TxidEncoding::BitcoinHex,
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

    /// Number of decimal places between one coin and one base unit.
    ///
    /// All Bitcoin-derivative chains use 8 (100,000,000 sats per coin). This is
    /// the single source of truth for converting between the `u64` base-unit
    /// amounts stored in [`Swap::l1_amount`] and human-readable decimal strings —
    /// see [`format_l1_amount`] and [`parse_l1_amount`].
    pub fn decimals(&self) -> u8 {
        match self {
            Self::BTC
            | Self::BCH
            | Self::LTC
            | Self::Signet
            | Self::Regtest => 8,
        }
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

    /// Human-readable label for pickers and headings, e.g. "Bitcoin Signet (sBTC)".
    ///
    /// Prefer this over an inline `match` at each UI site: adding a chain here
    /// makes it appear everywhere, and the compiler flags this one location if a
    /// new variant is added.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::BTC => "Bitcoin (BTC)",
            Self::BCH => "Bitcoin Cash Testnet 4 (BCH)",
            Self::LTC => "Litecoin (LTC)",
            Self::Signet => "Bitcoin Signet (sBTC)",
            Self::Regtest => "Bitcoin Regtest (rBTC)",
        }
    }

    /// One-line hint describing how to run a node for this chain.
    pub fn setup_hint(&self) -> &'static str {
        match self {
            Self::BTC => {
                "Use Bitcoin Core with -txindex=1 for full transaction lookup."
            }
            Self::BCH => {
                "Use Bitcoin Cash Node (BCHN) or Bitcoin ABC with -txindex=1."
            }
            Self::LTC => {
                "Use Litecoin Core with -txindex=1 for full transaction lookup."
            }
            Self::Signet => "Use Bitcoin Core with -signet -txindex=1 flags.",
            Self::Regtest => {
                "Use Bitcoin Core with -regtest -txindex=1 flags for local testing."
            }
        }
    }

    /// Best-effort validation of an L1 recipient address for this chain.
    ///
    /// For chains the `bitcoin` crate can model this is a full parse plus a
    /// network check. For BCH and LTC it is only a sanity check, because
    /// CashAddr and Litecoin's address prefixes are outside what
    /// `bitcoin::Address` can parse — see [`Self::bitcoin_network`]. Callers must
    /// therefore treat `Ok(())` as "not obviously wrong", not as "deliverable".
    pub fn validate_l1_address(&self, address: &str) -> Result<(), String> {
        let trimmed = address.trim();
        if trimmed.is_empty() {
            return Err("L1 address is empty".to_string());
        }
        if trimmed.len() != address.len() {
            return Err(
                "L1 address has leading or trailing whitespace".to_string()
            );
        }
        match self.bitcoin_network() {
            Some(network) => {
                let parsed = trimmed
                    .parse::<bitcoin::Address<bitcoin::address::NetworkUnchecked>>(
                    )
                    .map_err(|err| format!("invalid {} address: {err}", self.ticker()))?;
                if !parsed.is_valid_for_network(network) {
                    return Err(format!(
                        "address is not valid for {} ({network})",
                        self.ticker()
                    ));
                }
                Ok(())
            }
            None => {
                // CashAddr may carry a `bitcoincash:` / `bchtest:` prefix.
                let body =
                    trimmed.rsplit_once(':').map_or(trimmed, |(_, rest)| rest);
                if body.len() < 20 || body.len() > 100 {
                    return Err(format!(
                        "implausible {} address length",
                        self.ticker()
                    ));
                }
                if !body.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Err(format!(
                        "invalid character in {} address",
                        self.ticker()
                    ));
                }
                Ok(())
            }
        }
    }

    /// Get all supported parent chain types
    ///
    /// Kept as a slice for callers that need one. `all_variants_are_listed` in
    /// the tests below fails if this drifts from the enum.
    pub fn all() -> &'static [ParentChainType] {
        &[Self::BTC, Self::BCH, Self::LTC, Self::Signet, Self::Regtest]
    }
}

/// Render a base-unit amount as a decimal string using the chain's decimals.
///
/// Trailing fractional zeros are trimmed, matching
/// `bitcoin::Amount::to_string_in(Denomination::Bitcoin)` for 8-decimal chains
/// (pinned by `format_l1_amount_matches_bitcoin_for_8_decimals`).
pub fn format_l1_amount(base_units: u64, chain: ParentChainType) -> String {
    let decimals = u32::from(chain.decimals());
    if decimals == 0 {
        return base_units.to_string();
    }
    let divisor = 10u64.pow(decimals);
    let whole = base_units / divisor;
    let frac = base_units % divisor;
    if frac == 0 {
        return whole.to_string();
    }
    let frac_str = format!("{frac:0width$}", width = decimals as usize);
    format!("{whole}.{}", frac_str.trim_end_matches('0'))
}

/// Parse a decimal string into base units using the chain's decimals.
///
/// This is the counterpart to [`format_l1_amount`] and the reason it exists:
/// parsing an L1 amount with a hardcoded 8-decimal denomination silently
/// misreads any chain that does not use 8 decimals.
pub fn parse_l1_amount(s: &str, chain: ParentChainType) -> Result<u64, String> {
    let decimals = usize::from(chain.decimals());
    let s = s.trim();
    if s.is_empty() {
        return Err("amount is empty".to_string());
    }
    let (whole_str, frac_str) = s.split_once('.').unwrap_or((s, ""));
    if whole_str.is_empty() && frac_str.is_empty() {
        return Err(format!("invalid decimal amount: {s}"));
    }
    let is_digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    if !is_digits(whole_str) || !is_digits(frac_str) {
        return Err(format!("invalid decimal amount: {s}"));
    }
    if frac_str.len() > decimals {
        return Err(format!(
            "{} supports at most {decimals} decimal places",
            chain.ticker()
        ));
    }
    let whole: u64 = if whole_str.is_empty() {
        0
    } else {
        whole_str
            .parse()
            .map_err(|_| format!("amount is too large: {s}"))?
    };
    let mut padded = frac_str.to_string();
    while padded.len() < decimals {
        padded.push('0');
    }
    let frac: u64 = if padded.is_empty() {
        0
    } else {
        padded
            .parse()
            .map_err(|_| format!("invalid decimal amount: {s}"))?
    };
    whole
        .checked_mul(10u64.pow(decimals as u32))
        .and_then(|scaled| scaled.checked_add(frac))
        .ok_or_else(|| format!("amount is too large: {s}"))
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

/// Byte length of a base58-encoded transaction ID (a Solana signature).
const BASE58_TXID_LEN: usize = 64;

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

    /// Parse a base58 transaction ID (raw base58, no checksum).
    ///
    /// Used by chains whose transaction IDs are not 32-byte Bitcoin hashes —
    /// a Solana signature is 64 bytes and is quoted in base58 everywhere.
    pub fn from_base58(s: &str) -> Result<Self, String> {
        let bytes = bitcoin::base58::decode(s.trim())
            .map_err(|err| format!("invalid base58 txid: {err}"))?;
        if bytes.len() != BASE58_TXID_LEN {
            return Err(format!(
                "base58 txid must decode to {BASE58_TXID_LEN} bytes, got {}",
                bytes.len()
            ));
        }
        Ok(Self::Hash(bytes))
    }

    /// Base58 encoding. Never byte-reversed — that is a Bitcoin-only convention.
    pub fn to_base58(&self) -> String {
        match self {
            Self::Hash32(hash) => bitcoin::base58::encode(hash),
            Self::Hash(bytes) => bitcoin::base58::encode(bytes),
        }
    }

    /// Parse user-supplied txid text using `chain`'s encoding.
    ///
    /// Always dispatches on the chain and never sniffs the string, so one
    /// chain's encoding can't be silently misread as another's.
    pub fn parse_for_chain(
        chain: ParentChainType,
        s: &str,
    ) -> Result<Self, String> {
        match chain.txid_encoding() {
            TxidEncoding::BitcoinHex => Self::from_hex(s),
            TxidEncoding::Base58 => Self::from_base58(s),
        }
    }

    /// Render this txid the way users of `chain` expect to see it.
    pub fn display_for_chain(&self, chain: ParentChainType) -> String {
        match chain.txid_encoding() {
            TxidEncoding::BitcoinHex => self.to_hex(),
            TxidEncoding::Base58 => self.to_base58(),
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
    fn max_l1_tx_age_per_chain() {
        assert_eq!(ParentChainType::BTC.max_l1_tx_age(), 2016);
        assert_eq!(ParentChainType::BCH.max_l1_tx_age(), 2016);
        assert_eq!(ParentChainType::LTC.max_l1_tx_age(), 8064);
        assert_eq!(ParentChainType::Signet.max_l1_tx_age(), 2016);
        assert_eq!(ParentChainType::Regtest.max_l1_tx_age(), 500);
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
                chain.max_l1_tx_age() >= chain.default_swap_expiration_blocks(),
                "{:?}: max_l1_tx_age ({}) should be >= expiration_blocks ({})",
                chain,
                chain.max_l1_tx_age(),
                chain.default_swap_expiration_blocks(),
            );
        }
    }

    #[test]
    fn all_variants_are_listed() {
        use strum::{EnumCount as _, IntoEnumIterator as _};

        // `all()` is a hand-written slice; this fails if a variant is added to
        // the enum without being listed, or if the two orders diverge.
        assert_eq!(ParentChainType::all().len(), ParentChainType::COUNT);
        let iterated: Vec<_> = ParentChainType::iter().collect();
        assert_eq!(ParentChainType::all(), iterated.as_slice());
    }

    #[test]
    fn borsh_discriminants_are_stable() {
        // ParentChainType is Borsh-encoded by variant index inside
        // TxData::SwapCreate, which is part of the block body and therefore the
        // sidechain merkle root. Reordering or inserting a variant silently
        // changes consensus encoding, so pin every discriminant here.
        // New variants must be APPENDED, with a new line added below.
        let expected: &[(ParentChainType, u8)] = &[
            (ParentChainType::BTC, 0),
            (ParentChainType::BCH, 1),
            (ParentChainType::LTC, 2),
            (ParentChainType::Signet, 3),
            (ParentChainType::Regtest, 4),
        ];
        assert_eq!(expected.len(), ParentChainType::all().len());
        for (chain, discriminant) in expected {
            let encoded = borsh::to_vec(chain).unwrap();
            assert_eq!(
                encoded,
                vec![*discriminant],
                "{chain:?} must keep Borsh discriminant {discriminant}"
            );
        }
    }

    #[test]
    fn parent_chain_type_string_round_trip() {
        use std::str::FromStr as _;

        for chain in ParentChainType::all() {
            let rendered = chain.to_string();
            assert_eq!(ParentChainType::from_str(&rendered).unwrap(), *chain);
            // The CLI accepts lowercase; keep that working.
            assert_eq!(
                ParentChainType::from_str(&rendered.to_lowercase()).unwrap(),
                *chain
            );
            // Display must match the serde representation, since the variant
            // name is also a map key in l1_rpc_configs.json.
            let json = serde_json::to_string(chain).unwrap();
            assert_eq!(json, format!("\"{rendered}\""));
        }
        assert!(ParentChainType::from_str("dogecoin").is_err());
    }

    #[test]
    fn format_l1_amount_matches_bitcoin_for_8_decimals() {
        // Pin the new formatter against the behaviour it replaces, so display
        // for existing chains is unchanged.
        for sats in [
            0u64,
            1,
            999,
            100_000,
            100_000_000,
            123_456_789,
            2_100_000_000_000,
        ] {
            for chain in ParentChainType::all() {
                assert_eq!(chain.decimals(), 8);
                assert_eq!(
                    format_l1_amount(sats, *chain),
                    bitcoin::Amount::from_sat(sats)
                        .to_string_in(bitcoin::Denomination::Bitcoin),
                    "mismatch for {sats} sats on {chain:?}"
                );
            }
        }
    }

    #[test]
    fn parse_l1_amount_round_trips() {
        let chain = ParentChainType::BTC;
        for (text, expected) in [
            ("0", 0u64),
            ("1", 100_000_000),
            ("0.001", 100_000),
            (".001", 100_000),
            ("1.23456789", 123_456_789),
            ("  0.5  ", 50_000_000),
        ] {
            assert_eq!(parse_l1_amount(text, chain), Ok(expected), "{text}");
        }
        for sats in [0u64, 1, 100_000, 123_456_789] {
            let rendered = format_l1_amount(sats, chain);
            assert_eq!(parse_l1_amount(&rendered, chain), Ok(sats));
        }
    }

    #[test]
    fn parse_l1_amount_rejects_bad_input() {
        let chain = ParentChainType::BTC;
        // More precision than the chain has base units for: must not truncate.
        assert!(parse_l1_amount("0.123456789", chain).is_err());
        assert!(parse_l1_amount("", chain).is_err());
        assert!(parse_l1_amount("-1", chain).is_err());
        assert!(parse_l1_amount("abc", chain).is_err());
        assert!(parse_l1_amount("1.2.3", chain).is_err());
        assert!(parse_l1_amount("1e8", chain).is_err());
        // Overflow rather than wrap.
        assert!(parse_l1_amount("184467440738", chain).is_err());
    }

    #[test]
    fn base58_txid_round_trip() {
        let bytes: Vec<u8> = (0..BASE58_TXID_LEN).map(|i| i as u8).collect();
        let encoded = bitcoin::base58::encode(&bytes);
        let txid = SwapTxId::from_base58(&encoded).unwrap();
        assert_eq!(txid, SwapTxId::Hash(bytes));
        assert_eq!(txid.to_base58(), encoded);
        // Not byte-reversed, unlike the Bitcoin hex path.
        assert_eq!(
            SwapTxId::from_base58(&encoded).unwrap().to_base58(),
            encoded
        );
        // Wrong length is rejected rather than silently accepted.
        assert!(
            SwapTxId::from_base58(&bitcoin::base58::encode(&[0u8; 32]))
                .is_err()
        );
        assert!(SwapTxId::from_base58("not base58 !!!").is_err());
    }

    #[test]
    fn parse_for_chain_uses_the_chain_encoding() {
        let hex = "aa".repeat(32);
        for chain in ParentChainType::all() {
            assert_eq!(chain.txid_encoding(), TxidEncoding::BitcoinHex);
            let txid = SwapTxId::parse_for_chain(*chain, &hex).unwrap();
            assert_eq!(txid.display_for_chain(*chain), hex);
            // A base58 signature must NOT be accepted on a hex chain.
            let sig = bitcoin::base58::encode(&[7u8; BASE58_TXID_LEN]);
            assert!(SwapTxId::parse_for_chain(*chain, &sig).is_err());
        }
    }

    #[test]
    fn validate_l1_address_checks_network_where_it_can() {
        // Build genuinely valid addresses rather than hardcoding literals: the
        // bech32 HRP is covered by the checksum, so an address cannot be
        // converted between networks by swapping its prefix. (The literal used
        // in the integration tests, "bcrt1qxy2kgdygjrsq...", is exactly that
        // mistake and does not verify.)
        let pubkey: bitcoin::CompressedPublicKey =
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
                .parse()
                .unwrap();
        let regtest =
            bitcoin::Address::p2wpkh(&pubkey, bitcoin::KnownHrp::Regtest)
                .to_string();
        assert!(
            ParentChainType::Regtest
                .validate_l1_address(&regtest)
                .is_ok()
        );
        assert!(ParentChainType::BTC.validate_l1_address(&regtest).is_err());

        let mainnet =
            bitcoin::Address::p2wpkh(&pubkey, bitcoin::KnownHrp::Mainnet)
                .to_string();
        assert!(ParentChainType::BTC.validate_l1_address(&mainnet).is_ok());
        assert!(
            ParentChainType::Regtest
                .validate_l1_address(&mainnet)
                .is_err()
        );

        // A legacy base58 mainnet address (the genesis coinbase payout).
        let legacy = "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2";
        assert!(ParentChainType::BTC.validate_l1_address(legacy).is_ok());

        assert!(ParentChainType::BTC.validate_l1_address("").is_err());
        assert!(
            ParentChainType::BTC
                .validate_l1_address("nonsense")
                .is_err()
        );
        assert!(
            ParentChainType::BTC
                .validate_l1_address(" 1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2 ")
                .is_err()
        );

        // BCH/LTC get a sanity check only, since bitcoin::Address cannot parse
        // CashAddr or Litecoin prefixes.
        assert!(ParentChainType::BCH.bitcoin_network().is_none());
        assert!(
            ParentChainType::BCH
                .validate_l1_address(
                    "bchtest:qq2azmyyv6dtgczexyalqar70q036yund53jvfde0x"
                )
                .is_ok()
        );
        assert!(ParentChainType::BCH.validate_l1_address("short").is_err());
        assert!(
            ParentChainType::LTC
                .validate_l1_address(
                    "ltc1qgxm5d0v98g7hqgv8fmrsyn4mrnzu8yl8lyu6nl"
                )
                .is_ok()
        );
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
