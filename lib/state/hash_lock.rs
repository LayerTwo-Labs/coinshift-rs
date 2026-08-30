//! Spend rules for [`OutputContent::HashLocked`], the escrow primitive behind
//! atomic swaps.
//!
//! # Why this module exists
//!
//! A swap has two legs on two chains, and the hard part has always been the
//! other chain: consensus cannot see Bitcoin, so anything that asks "did the
//! payment happen?" ends up trusting a node-local RPC — which is how a miner
//! could take an escrow without paying at all.
//!
//! A hash lock removes the question. The maker picks a secret `s` and locks
//! their coins to `sha256(s)`; the taker locks Bitcoin to the same hash with an
//! earlier deadline. Taking the Bitcoin leg necessarily publishes `s`, and the
//! taker uses it here. The only thing this side ever checks is that a supplied
//! preimage hashes to the committed value — a pure function of the transaction,
//! identical on every node, with nothing for anyone to lie about.
//!
//! # The two paths
//!
//! - **Claim.** [`TxData::HashLockClaim`] carries the preimage. Every
//!   `HashLocked` input it spends must hash-match, and each input's `claimant`
//!   must be paid at least that input's value.
//! - **Refund.** An ordinary transaction may reclaim a `HashLocked` output once
//!   the chain reaches its `timeout_height`, paying the output's own address.
//!   Authorization is unmodified here, so the owner signs for it as usual.
//!
//! Every other transaction kind is forbidden from spending a `HashLocked`
//! output, so a swap escrow cannot be quietly swept by a `SwapClaim` or folded
//! into a `SwapCreate`.
//!
//! # Timeout ordering is the security property
//!
//! The party who knows the secret must hold the *later* deadline. The maker
//! knows `s`, so the Coinshift lock expires after the Bitcoin one; the taker
//! therefore always has time to follow the reveal. Inverted, the maker could
//! take the Bitcoin and then reclaim their own escrow once the taker's window
//! had closed. That ordering is enforced by the wallet when it builds the pair,
//! not here — consensus sees only one leg and cannot check the relationship.

use sneed::RoTxn;

use crate::{
    state::{Error, State, swap::amount_paid_to},
    types::{Address, FilledTransaction, OutputContent, Transaction, TxData},
};

/// The height the transaction under validation is being validated at.
///
/// Mirrors `swap::validating_height`: the tip's height is the last block, so
/// the one being validated is the next. Prevalidation, connection and the
/// mempool path all run against the same tip, so all three agree.
fn validating_height(state: &State, rotxn: &RoTxn) -> Result<u32, Error> {
    Ok(state.try_get_height(rotxn)?.map_or(0, |height| height + 1))
}

/// Total value of `HashLocked` inputs, grouped by the address each one owes.
///
/// Grouping matters when a transaction spends more than one lock: paying one
/// claimant twice must not satisfy another claimant's share.
fn owed_by_address<'a, I>(
    inputs: I,
) -> Result<Vec<(Address, bitcoin::Amount)>, Error>
where
    I: Iterator<Item = (Address, bitcoin::Amount)> + 'a,
{
    let mut owed: Vec<(Address, bitcoin::Amount)> = Vec::new();
    for (address, value) in inputs {
        match owed.iter_mut().find(|(existing, _)| *existing == address) {
            Some((_, total)) => {
                *total = total.checked_add(value).ok_or_else(|| {
                    Error::InvalidTransaction(
                        "hash-locked value overflows".to_string(),
                    )
                })?;
            }
            None => owed.push((address, value)),
        }
    }
    Ok(owed)
}

/// Validate a [`TxData::HashLockClaim`]: spending hash locks by revealing the
/// secret.
pub fn validate_hash_lock_claim(
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let TxData::HashLockClaim { preimage } = &transaction.data else {
        return Err(Error::InvalidTransaction(
            "Expected HashLockClaim transaction".to_string(),
        ));
    };

    use bitcoin::hashes::{Hash as _, sha256};
    let digest = sha256::Hash::hash(preimage).to_byte_array();

    let mut claims = Vec::new();
    for utxo in &filled_transaction.spent_utxos {
        let OutputContent::HashLocked {
            value,
            hash,
            claimant,
            ..
        } = &utxo.content
        else {
            continue;
        };
        // One preimage opens one hash. A transaction spending locks with
        // different hashes cannot be opened by a single secret, so it is
        // rejected rather than partially honoured.
        if digest != *hash {
            return Err(Error::InvalidTransaction(
                "HashLockClaim preimage does not match the locked hash"
                    .to_string(),
            ));
        }
        claims.push((*claimant, *value));
    }

    if claims.is_empty() {
        return Err(Error::InvalidTransaction(
            "HashLockClaim must spend at least one hash-locked output"
                .to_string(),
        ));
    }

    for (claimant, owed) in owed_by_address(claims.into_iter())? {
        let paid = amount_paid_to(transaction, &claimant)?;
        if paid < owed {
            return Err(Error::InvalidTransaction(format!(
                "HashLockClaim must pay at least {owed} to {claimant}, but pays {paid}"
            )));
        }
    }
    Ok(())
}

/// Validate an ordinary transaction that spends one or more hash locks: the
/// refund path.
///
/// Refunds are allowed only once the chain has reached each lock's
/// `timeout_height`, and must return the value to the output's own address.
/// A transaction that spends no hash locks passes trivially.
pub fn validate_hash_lock_refund(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let height = validating_height(state, rotxn)?;

    let mut refunds = Vec::new();
    for utxo in &filled_transaction.spent_utxos {
        let OutputContent::HashLocked {
            value,
            timeout_height,
            ..
        } = &utxo.content
        else {
            continue;
        };
        if height < *timeout_height {
            return Err(Error::InvalidTransaction(format!(
                "hash-locked output is not refundable until height {timeout_height}, and this block is {height}"
            )));
        }
        refunds.push((utxo.address, *value));
    }

    for (address, owed) in owed_by_address(refunds.into_iter())? {
        let paid = amount_paid_to(transaction, &address)?;
        if paid < owed {
            return Err(Error::InvalidTransaction(format!(
                "refund of a hash-locked output must pay at least {owed} to {address}, but pays {paid}"
            )));
        }
    }
    Ok(())
}

/// Reject any other transaction kind that tries to spend a hash lock.
///
/// `SwapCreate`, `SwapClaim` and `SwapAccept` each have their own reasons to
/// move value around, and none of them is entitled to a swap escrow. Without
/// this a `SwapClaim` could sweep a hash lock as an incidental input.
pub fn reject_hash_locked_inputs(
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    if filled_transaction
        .spent_utxos
        .iter()
        .any(|utxo| utxo.content.is_hash_locked())
    {
        return Err(Error::InvalidTransaction(
            "only a HashLockClaim or an ordinary refund may spend a hash-locked output"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bitcoin::hashes::{Hash as _, sha256};
    use sneed::Env;

    use super::*;
    use crate::types::{OutPoint, Output, Txid};

    fn sat(value: u64) -> bitcoin::Amount {
        bitcoin::Amount::from_sat(value)
    }

    fn test_state() -> (temp_dir::TempDir, Env, State) {
        let dir = temp_dir::TempDir::new().unwrap();
        let mut opts = heed::EnvOpenOptions::new();
        opts.map_size(10 * 1024 * 1024).max_dbs(State::NUM_DBS);
        let env = unsafe { Env::open(&opts, dir.path()) }.unwrap();
        let state = State::new(&env).unwrap();
        (dir, env, state)
    }

    const SECRET: [u8; 32] = [7u8; 32];

    fn digest_of(preimage: &[u8; 32]) -> [u8; 32] {
        sha256::Hash::hash(preimage).to_byte_array()
    }

    /// A hash lock owned by `owner`, claimable by `claimant` after revealing
    /// `SECRET`, refundable to `owner` at `timeout_height`.
    fn locked_utxo(
        owner: Address,
        claimant: Address,
        value: u64,
        timeout_height: u32,
    ) -> Output {
        Output {
            address: owner,
            content: OutputContent::HashLocked {
                value: sat(value),
                hash: digest_of(&SECRET),
                claimant,
                timeout_height,
            },
        }
    }

    fn tx_paying(data: TxData, to: Address, value: u64) -> Transaction {
        Transaction {
            inputs: vec![(
                OutPoint::Regular {
                    txid: Txid([9u8; 32]),
                    vout: 0,
                },
                [0u8; 32],
            )],
            proof: Default::default(),
            outputs: vec![Output {
                address: to,
                content: OutputContent::Value(sat(value)),
            }],
            data,
        }
    }

    fn filled(tx: &Transaction, spent: Vec<Output>) -> FilledTransaction {
        FilledTransaction {
            spent_utxos: spent,
            transaction: tx.clone(),
        }
    }

    /// The happy path: the right secret, paying the named claimant.
    #[test]
    fn a_correct_preimage_paying_the_claimant_is_accepted() {
        let (owner, claimant) = (Address([1u8; 20]), Address([2u8; 20]));
        let tx = tx_paying(
            TxData::HashLockClaim { preimage: SECRET },
            claimant,
            50_000,
        );
        let f = filled(&tx, vec![locked_utxo(owner, claimant, 50_000, 100)]);
        assert!(validate_hash_lock_claim(&tx, &f).is_ok());
    }

    /// The whole security property in one test: without the secret, nothing.
    #[test]
    fn a_wrong_preimage_is_rejected() {
        let (owner, claimant) = (Address([1u8; 20]), Address([2u8; 20]));
        let tx = tx_paying(
            TxData::HashLockClaim {
                preimage: [8u8; 32],
            },
            claimant,
            50_000,
        );
        let f = filled(&tx, vec![locked_utxo(owner, claimant, 50_000, 100)]);
        assert!(matches!(
            validate_hash_lock_claim(&tx, &f),
            Err(Error::InvalidTransaction(_))
        ));
    }

    /// Knowing the secret entitles you to nothing if you pay yourself.
    #[test]
    fn the_right_secret_paying_the_wrong_address_is_rejected() {
        let (owner, claimant) = (Address([1u8; 20]), Address([2u8; 20]));
        let attacker = Address([3u8; 20]);
        let tx = tx_paying(
            TxData::HashLockClaim { preimage: SECRET },
            attacker,
            50_000,
        );
        let f = filled(&tx, vec![locked_utxo(owner, claimant, 50_000, 100)]);
        assert!(matches!(
            validate_hash_lock_claim(&tx, &f),
            Err(Error::InvalidTransaction(_))
        ));
    }

    /// Underpaying the claimant is the same failure as not paying them.
    #[test]
    fn underpaying_the_claimant_is_rejected() {
        let (owner, claimant) = (Address([1u8; 20]), Address([2u8; 20]));
        let tx = tx_paying(
            TxData::HashLockClaim { preimage: SECRET },
            claimant,
            49_999,
        );
        let f = filled(&tx, vec![locked_utxo(owner, claimant, 50_000, 100)]);
        assert!(matches!(
            validate_hash_lock_claim(&tx, &f),
            Err(Error::InvalidTransaction(_))
        ));
    }

    /// Two locks to the same claimant owe the sum, not the larger of the two —
    /// paying one input's value must not settle both.
    #[test]
    fn two_locks_to_one_claimant_owe_the_sum() {
        let (owner, claimant) = (Address([1u8; 20]), Address([2u8; 20]));
        let tx = tx_paying(
            TxData::HashLockClaim { preimage: SECRET },
            claimant,
            50_000,
        );
        let f = filled(
            &tx,
            vec![
                locked_utxo(owner, claimant, 50_000, 100),
                locked_utxo(owner, claimant, 50_000, 100),
            ],
        );
        assert!(
            matches!(
                validate_hash_lock_claim(&tx, &f),
                Err(Error::InvalidTransaction(_))
            ),
            "paying 50_000 must not discharge two 50_000 locks"
        );
    }

    /// The refund boundary. Height is the block being validated, so a lock with
    /// `timeout_height = 5` is refundable in block 5 and not in block 4.
    #[test]
    fn refund_is_rejected_before_the_timeout_and_allowed_at_it() {
        let owner = Address([1u8; 20]);
        let claimant = Address([2u8; 20]);
        let tx = tx_paying(TxData::Regular, owner, 50_000);
        let f = filled(&tx, vec![locked_utxo(owner, claimant, 50_000, 5)]);

        for (tip, refundable) in
            [(None, false), (Some(3u32), false), (Some(4u32), true)]
        {
            let (_dir, env, state) = test_state();
            let mut rwtxn = env.write_txn().unwrap();
            if let Some(tip) = tip {
                state.height.put(&mut rwtxn, &(), &tip).unwrap();
            }
            rwtxn.commit().unwrap();
            let rotxn = env.read_txn().unwrap();
            let result = validate_hash_lock_refund(&state, &rotxn, &tx, &f);
            assert_eq!(
                result.is_ok(),
                refundable,
                "tip {tip:?} validates block {}, timeout is 5",
                tip.map_or(0, |t| t + 1)
            );
        }
    }

    /// A refund that pays someone other than the output's owner is rejected,
    /// even after the timeout.
    #[test]
    fn a_refund_must_pay_the_outputs_own_address() {
        let owner = Address([1u8; 20]);
        let claimant = Address([2u8; 20]);
        let attacker = Address([3u8; 20]);
        let tx = tx_paying(TxData::Regular, attacker, 50_000);
        let f = filled(&tx, vec![locked_utxo(owner, claimant, 50_000, 0)]);

        let (_dir, env, state) = test_state();
        let rotxn = env.read_txn().unwrap();
        assert!(matches!(
            validate_hash_lock_refund(&state, &rotxn, &tx, &f),
            Err(Error::InvalidTransaction(_))
        ));
    }

    /// A claim that spends no hash lock at all is not a claim.
    #[test]
    fn a_claim_spending_no_hash_lock_is_rejected() {
        let claimant = Address([2u8; 20]);
        let tx = tx_paying(
            TxData::HashLockClaim { preimage: SECRET },
            claimant,
            50_000,
        );
        let f = filled(
            &tx,
            vec![Output {
                address: claimant,
                content: OutputContent::Value(sat(50_000)),
            }],
        );
        assert!(matches!(
            validate_hash_lock_claim(&tx, &f),
            Err(Error::InvalidTransaction(_))
        ));
    }

    /// Swap transactions must not sweep an escrow as an incidental input.
    #[test]
    fn other_transaction_kinds_may_not_spend_a_hash_lock() {
        let (owner, claimant) = (Address([1u8; 20]), Address([2u8; 20]));
        let tx = tx_paying(TxData::Regular, owner, 50_000);
        let f = filled(&tx, vec![locked_utxo(owner, claimant, 50_000, 0)]);
        assert!(matches!(
            reject_hash_locked_inputs(&f),
            Err(Error::InvalidTransaction(_))
        ));

        let plain = filled(
            &tx,
            vec![Output {
                address: owner,
                content: OutputContent::Value(sat(50_000)),
            }],
        );
        assert!(reject_hash_locked_inputs(&plain).is_ok());
    }

    /// The one that would silently break atomicity: our lock and the Bitcoin
    /// HTLC must agree on the hash function. Bitcoin's OP_SHA256 is SHA-256,
    /// and this codebase reaches for blake3 elsewhere.
    #[test]
    fn the_commitment_is_sha256_not_blake3() {
        let expected = sha256::Hash::hash(&SECRET).to_byte_array();
        assert_eq!(digest_of(&SECRET), expected);
        assert_ne!(
            digest_of(&SECRET),
            *blake3::hash(&SECRET).as_bytes(),
            "a blake3 commitment cannot be opened by a Bitcoin OP_SHA256 HTLC"
        );
    }
}
