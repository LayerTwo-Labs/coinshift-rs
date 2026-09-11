//! Proof that a swap's parent-chain payment happened, verifiable by every
//! node from block data alone.
//!
//! A `SwapClaim` carries an [`L1PaymentProof`] in its `proof_data` field: the
//! parent-chain block header, a merkle branch (together, the `MerkleBlock`
//! that Bitcoin Core's `gettxoutproof` returns) and the raw payment
//! transaction. Nothing in it is trusted as supplied. The header is believed
//! only because its hash is a mainchain block the node has already validated
//! through the enforcer and which lies on the ancestry of the L2 block being
//! validated; the transaction is believed only because it hashes into that
//! header's merkle root; and confirmations are counted from the L2 header's
//! `prev_main_hash`, which is consensus data, rather than from any node's live
//! view of the parent chain.
//!
//! The payment must also commit to the swap and to the L2 address that gets
//! the escrow, through an `OP_RETURN` output (see [`payment_commitment`]).
//! That is what ties a payment to one swap and what makes the claimant the
//! party who paid: nobody can present a proof of a payment committing to their
//! address without having made that payment.

use std::str::FromStr as _;

use bitcoin::{
    consensus::Decodable as _,
    script::{Instruction, PushBytes},
};
use borsh::{BorshDeserialize, BorshSerialize};
use thiserror::Error;

use crate::types::{Address, Swap, SwapId, proto::mainchain::BlockHeaderInfo};

/// Magic prefix of the `OP_RETURN` commitment.
pub const COMMITMENT_MAGIC: &[u8; 4] = b"CSFT";

/// Length of the `OP_RETURN` payload: magic, swap id, L2 claimer address.
pub const COMMITMENT_LEN: usize = 4 + 32 + 20;

/// Current proof format version.
pub const PROOF_VERSION: u8 = 0;

/// Serialized in `TxData::SwapClaim::proof_data` with borsh.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct L1PaymentProof {
    pub version: u8,
    /// Consensus-encoded `bitcoin::MerkleBlock`: the parent-chain block header
    /// plus a partial merkle tree proving the payment's inclusion. Exactly
    /// what `gettxoutproof [txid]` returns.
    pub merkle_block: Vec<u8>,
    /// Consensus-encoded payment transaction, as `getrawtransaction txid`
    /// returns.
    pub raw_tx: Vec<u8>,
}

impl L1PaymentProof {
    pub fn new(merkle_block: Vec<u8>, raw_tx: Vec<u8>) -> Self {
        Self {
            version: PROOF_VERSION,
            merkle_block,
            raw_tx,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        borsh::to_vec(self).expect("L1PaymentProof always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        borsh::from_slice(bytes).map_err(|err| Error::Encoding(err.to_string()))
    }
}

/// What a verified proof establishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedL1Payment {
    pub txid: bitcoin::Txid,
    pub block_hash: bitcoin::BlockHash,
    pub block_height: u32,
    /// Depth of the payment's block below the anchor, counting the payment's
    /// own block as one confirmation.
    pub confirmations: u32,
    /// The L2 address the payment committed to.
    pub claimer: Address,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("malformed L1 payment proof: {0}")]
    Encoding(String),
    #[error("unsupported L1 payment proof version {0}")]
    Version(u8),
    #[error(
        "parent chain {0:?} is not the chain this sidechain is anchored to; \
         payments on it cannot be proven"
    )]
    UnprovableChain(crate::types::ParentChainType),
    #[error("anchor block {0} is unknown")]
    UnknownAnchor(bitcoin::BlockHash),
    #[error(
        "L1 block {0} is unknown; it is not a block this node has validated"
    )]
    UnknownBlock(bitcoin::BlockHash),
    #[error(
        "L1 block {block} at height {height} is above the anchor at height \
         {anchor_height}"
    )]
    AboveAnchor {
        block: bitcoin::BlockHash,
        height: u32,
        anchor_height: u32,
    },
    #[error(
        "L1 block {block} at height {height} is not an ancestor of anchor \
         {anchor}"
    )]
    NotOnAnchorChain {
        block: bitcoin::BlockHash,
        height: u32,
        anchor: bitcoin::BlockHash,
    },
    #[error(
        "L1 payment is {confirmations} blocks deep but the swap requires \
         {required}"
    )]
    NotEnoughConfirmations { confirmations: u32, required: u32 },
    #[error(
        "L1 payment is {confirmations} blocks deep, older than the {max} the \
         swap's parent chain allows"
    )]
    TooOld { confirmations: u32, max: u32 },
    #[error("merkle proof does not include the payment transaction {0}")]
    TxNotProven(bitcoin::Txid),
    #[error("merkle proof is invalid: {0}")]
    Merkle(String),
    #[error(
        "swap's L1 recipient address {0} is not valid for its parent chain"
    )]
    RecipientAddress(String),
    #[error(
        "L1 payment pays {paid} to the swap's recipient but the swap requires \
         {required}"
    )]
    Underpaid {
        paid: bitcoin::Amount,
        required: bitcoin::Amount,
    },
    #[error("L1 payment carries no OP_RETURN commitment to this swap")]
    NoCommitment,
    #[error("L1 payment commits to swap {0}, not this one")]
    WrongSwap(SwapId),
    #[error(
        "L1 payment commits to claimer {committed} but the swap's recipient is \
         fixed to {recipient}"
    )]
    ClaimerMismatch {
        committed: Address,
        recipient: Address,
    },
}

/// Read-only view of the mainchain headers the node has validated.
pub trait MainchainView {
    fn main_header_info(
        &self,
        block_hash: &bitcoin::BlockHash,
    ) -> Result<Option<BlockHeaderInfo>, crate::state::Error>;
}

/// The `OP_RETURN` payload a filler must include in the L1 payment: magic,
/// swap id, and the L2 address that will receive the escrow.
pub fn payment_commitment(swap_id: &SwapId, claimer: &Address) -> Vec<u8> {
    let mut data = Vec::with_capacity(COMMITMENT_LEN);
    data.extend_from_slice(COMMITMENT_MAGIC);
    data.extend_from_slice(&swap_id.0);
    data.extend_from_slice(&claimer.0);
    data
}

/// The `scriptPubKey` of the commitment output.
pub fn commitment_script_for(
    swap_id: &SwapId,
    claimer: &Address,
) -> bitcoin::ScriptBuf {
    let data = payment_commitment(swap_id, claimer);
    let push: &PushBytes = data
        .as_slice()
        .try_into()
        .expect("commitment is far below the push size limit");
    bitcoin::ScriptBuf::new_op_return(push)
}

/// Parse a commitment out of an output script, if it is one.
pub fn parse_commitment(script: &bitcoin::Script) -> Option<(SwapId, Address)> {
    if !script.is_op_return() {
        return None;
    }
    let mut instructions = script.instructions();
    match instructions.next() {
        Some(Ok(Instruction::Op(bitcoin::opcodes::all::OP_RETURN))) => {}
        _ => return None,
    }
    let payload = match instructions.next() {
        Some(Ok(Instruction::PushBytes(bytes))) => bytes.as_bytes(),
        _ => return None,
    };
    if payload.len() != COMMITMENT_LEN || &payload[..4] != COMMITMENT_MAGIC {
        return None;
    }
    let mut swap_id = [0u8; 32];
    swap_id.copy_from_slice(&payload[4..36]);
    let mut claimer = [0u8; 20];
    claimer.copy_from_slice(&payload[36..56]);
    Some((SwapId(swap_id), Address(claimer)))
}

/// The commitment in `tx`, if any output carries one.
pub fn find_commitment(tx: &bitcoin::Transaction) -> Option<(SwapId, Address)> {
    tx.output
        .iter()
        .find_map(|out| parse_commitment(&out.script_pubkey))
}

/// The payment txid and committed claimer carried by a proof, without
/// verifying it against any headers. For rebuilding records from blocks that
/// consensus has already accepted.
pub fn payment_summary(
    proof_bytes: &[u8],
) -> Result<(bitcoin::Txid, Address), Error> {
    let proof = L1PaymentProof::from_bytes(proof_bytes)?;
    let tx =
        bitcoin::Transaction::consensus_decode(&mut proof.raw_tx.as_slice())
            .map_err(|err| Error::Encoding(format!("transaction: {err}")))?;
    let (_, claimer) = find_commitment(&tx).ok_or(Error::NoCommitment)?;
    Ok((tx.compute_txid(), claimer))
}

/// Verify `proof` for `swap` against the mainchain headers in `view`, with
/// `anchor` (the L2 header's `prev_main_hash`) as the tip confirmations are
/// counted from.
///
/// Every check is a pure function of the proof, the swap record and headers
/// that are consensus state, so any two nodes with the same chain agree.
pub fn verify(
    proof_bytes: &[u8],
    swap: &Swap,
    anchor: bitcoin::BlockHash,
    view: &dyn MainchainView,
) -> Result<VerifiedL1Payment, crate::state::Error> {
    if !swap.parent_chain.supports_payment_proofs() {
        return Err(Error::UnprovableChain(swap.parent_chain).into());
    }
    let proof = L1PaymentProof::from_bytes(proof_bytes)?;
    if proof.version != PROOF_VERSION {
        return Err(Error::Version(proof.version).into());
    }
    let merkle_block = bitcoin::MerkleBlock::consensus_decode(
        &mut proof.merkle_block.as_slice(),
    )
    .map_err(|err| Error::Encoding(format!("merkle block: {err}")))?;
    let tx =
        bitcoin::Transaction::consensus_decode(&mut proof.raw_tx.as_slice())
            .map_err(|err| Error::Encoding(format!("transaction: {err}")))?;
    let txid = tx.compute_txid();

    // 1. The header is one this node validated, and it sits on the anchor's
    //    ancestry no deeper than the parent chain's acceptance window.
    let block_hash = merkle_block.header.block_hash();
    let anchor_info = view
        .main_header_info(&anchor)?
        .ok_or(Error::UnknownAnchor(anchor))?;
    let block_info = view
        .main_header_info(&block_hash)?
        .ok_or(Error::UnknownBlock(block_hash))?;
    if block_info.height > anchor_info.height {
        return Err(Error::AboveAnchor {
            block: block_hash,
            height: block_info.height,
            anchor_height: anchor_info.height,
        }
        .into());
    }
    let confirmations = anchor_info.height - block_info.height + 1;
    let max_age = swap.parent_chain.max_l1_tx_age_blocks();
    if confirmations > max_age {
        return Err(Error::TooOld {
            confirmations,
            max: max_age,
        }
        .into());
    }
    // Walk back from the anchor to the block's height; the walk is bounded by
    // the age check above.
    let mut cursor = anchor_info;
    while cursor.height > block_info.height {
        cursor = view
            .main_header_info(&cursor.prev_block_hash)?
            .ok_or(Error::UnknownBlock(cursor.prev_block_hash))?;
    }
    if cursor.block_hash != block_hash {
        return Err(Error::NotOnAnchorChain {
            block: block_hash,
            height: block_info.height,
            anchor,
        }
        .into());
    }
    if confirmations < swap.required_confirmations.max(1) {
        return Err(Error::NotEnoughConfirmations {
            confirmations,
            required: swap.required_confirmations.max(1),
        }
        .into());
    }

    // 2. The transaction is in that block. `extract_matches` recomputes the
    //    root from the partial tree and rejects malformed trees, including the
    //    duplicate-hash shape of CVE-2012-2459.
    let mut matches = Vec::new();
    let mut indexes = Vec::new();
    merkle_block
        .extract_matches(&mut matches, &mut indexes)
        .map_err(|err| Error::Merkle(format!("{err:?}")))?;
    if !matches.contains(&txid) {
        return Err(Error::TxNotProven(txid).into());
    }

    // 3. It pays the swap's L1 recipient at least the L1 amount.
    let network = swap.parent_chain.to_bitcoin_network();
    let recipient_script =
        bitcoin::Address::from_str(&swap.l1_recipient_address)
            .ok()
            .and_then(|addr| addr.require_network(network).ok())
            .map(|addr| addr.script_pubkey())
            .ok_or_else(|| {
                Error::RecipientAddress(swap.l1_recipient_address.clone())
            })?;
    let paid = tx
        .output
        .iter()
        .filter(|out| out.script_pubkey == recipient_script)
        .map(|out| out.value)
        .try_fold(bitcoin::Amount::ZERO, |acc, v| acc.checked_add(v))
        .unwrap_or(bitcoin::Amount::MAX_MONEY);
    if paid < swap.l1_amount {
        return Err(Error::Underpaid {
            paid,
            required: swap.l1_amount,
        }
        .into());
    }

    // 4. It commits to this swap and names the claimer.
    let (committed_swap, claimer) =
        find_commitment(&tx).ok_or(Error::NoCommitment)?;
    if committed_swap != swap.id {
        return Err(Error::WrongSwap(committed_swap).into());
    }
    if let Some(recipient) = swap.l2_recipient
        && recipient != claimer
    {
        return Err(Error::ClaimerMismatch {
            committed: claimer,
            recipient,
        }
        .into());
    }

    Ok(VerifiedL1Payment {
        txid,
        block_hash,
        block_height: block_info.height,
        confirmations,
        claimer,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Builders for synthetic parent-chain blocks, shared with the swap and
    //! block validation tests.

    use bitcoin::{
        absolute::LockTime, block::Version, consensus::Encodable as _,
        hashes::Hash as _, transaction::Version as TxVersion,
    };

    use super::*;

    /// A regtest-shaped block containing a coinbase and `payment`.
    pub fn block_with(
        prev: bitcoin::BlockHash,
        payment: bitcoin::Transaction,
    ) -> bitcoin::Block {
        let coinbase = bitcoin::Transaction {
            version: TxVersion::TWO,
            lock_time: LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint::null(),
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(50_000_000),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        };
        let txdata = vec![coinbase, payment];
        let merkle_root = bitcoin::merkle_tree::calculate_root(
            txdata.iter().map(bitcoin::Transaction::compute_txid),
        )
        .map(|root| bitcoin::TxMerkleNode::from_raw_hash(root.to_raw_hash()))
        .unwrap();
        bitcoin::Block {
            header: bitcoin::block::Header {
                version: Version::TWO,
                prev_blockhash: prev,
                merkle_root,
                time: 0,
                bits: bitcoin::CompactTarget::from_consensus(0x207fffff),
                nonce: 0,
            },
            txdata,
        }
    }

    /// A payment of `amount` to `recipient`, committing to `swap_id` and
    /// `claimer`, with `extra` further outputs.
    pub fn payment(
        recipient: &bitcoin::Address,
        amount: bitcoin::Amount,
        commitment: Option<bitcoin::ScriptBuf>,
    ) -> bitcoin::Transaction {
        let mut output = vec![bitcoin::TxOut {
            value: amount,
            script_pubkey: recipient.script_pubkey(),
        }];
        if let Some(script_pubkey) = commitment {
            output.push(bitcoin::TxOut {
                value: bitcoin::Amount::ZERO,
                script_pubkey,
            });
        }
        bitcoin::Transaction {
            version: TxVersion::TWO,
            lock_time: LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint {
                    txid: bitcoin::Txid::from_byte_array([9u8; 32]),
                    vout: 0,
                },
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: bitcoin::Witness::new(),
            }],
            output,
        }
    }

    /// Encode a proof for `payment` inside `block`.
    pub fn proof_for(
        block: &bitcoin::Block,
        payment: &bitcoin::Transaction,
    ) -> Vec<u8> {
        let txid = payment.compute_txid();
        let merkle_block =
            bitcoin::MerkleBlock::from_block_with_predicate(block, |t| {
                *t == txid
            });
        let mut merkle_bytes = Vec::new();
        merkle_block.consensus_encode(&mut merkle_bytes).unwrap();
        let mut tx_bytes = Vec::new();
        payment.consensus_encode(&mut tx_bytes).unwrap();
        L1PaymentProof::new(merkle_bytes, tx_bytes).to_bytes()
    }

    pub fn header_info(block: &bitcoin::Block, height: u32) -> BlockHeaderInfo {
        BlockHeaderInfo {
            block_hash: block.block_hash(),
            prev_block_hash: block.header.prev_blockhash,
            height,
            work: bitcoin::Work::from_be_bytes([0u8; 32]),
        }
    }

    /// A chain of `n` synthetic blocks on top of `base`, each containing one
    /// dummy payment so their hashes differ.
    pub fn chain(
        base: bitcoin::BlockHash,
        base_height: u32,
        n: u32,
    ) -> Vec<(bitcoin::Block, BlockHeaderInfo)> {
        let filler = bitcoin::Address::p2wpkh(
            &bitcoin::CompressedPublicKey::from_slice(&[
                2, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0,
                0x62, 0x95, 0xce, 0x87, 0x0b, 0x07, 0x02, 0x9b, 0xfc, 0xdb,
                0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8,
                0x17, 0x98,
            ])
            .unwrap(),
            bitcoin::Network::Regtest,
        );
        let mut out = Vec::new();
        let mut prev = base;
        for i in 0..n {
            let mut tx = payment(&filler, bitcoin::Amount::from_sat(1), None);
            tx.lock_time = LockTime::from_consensus(i);
            let block = block_with(prev, tx);
            prev = block.block_hash();
            let height = base_height + i + 1;
            let info = header_info(&block, height);
            out.push((block, info));
        }
        out
    }

    pub struct MapView(
        pub std::collections::HashMap<bitcoin::BlockHash, BlockHeaderInfo>,
    );

    impl MainchainView for MapView {
        fn main_header_info(
            &self,
            block_hash: &bitcoin::BlockHash,
        ) -> Result<Option<BlockHeaderInfo>, crate::state::Error> {
            Ok(self.0.get(block_hash).copied())
        }
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::hashes::Hash as _;

    use super::{test_support::*, *};
    use crate::types::{ParentChainType, SwapDirection, SwapTxId};

    const L1_RECIPIENT: &str = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080";

    fn swap(
        l2_recipient: Option<Address>,
        required_confirmations: u32,
    ) -> Swap {
        Swap::new(
            SwapId([11u8; 32]),
            SwapDirection::L2ToL1,
            ParentChainType::Regtest,
            SwapTxId::Hash32([0u8; 32]),
            Some(required_confirmations),
            l2_recipient,
            bitcoin::Amount::from_sat(50_000),
            L1_RECIPIENT.to_owned(),
            bitcoin::Amount::from_sat(40_000),
            0,
            None,
            Some(Address([5u8; 20])),
        )
    }

    fn recipient() -> bitcoin::Address {
        bitcoin::Address::from_str(L1_RECIPIENT)
            .unwrap()
            .require_network(bitcoin::Network::Regtest)
            .unwrap()
    }

    /// Chain: genesis-ish base (height 100) -> payment block (101) -> two more
    /// (102, 103). Returns the view, the payment block, and the tip hash.
    fn scenario(
        payment: bitcoin::Transaction,
    ) -> (MapView, bitcoin::Block, Vec<bitcoin::BlockHash>) {
        let base = bitcoin::BlockHash::from_byte_array([1u8; 32]);
        let mut view = std::collections::HashMap::new();
        view.insert(
            base,
            BlockHeaderInfo {
                block_hash: base,
                prev_block_hash: bitcoin::BlockHash::all_zeros(),
                height: 100,
                work: bitcoin::Work::from_be_bytes([0u8; 32]),
            },
        );
        let payment_block = block_with(base, payment);
        view.insert(
            payment_block.block_hash(),
            header_info(&payment_block, 101),
        );
        let mut hashes = vec![payment_block.block_hash()];
        for (block, info) in chain(payment_block.block_hash(), 101, 2) {
            hashes.push(block.block_hash());
            view.insert(block.block_hash(), info);
        }
        (MapView(view), payment_block, hashes)
    }

    #[test]
    fn commitment_round_trips() {
        let swap_id = SwapId([3u8; 32]);
        let claimer = Address([4u8; 20]);
        let script = commitment_script_for(&swap_id, &claimer);
        assert_eq!(parse_commitment(&script), Some((swap_id, claimer)));
        assert_eq!(
            payment_commitment(&swap_id, &claimer).len(),
            COMMITMENT_LEN
        );
        assert!(parse_commitment(&recipient().script_pubkey()).is_none());
    }

    #[test]
    fn valid_proof_verifies_with_confirmations_from_anchor() {
        let swap = swap(None, 3);
        let claimer = Address([4u8; 20]);
        let payment = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &claimer)),
        );
        let (view, block, hashes) = scenario(payment.clone());
        let proof = proof_for(&block, &payment);

        // Anchored at the payment block itself: one confirmation, not enough.
        let err = verify(&proof, &swap, hashes[0], &view).unwrap_err();
        assert!(
            err.to_string().contains("1 blocks deep"),
            "expected NotEnoughConfirmations, got {err}"
        );
        // Anchored two blocks later: three confirmations.
        let verified = verify(&proof, &swap, hashes[2], &view).unwrap();
        assert_eq!(verified.claimer, claimer);
        assert_eq!(verified.confirmations, 3);
        assert_eq!(verified.block_height, 101);
        assert_eq!(verified.txid, payment.compute_txid());
    }

    #[test]
    fn rejects_payment_in_unknown_block() {
        let swap = swap(None, 1);
        let payment = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &Address([4u8; 20]))),
        );
        let (view, _block, hashes) = scenario(payment.clone());
        // A block the node never saw: same payment, different parent.
        let foreign = block_with(
            bitcoin::BlockHash::from_byte_array([7u8; 32]),
            payment.clone(),
        );
        let proof = proof_for(&foreign, &payment);
        let err = verify(&proof, &swap, hashes[2], &view).unwrap_err();
        assert!(err.to_string().contains("is unknown"), "{err}");
    }

    #[test]
    fn rejects_block_off_the_anchor_chain() {
        let swap = swap(None, 1);
        let payment = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &Address([4u8; 20]))),
        );
        let (mut view, block, hashes) = scenario(payment.clone());
        // A sibling of the payment block at the same height, known to the
        // node but not on the anchor's ancestry.
        let base = block.header.prev_blockhash;
        let sibling = block_with(
            base,
            super::test_support::payment(
                &recipient(),
                bitcoin::Amount::ONE_SAT,
                None,
            ),
        );
        view.0
            .insert(sibling.block_hash(), header_info(&sibling, 101));
        let sibling_payment = &sibling.txdata[1];
        let mut sibling_swap = swap.clone();
        sibling_swap.l1_amount = bitcoin::Amount::ONE_SAT;
        let proof = proof_for(&sibling, sibling_payment);
        let err = verify(&proof, &sibling_swap, hashes[2], &view).unwrap_err();
        assert!(err.to_string().contains("not an ancestor"), "{err}");
    }

    #[test]
    fn rejects_tampered_transaction() {
        let swap = swap(None, 1);
        let honest = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &Address([4u8; 20]))),
        );
        let (view, block, hashes) = scenario(honest.clone());
        // Proof for the honest tx, but the attacker swaps in a tx that names
        // their own address: its txid is not in the tree.
        let forged = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &Address([6u8; 20]))),
        );
        let mut proof =
            L1PaymentProof::from_bytes(&proof_for(&block, &honest)).unwrap();
        proof.raw_tx = bitcoin::consensus::serialize(&forged);
        let err =
            verify(&proof.to_bytes(), &swap, hashes[2], &view).unwrap_err();
        assert!(err.to_string().contains("does not include"), "{err}");
    }

    #[test]
    fn rejects_underpayment_missing_and_wrong_commitment() {
        let claimer = Address([4u8; 20]);
        let swap = swap(None, 1);
        for (payment, expect) in [
            (
                payment(
                    &recipient(),
                    swap.l1_amount - bitcoin::Amount::ONE_SAT,
                    Some(commitment_script_for(&swap.id, &claimer)),
                ),
                "pays",
            ),
            (payment(&recipient(), swap.l1_amount, None), "no OP_RETURN"),
            (
                payment(
                    &recipient(),
                    swap.l1_amount,
                    Some(commitment_script_for(&SwapId([12u8; 32]), &claimer)),
                ),
                "commits to swap",
            ),
        ] {
            let (view, block, hashes) = scenario(payment.clone());
            let proof = proof_for(&block, &payment);
            let err = verify(&proof, &swap, hashes[2], &view).unwrap_err();
            assert!(
                err.to_string().contains(expect),
                "expected {expect}, got {err}"
            );
        }
    }

    #[test]
    fn pre_specified_swap_requires_matching_claimer() {
        let recipient_l2 = Address([8u8; 20]);
        let swap = swap(Some(recipient_l2), 1);
        let wrong = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &Address([4u8; 20]))),
        );
        let (view, block, hashes) = scenario(wrong.clone());
        let err = verify(&proof_for(&block, &wrong), &swap, hashes[2], &view)
            .unwrap_err();
        assert!(err.to_string().contains("fixed to"), "{err}");

        let right = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &recipient_l2)),
        );
        let (view, block, hashes) = scenario(right.clone());
        let verified =
            verify(&proof_for(&block, &right), &swap, hashes[2], &view)
                .unwrap();
        assert_eq!(verified.claimer, recipient_l2);
    }

    #[test]
    fn rejects_unprovable_chain_and_bad_encoding() {
        let mut swap = swap(None, 1);
        let payment = payment(
            &recipient(),
            swap.l1_amount,
            Some(commitment_script_for(&swap.id, &Address([4u8; 20]))),
        );
        let (view, block, hashes) = scenario(payment.clone());
        let proof = proof_for(&block, &payment);
        swap.parent_chain = ParentChainType::BCH;
        let err = verify(&proof, &swap, hashes[2], &view).unwrap_err();
        assert!(err.to_string().contains("cannot be proven"), "{err}");
        swap.parent_chain = ParentChainType::Regtest;
        assert!(verify(b"garbage", &swap, hashes[2], &view).is_err());
    }
}
