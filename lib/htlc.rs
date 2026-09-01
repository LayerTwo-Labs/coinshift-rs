//! The Bitcoin leg of an atomic swap: a P2WSH hash-timelocked contract.
//!
//! Nothing here is consensus code. Coinshift never validates a Bitcoin script,
//! never sees a Bitcoin block, and never needs to — the two chains are joined
//! only by a shared secret. This module builds the Bitcoin side so that a
//! wallet can lock, claim and refund it, and so that the hash it commits to is
//! the same one [`crate::state::hash_lock`] will check on ours.
//!
//! # The script
//!
//! ```text
//! OP_IF
//!     OP_SHA256 <hash> OP_EQUALVERIFY <claim_pubkey>
//! OP_ELSE
//!     <timeout> OP_CHECKLOCKTIMEVERIFY OP_DROP <refund_pubkey>
//! OP_ENDIF
//! OP_CHECKSIG
//! ```
//!
//! The claimant spends the `OP_IF` branch by supplying the preimage; the funder
//! spends the `OP_ELSE` branch once the locktime has passed. Both are ordinary
//! Bitcoin script — no soft-fork dependency beyond CLTV, which has been
//! available since 2015.
//!
//! # Why SHA-256 and not blake3
//!
//! `OP_SHA256` is what Bitcoin gives us, so the Coinshift side must commit to
//! the same digest. This codebase reaches for blake3 elsewhere — `SwapId` is a
//! blake3 hash — and using it here would produce two locks that no single
//! secret opens. Both legs would then be refundable and neither claimable: a
//! swap that looks correct right up until it silently does nothing.
//!
//! # Why the deadlines are the security property
//!
//! See [`SwapDeadlines`]. Consensus on either chain sees one leg and cannot
//! check the relationship between the two, so getting this ordering right is
//! the wallet's job and nobody else's.

use bitcoin::{
    Address, Network, ScriptBuf, blockdata::opcodes::all as opcodes,
    hashes::Hash as _, script::Builder,
};

/// A swap secret: 32 bytes whose SHA-256 digest is published as the lock.
///
/// Deliberately not `Copy` and not `Debug`-printable in full. Leaking the
/// secret before the Bitcoin leg is claimed lets anyone take that leg.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; 32]);

impl Secret {
    /// Draw a fresh secret from the OS entropy source.
    pub fn random() -> Self {
        use rand::RngCore as _;
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The commitment published in both legs.
    ///
    /// SHA-256, because the Bitcoin leg is gated on `OP_SHA256`. See the module
    /// docs for what using anything else would cost.
    pub fn hash(&self) -> [u8; 32] {
        bitcoin::hashes::sha256::Hash::hash(&self.0).to_byte_array()
    }
}

impl std::fmt::Debug for Secret {
    /// Redacted on purpose: a secret in a log is a secret on the wire.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Everything the Bitcoin leg's script commits to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtlcParams {
    /// SHA-256 of the swap secret. The same value goes in the Coinshift lock.
    pub hash: [u8; 32],
    /// Spends the contract by revealing the preimage.
    pub claim_pubkey: bitcoin::PublicKey,
    /// Reclaims the contract after `timeout_height`.
    pub refund_pubkey: bitcoin::PublicKey,
    /// Bitcoin block height at which the refund branch becomes spendable.
    pub timeout_height: u32,
}

impl HtlcParams {
    /// The witness script both spend paths are checked against.
    pub fn witness_script(&self) -> ScriptBuf {
        Builder::new()
            .push_opcode(opcodes::OP_IF)
            .push_opcode(opcodes::OP_SHA256)
            .push_slice(self.hash)
            .push_opcode(opcodes::OP_EQUALVERIFY)
            .push_key(&self.claim_pubkey)
            .push_opcode(opcodes::OP_ELSE)
            .push_int(i64::from(self.timeout_height))
            .push_opcode(opcodes::OP_CLTV)
            .push_opcode(opcodes::OP_DROP)
            .push_key(&self.refund_pubkey)
            .push_opcode(opcodes::OP_ENDIF)
            .push_opcode(opcodes::OP_CHECKSIG)
            .into_script()
    }

    /// The P2WSH address the taker funds.
    pub fn address(&self, network: Network) -> Address {
        Address::p2wsh(self.witness_script().as_script(), network)
    }

    /// Witness stack for the claim branch: signature, preimage, TRUE, script.
    ///
    /// The trailing `1` selects `OP_IF`; the preimage satisfies `OP_SHA256`.
    pub fn claim_witness(
        &self,
        signature: &[u8],
        secret: &Secret,
    ) -> bitcoin::Witness {
        let mut witness = bitcoin::Witness::new();
        witness.push(signature);
        witness.push(secret.as_bytes());
        witness.push([1u8]);
        witness.push(self.witness_script().as_bytes());
        witness
    }

    /// Witness stack for the refund branch: signature, FALSE, script.
    ///
    /// The empty item selects `OP_ELSE`. The spending transaction must also set
    /// `lock_time` at or past `timeout_height` and a non-final sequence, or
    /// `OP_CHECKLOCKTIMEVERIFY` fails regardless of this stack.
    pub fn refund_witness(&self, signature: &[u8]) -> bitcoin::Witness {
        let mut witness = bitcoin::Witness::new();
        witness.push(signature);
        witness.push([] as [u8; 0]);
        witness.push(self.witness_script().as_bytes());
        witness
    }
}

/// Which output is being spent, and where the value goes.
///
/// Grouped rather than passed loose because the alternative is eight
/// positional arguments, four of which are amounts and outpoints that would
/// transpose silently.
#[derive(Clone, Copy, Debug)]
pub struct SpendRequest<'a> {
    /// The funding output of the contract.
    pub outpoint: bitcoin::OutPoint,
    /// Its value. Needed for the segwit sighash, which commits to it.
    pub value: bitcoin::Amount,
    /// Where the balance goes.
    pub to: &'a bitcoin::Address,
    /// Deducted from `value`.
    pub fee: bitcoin::Amount,
}

/// How a spend of the Bitcoin leg is being made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpendPath {
    /// Reveal the preimage. Spendable immediately.
    Claim,
    /// Wait out the timeout. Requires the transaction to say so — see
    /// [`HtlcParams::spend_transaction`].
    Refund,
}

#[derive(Debug, thiserror::Error)]
pub enum SpendError {
    #[error("failed to compute the p2wsh sighash: {0}")]
    Sighash(String),
}

impl HtlcParams {
    /// Build and sign a spend of this contract.
    ///
    /// `request` names the funding output and where the balance goes.
    ///
    /// The refund path is the fiddly one, and getting it wrong looks like a
    /// mysteriously rejected transaction rather than an error. `OP_CLTV`
    /// compares against the spending transaction's `lock_time`, and it refuses
    /// to run at all if the input's sequence is final — so a refund needs
    /// **both** `lock_time >= timeout_height` and a non-final sequence. Setting
    /// one without the other fails. This sets both, and only for the refund
    /// path, because a claim wants neither.
    pub fn spend_transaction(
        &self,
        path: SpendPath,
        request: SpendRequest<'_>,
        key: &bitcoin::secp256k1::SecretKey,
        secret: Option<&Secret>,
    ) -> Result<bitcoin::Transaction, SpendError> {
        let SpendRequest {
            outpoint,
            value,
            to,
            fee,
        } = request;
        use bitcoin::{
            EcdsaSighashType, Sequence, TxIn, TxOut, absolute::LockTime,
            sighash::SighashCache, transaction::Version,
        };

        let (lock_time, sequence) = match path {
            SpendPath::Claim => (LockTime::ZERO, Sequence::MAX),
            SpendPath::Refund => (
                LockTime::from_height(self.timeout_height)
                    .unwrap_or(LockTime::ZERO),
                // Non-final, or OP_CHECKLOCKTIMEVERIFY refuses to run.
                Sequence::ENABLE_LOCKTIME_NO_RBF,
            ),
        };

        let mut tx = bitcoin::Transaction {
            version: Version::TWO,
            lock_time,
            input: vec![TxIn {
                previous_output: outpoint,
                script_sig: bitcoin::ScriptBuf::new(),
                sequence,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![TxOut {
                value: value - fee,
                script_pubkey: to.script_pubkey(),
            }],
        };

        let script = self.witness_script();
        let sighash = SighashCache::new(&tx)
            .p2wsh_signature_hash(0, &script, value, EcdsaSighashType::All)
            .map_err(|err| SpendError::Sighash(err.to_string()))?;

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let message =
            bitcoin::secp256k1::Message::from_digest(sighash.to_byte_array());
        let signature = bitcoin::ecdsa::Signature {
            signature: secp.sign_ecdsa(&message, key),
            sighash_type: EcdsaSighashType::All,
        };

        tx.input[0].witness = match (path, secret) {
            (SpendPath::Claim, Some(secret)) => {
                self.claim_witness(&signature.serialize(), secret)
            }
            (SpendPath::Claim, None) => {
                // A claim without the secret cannot be built; produce the
                // refund shape rather than a witness that silently fails.
                self.refund_witness(&signature.serialize())
            }
            (SpendPath::Refund, _) => {
                self.refund_witness(&signature.serialize())
            }
        };
        Ok(tx)
    }
}

/// The Coinshift leg's terms — the mirror of [`HtlcParams`].
///
/// The two are kept next to each other deliberately: a swap is correct exactly
/// when they agree on `commitment` and their deadlines are ordered so the party
/// holding the secret expires last. Seeing them together is the cheapest way to
/// notice when they do not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HashLockTerms {
    /// SHA-256 of the swap secret. Must equal [`HtlcParams::hash`].
    pub commitment: [u8; 32],
    /// The taker, who can only spend this by revealing the preimage.
    pub claimant: crate::types::Address,
    /// Where the escrow goes if it times out. Becomes the output's own address,
    /// so it is also the key that authorises the refund.
    pub refund_to: crate::types::Address,
    /// Coinshift height at which the refund becomes spendable. Must be the
    /// *later* of the two deadlines — see [`SwapDeadlines`].
    pub timeout_height: u32,
}

/// Expected wall-clock seconds per Bitcoin block.
pub const BITCOIN_BLOCK_SECS: u32 = 600;

/// Expected wall-clock seconds per Coinshift block.
///
/// Coinshift is merge-mined against eCash, which targets the same ten minutes
/// Bitcoin does, so in expectation the two chains advance together. In practice
/// they will not, which is what [`REVEAL_SAFETY_BLOCKS`] is for.
pub const COINSHIFT_BLOCK_SECS: u32 = 600;

/// Slack, in Coinshift blocks, between the two deadlines.
///
/// # This is a security parameter, not a comfort margin
///
/// After the maker reveals the secret by taking the Bitcoin leg, the taker has
/// to notice and get their own claim mined. If they do not manage it before
/// the Coinshift escrow expires, the maker refunds that escrow **and keeps the
/// Bitcoin** — a complete loss for the taker, caused by inattention rather
/// than by anyone breaking a rule.
///
/// So this number is how long a taker may look away. At twelve blocks it was
/// two hours, which is fine for a daemon and far too short for a person. 144
/// blocks is roughly a day, which a human can survive and an automated watcher
/// does not mind.
///
/// The cost is locked capital: both sides wait longer for a refund when a swap
/// is abandoned. That is the trade, and it is the kind of number that should be
/// set from how this is actually used rather than left at whatever the first
/// draft happened to pick.
///
/// **A taker still needs to watch.** No margin removes that requirement; this
/// only decides how demanding it is.
pub const REVEAL_SAFETY_BLOCKS: u32 = 144;

/// The two deadlines of a swap, in a shape that cannot hold an unsafe pair.
///
/// # The property
///
/// **The party who knows the secret must hold the later deadline.** The maker
/// picks the secret, so the maker's Coinshift lock has to outlive the taker's
/// Bitcoin one. Then the sequence is forced: the maker must reveal to get paid,
/// and once revealed the taker still has time to follow.
///
/// Invert it and the maker steals outright — take the Bitcoin, wait for the
/// taker's Coinshift window to close, reclaim the escrow, keep both. Nothing on
/// either chain notices, because neither chain can see the other leg. It is a
/// total loss produced by two constants in the wrong order, which is exactly
/// the kind of mistake that belongs behind a constructor rather than in a
/// comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapDeadlines {
    /// Bitcoin height at which the taker may reclaim their Bitcoin.
    pub bitcoin_timeout: u32,
    /// Coinshift height at which the maker may reclaim their escrow.
    pub coinshift_timeout: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum DeadlineError {
    #[error(
        "the Bitcoin refund at height {bitcoin_timeout} is not far enough \
         ahead of Bitcoin's tip ({bitcoin_height}) for a swap to complete"
    )]
    BitcoinTimeoutInPast {
        bitcoin_height: u32,
        bitcoin_timeout: u32,
    },
    #[error(
        "unsafe deadline ordering: the Coinshift escrow would expire at height \
         {coinshift_timeout}, but the taker needs until at least {minimum} to \
         act on a secret revealed at the Bitcoin deadline. The party holding \
         the secret must hold the later deadline"
    )]
    CoinshiftTimeoutTooEarly {
        coinshift_timeout: u32,
        minimum: u32,
    },
}

impl SwapDeadlines {
    /// The earliest Coinshift deadline that is safe for a given Bitcoin one.
    ///
    /// Converts the Bitcoin window into Coinshift blocks through the two block
    /// times, then adds [`REVEAL_SAFETY_BLOCKS`] of slack for the taker to see
    /// the reveal and get mined.
    pub fn minimum_coinshift_timeout(
        coinshift_height: u32,
        bitcoin_height: u32,
        bitcoin_timeout: u32,
    ) -> Option<u32> {
        let bitcoin_blocks_remaining =
            bitcoin_timeout.checked_sub(bitcoin_height)?;
        let equivalent = (u64::from(bitcoin_blocks_remaining)
            * u64::from(BITCOIN_BLOCK_SECS))
            / u64::from(COINSHIFT_BLOCK_SECS);
        let equivalent = u32::try_from(equivalent).ok()?;
        coinshift_height
            .checked_add(equivalent)?
            .checked_add(REVEAL_SAFETY_BLOCKS)
    }

    /// Build a pair, refusing any ordering that lets one side steal.
    pub fn new(
        coinshift_height: u32,
        bitcoin_height: u32,
        bitcoin_timeout: u32,
        coinshift_timeout: u32,
    ) -> Result<Self, DeadlineError> {
        let minimum = Self::minimum_coinshift_timeout(
            coinshift_height,
            bitcoin_height,
            bitcoin_timeout,
        )
        .ok_or(DeadlineError::BitcoinTimeoutInPast {
            bitcoin_height,
            bitcoin_timeout,
        })?;
        if coinshift_timeout < minimum {
            return Err(DeadlineError::CoinshiftTimeoutTooEarly {
                coinshift_timeout,
                minimum,
            });
        }
        Ok(Self {
            bitcoin_timeout,
            coinshift_timeout,
        })
    }

    /// The safe pair for a Bitcoin window of `bitcoin_blocks`, chosen for you.
    pub fn suggested(
        coinshift_height: u32,
        bitcoin_height: u32,
        bitcoin_blocks: u32,
    ) -> Result<Self, DeadlineError> {
        let bitcoin_timeout = bitcoin_height.saturating_add(bitcoin_blocks);
        let coinshift_timeout = Self::minimum_coinshift_timeout(
            coinshift_height,
            bitcoin_height,
            bitcoin_timeout,
        )
        .ok_or(DeadlineError::BitcoinTimeoutInPast {
            bitcoin_height,
            bitcoin_timeout,
        })?;
        Self::new(
            coinshift_height,
            bitcoin_height,
            bitcoin_timeout,
            coinshift_timeout,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pubkey(byte: u8) -> bitcoin::PublicKey {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let sk = bitcoin::secp256k1::SecretKey::from_slice(&[byte; 32])
            .expect("valid secret key");
        bitcoin::PublicKey::new(sk.public_key(&secp))
    }

    fn params() -> HtlcParams {
        HtlcParams {
            hash: Secret::from_bytes([7u8; 32]).hash(),
            claim_pubkey: pubkey(1),
            refund_pubkey: pubkey(2),
            timeout_height: 800_000,
        }
    }

    /// The commitment both legs share has to be SHA-256, because that is what
    /// `OP_SHA256` computes. blake3 is the hash this codebase reaches for
    /// everywhere else, and using it here would produce two locks that no
    /// single secret opens.
    #[test]
    fn the_commitment_is_sha256() {
        let secret = Secret::from_bytes([7u8; 32]);
        assert_eq!(
            secret.hash(),
            bitcoin::hashes::sha256::Hash::hash(&[7u8; 32]).to_byte_array()
        );
        assert_ne!(secret.hash(), *blake3::hash(&[7u8; 32]).as_bytes());
    }

    /// The script must contain both branches in the right order, and commit to
    /// the hash and the timeout.
    #[test]
    fn the_script_has_both_branches() {
        let p = params();
        let script = p.witness_script();
        let asm = script.to_asm_string();
        assert!(asm.contains("OP_IF"), "{asm}");
        assert!(asm.contains("OP_SHA256"), "{asm}");
        assert!(asm.contains("OP_EQUALVERIFY"), "{asm}");
        assert!(asm.contains("OP_ELSE"), "{asm}");
        assert!(asm.contains("OP_CLTV"), "{asm}");
        assert!(asm.contains("OP_DROP"), "{asm}");
        assert!(asm.contains("OP_ENDIF"), "{asm}");
        assert!(asm.contains("OP_CHECKSIG"), "{asm}");
        assert!(
            asm.contains(&hex::encode(p.hash)),
            "script must commit to the hash: {asm}"
        );
    }

    /// Same parameters, same address — and a different hash is a different
    /// contract, so a taker cannot be pointed at the wrong one.
    #[test]
    fn the_address_is_deterministic_and_hash_bound() {
        let p = params();
        assert_eq!(
            p.address(Network::Bitcoin),
            p.address(Network::Bitcoin),
            "derivation must be deterministic"
        );
        let mut other = p.clone();
        other.hash = Secret::from_bytes([8u8; 32]).hash();
        assert_ne!(
            p.address(Network::Bitcoin),
            other.address(Network::Bitcoin),
            "a different secret must produce a different address"
        );
        assert!(p.address(Network::Bitcoin).to_string().starts_with("bc1q"));
        assert!(
            p.address(Network::Regtest)
                .to_string()
                .starts_with("bcrt1q")
        );
    }

    /// Claim reveals the secret and selects OP_IF; refund does neither.
    #[test]
    fn the_two_witnesses_select_different_branches() {
        let p = params();
        let secret = Secret::from_bytes([7u8; 32]);
        let sig = [0xABu8; 71];

        let claim = p.claim_witness(&sig, &secret);
        assert_eq!(claim.len(), 4, "sig, preimage, TRUE, script");
        assert_eq!(claim.nth(1).unwrap(), secret.as_bytes());
        assert_eq!(claim.nth(2).unwrap(), &[1u8], "TRUE selects OP_IF");

        let refund = p.refund_witness(&sig);
        assert_eq!(refund.len(), 3, "sig, FALSE, script");
        assert!(
            refund.nth(1).unwrap().is_empty(),
            "empty item is FALSE, selecting OP_ELSE"
        );
        assert!(
            !refund.iter().any(|item| item == secret.as_bytes()),
            "the refund path must never carry the secret"
        );
    }

    /// The whole security property: the maker holds the secret, so the maker's
    /// deadline must be the later one.
    #[test]
    fn an_inverted_deadline_pair_is_refused() {
        // Bitcoin refunds in 100 blocks; the Coinshift escrow expires first.
        let err = SwapDeadlines::new(1_000, 800_000, 800_100, 1_050)
            .expect_err("this ordering lets the maker take both legs");
        assert!(matches!(
            err,
            DeadlineError::CoinshiftTimeoutTooEarly { .. }
        ));
    }

    /// The boundary: the suggested pair is the earliest accepted one, and one
    /// block less is refused.
    #[test]
    fn the_suggested_pair_is_exactly_the_minimum() {
        let suggested = SwapDeadlines::suggested(1_000, 800_000, 100)
            .expect("a 100-block Bitcoin window is workable");
        assert_eq!(suggested.bitcoin_timeout, 800_100);
        assert_eq!(
            suggested.coinshift_timeout,
            1_000 + 100 + REVEAL_SAFETY_BLOCKS
        );

        assert!(
            SwapDeadlines::new(
                1_000,
                800_000,
                800_100,
                suggested.coinshift_timeout
            )
            .is_ok(),
            "the minimum itself must be accepted"
        );
        assert!(
            SwapDeadlines::new(
                1_000,
                800_000,
                800_100,
                suggested.coinshift_timeout - 1
            )
            .is_err(),
            "one block below the minimum must be refused"
        );
    }

    /// A Bitcoin deadline already in the past cannot anchor anything.
    #[test]
    fn a_bitcoin_timeout_in_the_past_is_refused() {
        assert!(matches!(
            SwapDeadlines::new(1_000, 800_000, 799_999, 999_999),
            Err(DeadlineError::BitcoinTimeoutInPast { .. })
        ));
    }

    /// A secret must not be printable; logs and error reports leak.
    #[test]
    fn a_secret_does_not_print_itself() {
        let secret = Secret::from_bytes([7u8; 32]);
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "Secret(<redacted>)");
        assert!(!rendered.contains("07"));
    }

    fn keypair(
        byte: u8,
    ) -> (bitcoin::secp256k1::SecretKey, bitcoin::PublicKey) {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let sk = bitcoin::secp256k1::SecretKey::from_slice(&[byte; 32])
            .expect("valid secret key");
        (sk, bitcoin::PublicKey::new(sk.public_key(&secp)))
    }

    /// The refund path needs BOTH a lock_time at or past the deadline AND a
    /// non-final sequence. `OP_CHECKLOCKTIMEVERIFY` refuses to run at all if
    /// the input's sequence is final, so setting only the lock_time produces a
    /// transaction that fails for a reason nothing in it points at.
    #[test]
    fn a_refund_sets_both_the_locktime_and_a_non_final_sequence() {
        let (sk, pk) = keypair(1);
        let p = HtlcParams {
            hash: Secret::from_bytes([7u8; 32]).hash(),
            claim_pubkey: pk,
            refund_pubkey: pk,
            timeout_height: 800_100,
        };
        let to = p.address(Network::Regtest);
        let tx = p
            .spend_transaction(
                SpendPath::Refund,
                SpendRequest {
                    outpoint: bitcoin::OutPoint::null(),
                    value: bitcoin::Amount::from_sat(100_000),
                    to: &to,
                    fee: bitcoin::Amount::from_sat(1_000),
                },
                &sk,
                None,
            )
            .expect("refund should build");

        assert_eq!(
            tx.lock_time.to_consensus_u32(),
            800_100,
            "lock_time must reach the deadline"
        );
        assert!(
            !tx.input[0].sequence.is_final(),
            "a final sequence makes OP_CLTV refuse to run"
        );
        assert_eq!(tx.input[0].witness.len(), 3, "sig, FALSE, script");
    }

    /// A claim needs neither, and must never carry them — a lock_time in the
    /// future would make an immediately-spendable claim unspendable until then.
    #[test]
    fn a_claim_sets_no_locktime_and_reveals_the_secret() {
        let (sk, pk) = keypair(2);
        let secret = Secret::from_bytes([7u8; 32]);
        let p = HtlcParams {
            hash: secret.hash(),
            claim_pubkey: pk,
            refund_pubkey: pk,
            timeout_height: 800_100,
        };
        let to = p.address(Network::Regtest);
        let tx = p
            .spend_transaction(
                SpendPath::Claim,
                SpendRequest {
                    outpoint: bitcoin::OutPoint::null(),
                    value: bitcoin::Amount::from_sat(100_000),
                    to: &to,
                    fee: bitcoin::Amount::from_sat(1_000),
                },
                &sk,
                Some(&secret),
            )
            .expect("claim should build");

        assert_eq!(tx.lock_time.to_consensus_u32(), 0);
        assert!(tx.input[0].sequence.is_final());
        assert_eq!(tx.input[0].witness.len(), 4, "sig, preimage, TRUE, script");
        assert_eq!(tx.input[0].witness.nth(1).unwrap(), secret.as_bytes());
    }

    /// The fee has to come out of the output, or the transaction is invalid for
    /// paying out more than it takes in.
    #[test]
    fn the_fee_comes_out_of_the_spend() {
        let (sk, pk) = keypair(3);
        let p = HtlcParams {
            hash: Secret::from_bytes([7u8; 32]).hash(),
            claim_pubkey: pk,
            refund_pubkey: pk,
            timeout_height: 1,
        };
        let to = p.address(Network::Regtest);
        let tx = p
            .spend_transaction(
                SpendPath::Refund,
                SpendRequest {
                    outpoint: bitcoin::OutPoint::null(),
                    value: bitcoin::Amount::from_sat(100_000),
                    to: &to,
                    fee: bitcoin::Amount::from_sat(1_000),
                },
                &sk,
                None,
            )
            .unwrap();
        assert_eq!(tx.output[0].value, bitcoin::Amount::from_sat(99_000));
    }

    /// Fresh secrets must actually differ.
    #[test]
    fn random_secrets_are_distinct() {
        assert_ne!(Secret::random(), Secret::random());
    }
}
