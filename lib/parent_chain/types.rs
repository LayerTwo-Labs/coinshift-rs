//! Chain-neutral description of an L1 payment.
//!
//! These types replace the Bitcoin Core response shapes (`TransactionInfo`,
//! `Vout`, `ScriptPubKey`, `Vin`) at the boundary between the swap logic and
//! whatever parent chain it is watching. The swap logic only ever needs to know:
//! *did address A receive amount N, how final is it, and how old is it?*

use crate::types::{ParentChainType, Swap, SwapTxId};

/// What a swap is looking for on its parent chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaymentQuery {
    /// Recipient address, in the chain's own text form. Not validated here.
    pub address: String,
    /// Expected amount, in the chain's base units (sats, lamports, …).
    pub amount: u64,
    /// Confirmations required before the swap may be claimed.
    pub required_confirmations: u32,
    /// Maximum acceptable age, in the chain's own age unit. See
    /// [`ParentChainType::max_l1_tx_age`].
    pub max_age: u64,
}

impl PaymentQuery {
    /// The query that `swap` is waiting to be filled by.
    pub fn for_swap(swap: &Swap) -> Self {
        Self {
            address: swap.l1_recipient_address.clone(),
            amount: swap.l1_amount.to_sat(),
            required_confirmations: swap.required_confirmations,
            max_age: u64::from(swap.parent_chain.max_l1_tx_age()),
        }
    }
}

/// An L1 transaction considered as a possible swap fill.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct L1Payment {
    pub txid: SwapTxId,
    /// The txid rendered the way this chain's users expect to see it.
    pub txid_display: String,
    /// Base units paid to the query's address by the matching output, or 0
    /// when the transaction does not pay the query.
    pub amount: u64,
    /// Whether this transaction pays the query's address the query's amount.
    pub matches_query: bool,
    /// Best-effort identification of who sent the payment.
    pub sender: Option<String>,
    /// Finality, expressed as a confirmation depth.
    ///
    /// For a [`crate::types::ConfirmationModel::CommitmentLadder`] chain this is
    /// synthesized from the commitment level rather than measured, so it must
    /// only ever be compared against `required_confirmations` — never used as an
    /// age. Use [`Self::age`] for that.
    pub confirmations: u32,
    /// Age since inclusion, in the chain's own unit (blocks, slots, …).
    pub age: u64,
    /// Whether the transaction is in a block at all.
    pub included: bool,
    /// Inclusion height or slot.
    pub height: Option<u64>,
}

impl L1Payment {
    /// Confirmed, in a block, and recent enough to fill `query`.
    ///
    /// Age and finality are checked separately on purpose: they coincide for
    /// Bitcoin-family chains but are unrelated quantities for a chain whose
    /// finality is categorical.
    pub fn is_acceptable_for(&self, query: &PaymentQuery) -> bool {
        self.included && self.confirmations > 0 && self.age <= query.max_age
    }

    /// Whether `query`'s confirmation requirement is met.
    pub fn is_final_for(&self, query: &PaymentQuery) -> bool {
        self.confirmations >= query.required_confirmations
    }
}

/// Which chain an endpoint turned out to be serving.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainIdentity {
    pub chain: ParentChainType,
    /// The raw identifier the node reported, for error messages.
    pub raw: String,
}
