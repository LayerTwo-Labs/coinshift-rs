//! Swap validation and processing

use sneed::RoTxn;

use crate::{
    state::{Error, State, l1_proof},
    types::{
        Address, FilledTransaction, GetValue, OutputContent, SwapId,
        Transaction, TxData,
    },
};

/// Total value of the outputs paying `recipient`.
fn amount_paid_to(
    transaction: &Transaction,
    recipient: &Address,
) -> Result<bitcoin::Amount, Error> {
    transaction
        .outputs
        .iter()
        .filter(|output| output.address == *recipient)
        .map(GetValue::get_value)
        .try_fold(bitcoin::Amount::ZERO, |acc, val| acc.checked_add(val))
        .ok_or_else(|| {
            Error::InvalidTransaction(
                "Output value overflow in SwapClaim".to_string(),
            )
        })
}

/// Validate a SwapCreate transaction
pub fn validate_swap_create(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let TxData::SwapCreate {
        swap_id,
        parent_chain,
        l1_txid_bytes: _,
        required_confirmations: _,
        l2_recipient,
        l2_amount,
        l1_recipient_address,
        l1_amount,
    } = &transaction.data
    else {
        return Err(Error::InvalidTransaction(
            "Expected SwapCreate transaction".to_string(),
        ));
    };

    // 1. Verify swap ID matches computed ID
    let computed_swap_id = {
        // L2 → L1 swap
        // We need the sender's address - get it from the first input
        let first_input =
            filled_transaction.spent_utxos.first().ok_or_else(|| {
                Error::InvalidTransaction(
                    "SwapCreate must have inputs".to_string(),
                )
            })?;
        let l2_sender_address = first_input.address;
        SwapId::from_l2_to_l1(
            l1_recipient_address,
            bitcoin::Amount::from_sat(*l1_amount),
            &l2_sender_address,
            l2_recipient.as_ref(), // Now optional
        )
    };

    if computed_swap_id.0 != *swap_id {
        return Err(Error::InvalidTransaction(format!(
            "Swap ID mismatch: expected {}, computed {}",
            hex::encode(swap_id),
            computed_swap_id
        )));
    }

    // 2. Verify swap doesn't already exist
    if state.get_swap(rotxn, &computed_swap_id)?.is_some() {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} already exists",
            computed_swap_id
        )));
    }

    // 3. Verify l2_amount > 0
    if *l2_amount == 0 {
        return Err(Error::InvalidTransaction(
            "L2 amount must be greater than zero".to_string(),
        ));
    }

    // 3b. The swap must be claimable in principle: a claim has to prove the
    // L1 payment against the mainchain headers this sidechain validates, so
    // the parent chain must be that chain, and the recipient must be an
    // address on it that a proof can be matched against. Otherwise the
    // escrow could only ever come back through expiry.
    let parent_chain = *parent_chain;
    if !parent_chain.supports_payment_proofs() {
        return Err(Error::InvalidTransaction(format!(
            "Parent chain {parent_chain:?} is not the chain this sidechain is \
             anchored to; payments on it cannot be proven, so swaps against \
             it cannot be created"
        )));
    }
    let l1_address_valid = l1_recipient_address
        .parse::<bitcoin::Address<bitcoin::address::NetworkUnchecked>>()
        .ok()
        .and_then(|addr| {
            addr.require_network(parent_chain.to_bitcoin_network()).ok()
        })
        .is_some();
    if !l1_address_valid {
        return Err(Error::InvalidTransaction(format!(
            "L1 recipient address {l1_recipient_address} is not a valid \
             {parent_chain:?} address"
        )));
    }

    // 4. Verify transaction has outputs
    if transaction.outputs.is_empty() {
        return Err(Error::InvalidTransaction(
            "Transaction must have at least one output".to_string(),
        ));
    }

    // 5. For L2 → L1 swaps, verify inputs aren't locked and sufficient funds
    // Check that no inputs are locked to another swap
    for (outpoint, _) in &transaction.inputs {
        if let Some(locked_swap_id) =
            state.is_output_locked_to_swap(rotxn, outpoint)?
            && locked_swap_id.0 != *swap_id
        {
            // Check if the locked swap exists and is valid
            match state.get_swap(rotxn, &locked_swap_id) {
                Ok(Some(_)) => {
                    // Swap exists and is valid - this is a real lock
                    return Err(Error::InvalidTransaction(format!(
                        "Input {} is locked to swap {}",
                        outpoint, locked_swap_id
                    )));
                }
                Ok(None) => {
                    // Swap doesn't exist - orphaned lock
                    return Err(Error::OrphanedLock {
                        outpoint: *outpoint,
                        swap_id: locked_swap_id,
                    });
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
                        return Err(Error::OrphanedLock {
                            outpoint: *outpoint,
                            swap_id: locked_swap_id,
                        });
                    } else {
                        // Other database error - return original error
                        return Err(Error::InvalidTransaction(format!(
                            "Input {} is locked to swap {}, but error checking swap: {}",
                            outpoint, locked_swap_id, err
                        )));
                    }
                }
            }
        }
    }

    // Verify transaction spends at least l2_amount
    let total_input_value = filled_transaction
        .spent_utxos
        .iter()
        .map(crate::types::GetValue::get_value)
        .try_fold(bitcoin::Amount::ZERO, |acc, val| {
            acc.checked_add(val).ok_or(())
        })
        .map_err(|_| {
            Error::InvalidTransaction("Input value overflow".to_string())
        })?;

    let required_amount = bitcoin::Amount::from_sat(*l2_amount);
    if total_input_value < required_amount {
        return Err(Error::InvalidTransaction(format!(
            "Insufficient funds: need {}, have {}",
            required_amount, total_input_value
        )));
    }

    // The declared `l2_amount` becomes a consensus obligation once the swap is
    // saved: a claim must pay the recipient at least `swap.l2_amount`. Spending
    // enough inputs is not sufficient, since only the `SwapPending` outputs
    // carrying this swap's id are locked when the block connects; the rest is
    // change the creator keeps. Require those outputs to actually escrow the
    // declared amount, otherwise a creator could declare an inflated
    // `l2_amount` while locking a token value, leaving the L1 filler unable to
    // claim after having already paid on L1.
    let escrowed_value = transaction
        .outputs
        .iter()
        .filter_map(|output| match output.content {
            OutputContent::SwapPending {
                value,
                swap_id: output_swap_id,
            } if output_swap_id == *swap_id => Some(value),
            _ => None,
        })
        .try_fold(bitcoin::Amount::ZERO, |acc, val| {
            acc.checked_add(val).ok_or(())
        })
        .map_err(|_| {
            Error::InvalidTransaction(
                "SwapPending output value overflow".to_string(),
            )
        })?;

    if escrowed_value < required_amount {
        return Err(Error::InvalidTransaction(format!(
            "SwapCreate must lock at least {} in SwapPending outputs for swap {}, but locks {}",
            required_amount, computed_swap_id, escrowed_value
        )));
    }

    Ok(())
}

/// Height of the block currently being validated.
///
/// `try_get_height` returns the tip's height, so the block under validation is
/// the next one. `prevalidate_block`, `connect_block` and the mempool path all
/// run against the same tip, so they all agree on this value.
fn validating_height(state: &State, rotxn: &RoTxn) -> Result<u32, Error> {
    Ok(state.try_get_height(rotxn)?.map_or(0, |height| height + 1))
}

/// Validate a `SwapAccept`: a reservation of an open swap for a claimer.
///
/// Every rule here is decided from block data alone, so the reservation is
/// identical on every node. Since claims carry an L1 payment proof that
/// itself names the claimer (see [`validate_swap_claim_consensus`]), the
/// reservation is coordination rather than security: it tells other takers
/// the swap is spoken for so two of them do not both pay on L1. Entitlement
/// to the escrow comes from the proven payment, not from the reservation.
pub fn validate_swap_accept(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let TxData::SwapAccept {
        swap_id,
        l2_claimer_address,
    } = &transaction.data
    else {
        return Err(Error::InvalidTransaction(
            "Expected SwapAccept transaction".to_string(),
        ));
    };
    let swap_id = SwapId(*swap_id);

    // A reservation must not double as a way to move escrowed value.
    let () = validate_no_locked_outputs(state, rotxn, transaction)?;

    let swap = state
        .get_swap(rotxn, &swap_id)?
        .ok_or(Error::SwapNotFound { swap_id })?;

    if swap.l2_recipient.is_some() {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} is not an open swap; its recipient was fixed at creation",
            swap_id
        )));
    }

    let height = validating_height(state, rotxn)?;

    if let Some(expires_at) = swap.expires_at_height
        && height >= expires_at
    {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} expired at height {}, cannot be accepted at {}",
            swap_id, expires_at, height
        )));
    }

    // First reservation wins while it is live; a lapsed one releases the swap.
    if let Some(reservation) = state.get_swap_reservation(rotxn, &swap_id)?
        && reservation.is_live_at(swap.parent_chain, height)
    {
        return Err(Error::InvalidTransaction(format!(
            "Swap {} is already reserved by {} until height {}",
            swap_id,
            reservation.claimer,
            reservation.expires_at(swap.parent_chain),
        )));
    }

    // Proof that the reserver controls the address they are reserving for:
    // spend an input owned by it. `prevalidate`/`connect` already require every
    // authorization to match its spent output's address (SwapAccept gets no
    // exemption), so this input carries a signature from that key.
    //
    // Without it, anyone could reserve every open swap for an address they do
    // not own and stall the market for free.
    if !filled_transaction
        .spent_utxos
        .iter()
        .any(|utxo| utxo.address == *l2_claimer_address)
    {
        return Err(Error::InvalidTransaction(format!(
            "SwapAccept must spend an input owned by {}, the address it reserves for",
            l2_claimer_address
        )));
    }

    Ok(())
}

/// Validate a SwapClaim transaction in the mempool.
///
/// Exactly [`validate_swap_claim_consensus`], anchored at the mainchain block
/// the current tip was built against. The claim carries its own proof of the
/// L1 payment, so nothing here depends on this node's parent-chain monitoring
/// or on the node-local `Swap::state`; a claim the mempool accepts is one
/// the next block may contain, and vice versa.
pub fn validate_swap_claim(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    _filled_transaction: &FilledTransaction,
) -> Result<(), Error> {
    let anchor = state.tip_main_anchor(rotxn)?.ok_or_else(|| {
        Error::InvalidTransaction(
            "No sidechain tip to anchor L1 payment proofs to".to_string(),
        )
    })?;
    validate_swap_claim_consensus(state, rotxn, transaction, anchor).map(|_| ())
}

/// Validate that non-SwapClaim transactions don't spend locked outputs
pub fn validate_no_locked_outputs(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
) -> Result<(), Error> {
    // Skip validation for SwapClaim transactions
    if matches!(transaction.data, TxData::SwapClaim { .. }) {
        return Ok(());
    }

    // Check that no inputs are locked
    for (outpoint, _) in &transaction.inputs {
        if let Some(locked_swap_id) =
            state.is_output_locked_to_swap(rotxn, outpoint)?
        {
            // A lock whose swap record is missing or unreadable protects
            // nothing; report it as such so `Node::submit_transaction` can
            // clean it up and retry instead of leaving the output stranded.
            if state.get_swap(rotxn, &locked_swap_id)?.is_none() {
                return Err(Error::OrphanedLock {
                    outpoint: *outpoint,
                    swap_id: locked_swap_id,
                });
            }
            return Err(Error::InvalidTransaction(format!(
                "Cannot spend locked output {} (locked to swap {})",
                outpoint, locked_swap_id
            )));
        }
    }

    Ok(())
}

/// Validate a `SwapClaim` against consensus rules.
///
/// `anchor` is the mainchain block the L2 block being validated was built
/// against (`Header::prev_main_hash`). Every rule is decided from the claim,
/// the swap record and mainchain headers the node has validated, so every
/// node reaches the same verdict:
///
/// - the claim spends at least one output locked to its swap and nothing
///   locked to another swap;
/// - the swap record exists (fail closed on a missing or unreadable one);
/// - `proof_data` proves a parent-chain payment of at least `l1_amount` to
///   the swap's L1 recipient, in a block on `anchor`'s ancestry at least
///   `required_confirmations` deep, committing to this swap and to an L2
///   claimer address (see [`l1_proof`]);
/// - the claim pays that claimer at least `l2_amount`.
///
/// The proven payment is returned so `connect` can record it on the swap.
pub fn validate_swap_claim_consensus(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    anchor: bitcoin::BlockHash,
) -> Result<l1_proof::VerifiedL1Payment, Error> {
    let TxData::SwapClaim {
        swap_id,
        l2_claimer_address,
        proof_data,
    } = &transaction.data
    else {
        return Err(Error::InvalidTransaction(
            "Expected SwapClaim transaction".to_string(),
        ));
    };
    let swap_id = SwapId(*swap_id);

    // The claim must spend at least one output locked to this swap, and must
    // not spend any output locked to a different swap.
    let mut found_locked_input = false;
    for (outpoint, _) in &transaction.inputs {
        if let Some(locked_swap_id) =
            state.is_output_locked_to_swap(rotxn, outpoint)?
        {
            if locked_swap_id != swap_id {
                return Err(Error::InvalidTransaction(format!(
                    "Input {} is locked to different swap {}",
                    outpoint, locked_swap_id
                )));
            }
            found_locked_input = true;
        }
    }
    if !found_locked_input {
        return Err(Error::InvalidTransaction(
            "SwapClaim must spend at least one output locked to the swap"
                .to_string(),
        ));
    }

    // Fail closed if the swap record cannot be read. `State::get_swap` reports a
    // corrupted record as `Ok(None)`, and skipping the payout rule in that case
    // would let the escrow be spent to anywhere — on whichever nodes happened to
    // hit the bad record, which is also how nodes would come to disagree.
    let swap = state
        .get_swap(rotxn, &swap_id)?
        .ok_or(Error::SwapNotFound { swap_id })?;

    // Who is entitled to the escrow: whoever proves they paid for it.
    let proof = proof_data.as_deref().ok_or(Error::MissingL1Proof)?;
    let payment = state.verify_l1_payment_proof(rotxn, proof, &swap, anchor)?;
    let expected_recipient = payment.claimer;

    // The claim may still carry the legacy `l2_claimer_address` field. If it
    // does, it must agree with the proven payment rather than override it.
    if let Some(declared) = l2_claimer_address
        && *declared != expected_recipient
    {
        return Err(Error::InvalidTransaction(format!(
            "SwapClaim declares claimer {} but the L1 payment for swap {} \
             commits to {}",
            declared, swap_id, expected_recipient
        )));
    }

    // Paying the recipient a token amount and keeping the rest is the same
    // theft with an extra output, so require the full amount.
    let amount_to_recipient = amount_paid_to(transaction, &expected_recipient)?;
    if amount_to_recipient < swap.l2_amount {
        return Err(Error::InvalidTransaction(format!(
            "SwapClaim must pay at least {} to {}, but pays {}",
            swap.l2_amount, expected_recipient, amount_to_recipient
        )));
    }

    Ok(payment)
}

/// Validate swap consensus rules for a single transaction in a block.
///
/// Block validation must enforce the same swap rules as the mempool path in
/// [`State::validate_transaction`]; otherwise a miner could include a swap
/// transaction that no node would accept from the mempool, e.g. a regular
/// transaction spending a swap-locked output, or a claim that pays the attacker
/// instead of the swap recipient. `anchor` is the block header's
/// `prev_main_hash`; see [`validate_swap_claim_consensus`].
pub fn validate_block_transaction(
    state: &State,
    rotxn: &RoTxn,
    transaction: &Transaction,
    filled_transaction: &FilledTransaction,
    anchor: bitcoin::BlockHash,
) -> Result<(), Error> {
    match &transaction.data {
        TxData::SwapCreate { .. } => {
            validate_swap_create(state, rotxn, transaction, filled_transaction)
        }
        TxData::SwapClaim { .. } => {
            validate_swap_claim_consensus(state, rotxn, transaction, anchor)
                .map(|_| ())
        }
        TxData::SwapAccept { .. } => {
            validate_swap_accept(state, rotxn, transaction, filled_transaction)
        }
        TxData::Regular => {
            validate_no_locked_outputs(state, rotxn, transaction)
        }
    }
}

#[cfg(test)]
mod tests {
    use sneed::Env;

    use bitcoin::hashes::Hash as _;

    use super::*;
    use crate::types::{
        Address, OutPoint, Output, OutputContent, ParentChainType, Swap,
        SwapDirection, SwapState, SwapTxId, Transaction, Txid,
    };

    fn sat(value: u64) -> bitcoin::Amount {
        bitcoin::Amount::from_sat(value)
    }

    /// Build a `State` backed by a fresh temporary LMDB environment.
    fn test_state() -> (temp_dir::TempDir, Env, State) {
        let dir = temp_dir::TempDir::new().unwrap();
        let mut opts = heed::EnvOpenOptions::new();
        opts.map_size(10 * 1024 * 1024).max_dbs(State::NUM_DBS);
        let env = unsafe { Env::open(&opts, dir.path()) }.unwrap();
        let state = State::new(&env).unwrap();
        (dir, env, state)
    }

    /// Zero anchor for tests that never reach proof verification.
    #[allow(non_snake_case)]
    fn NO_ANCHOR() -> bitcoin::BlockHash {
        bitcoin::BlockHash::from_byte_array([0u8; 32])
    }

    const L1_RECIPIENT: &str = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080";

    fn l1_recipient() -> bitcoin::Address {
        L1_RECIPIENT
            .parse::<bitcoin::Address<bitcoin::address::NetworkUnchecked>>()
            .unwrap()
            .require_network(bitcoin::Network::Regtest)
            .unwrap()
    }

    /// A regular (non-claim) transaction that spends a swap-locked output must
    /// be rejected by block validation, just as it is in the mempool.
    #[test]
    fn block_validation_rejects_spending_locked_output() {
        let (_dir, env, state) = test_state();
        let swap_id = SwapId([7u8; 32]);
        let outpoint = OutPoint::Regular {
            txid: Txid([9u8; 32]),
            vout: 0,
        };
        let locked_output = Output {
            address: Address([1u8; 20]),
            content: OutputContent::SwapPending {
                value: sat(50_000),
                swap_id: swap_id.0,
            },
        };
        let swap = Swap::new(
            swap_id,
            SwapDirection::L2ToL1,
            ParentChainType::Regtest,
            SwapTxId::Hash32([0u8; 32]),
            None,
            Some(Address([2u8; 20])),
            sat(50_000),
            L1_RECIPIENT.to_string(),
            sat(40_000),
            0,
            None,
            Some(Address([1u8; 20])),
        );

        let mut rwtxn = env.write_txn().unwrap();
        state.save_swap(&mut rwtxn, &swap).unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
            .unwrap();
        rwtxn.commit().unwrap();

        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: Address([2u8; 20]),
                content: OutputContent::Value(sat(50_000)),
            }],
            data: TxData::Regular,
        };
        let filled = FilledTransaction {
            spent_utxos: vec![locked_output],
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(
            &state,
            &rotxn,
            &tx,
            &filled,
            NO_ANCHOR(),
        );
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "spending a locked output in a regular tx should be rejected, got {result:?}"
        );
    }

    /// A swap with `l2_amount` 50_000 escrowed in one locked output, either
    /// open (`l2_recipient == None`) or fixed to `recipient`.
    struct SwapFixture {
        _dir: temp_dir::TempDir,
        env: Env,
        state: State,
        swap: Swap,
        outpoint: OutPoint,
        locked_output: Output,
    }

    fn swap_fixture(l2_recipient: Option<Address>) -> SwapFixture {
        let (dir, env, state) = test_state();
        let creator = Address([5u8; 20]);
        let swap_id = SwapId([8u8; 32]);
        let outpoint = OutPoint::Regular {
            txid: Txid([1u8; 32]),
            vout: 0,
        };
        let locked_output = Output {
            address: creator,
            content: OutputContent::SwapPending {
                value: sat(50_000),
                swap_id: swap_id.0,
            },
        };
        let swap = Swap::new(
            swap_id,
            SwapDirection::L2ToL1,
            ParentChainType::Regtest,
            SwapTxId::Hash32([0u8; 32]),
            Some(2), // required_confirmations
            l2_recipient,
            sat(50_000),
            L1_RECIPIENT.to_string(),
            sat(40_000),
            0,
            None,
            Some(creator),
        );

        let mut rwtxn = env.write_txn().unwrap();
        state.save_swap(&mut rwtxn, &swap).unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &outpoint, &swap_id)
            .unwrap();
        rwtxn.commit().unwrap();

        SwapFixture {
            _dir: dir,
            env,
            state,
            swap,
            outpoint,
            locked_output,
        }
    }

    fn pre_specified_swap_state() -> (
        temp_dir::TempDir,
        Env,
        State,
        SwapId,
        OutPoint,
        Output,
        Address,
    ) {
        let recipient = Address([3u8; 20]);
        let SwapFixture {
            _dir,
            env,
            state,
            swap,
            outpoint,
            locked_output,
        } = swap_fixture(Some(recipient));
        (
            _dir,
            env,
            state,
            swap.id,
            outpoint,
            locked_output,
            recipient,
        )
    }

    /// A synthetic mainchain in `state.main_header_infos`: base block at
    /// height 100, the payment block at 101, then two more. Returns the proof
    /// for `payment` and the block hashes from the payment block up to the
    /// tip, so a test can pick its anchor (and thus the confirmation count).
    fn install_payment(
        state: &State,
        env: &Env,
        payment: &bitcoin::Transaction,
    ) -> (Vec<u8>, Vec<bitcoin::BlockHash>) {
        use crate::{
            state::l1_proof::test_support::{
                block_with, chain, header_info, proof_for,
            },
            types::proto::mainchain::BlockHeaderInfo,
        };
        let base = bitcoin::BlockHash::from_byte_array([1u8; 32]);
        let mut rwtxn = env.write_txn().unwrap();
        state
            .main_header_infos
            .put(
                &mut rwtxn,
                &base,
                &BlockHeaderInfo {
                    block_hash: base,
                    prev_block_hash: bitcoin::BlockHash::from_byte_array(
                        [0u8; 32],
                    ),
                    height: 100,
                    work: bitcoin::Work::from_be_bytes([0u8; 32]),
                },
            )
            .unwrap();
        let payment_block = block_with(base, payment.clone());
        state
            .main_header_infos
            .put(
                &mut rwtxn,
                &payment_block.block_hash(),
                &header_info(&payment_block, 101),
            )
            .unwrap();
        let mut hashes = vec![payment_block.block_hash()];
        for (block, info) in chain(payment_block.block_hash(), 101, 2) {
            state
                .main_header_infos
                .put(&mut rwtxn, &block.block_hash(), &info)
                .unwrap();
            hashes.push(block.block_hash());
        }
        rwtxn.commit().unwrap();
        (proof_for(&payment_block, payment), hashes)
    }

    /// An L1 payment of the swap's `l1_amount` to its recipient, committing
    /// to the swap and `claimer`.
    fn l1_payment(swap: &Swap, claimer: &Address) -> bitcoin::Transaction {
        crate::state::l1_proof::test_support::payment(
            &l1_recipient(),
            swap.l1_amount,
            Some(l1_proof::commitment_script_for(&swap.id, claimer)),
        )
    }

    fn claim_tx(
        swap_id: SwapId,
        outpoint: OutPoint,
        declared_claimer: Option<Address>,
        outputs: Vec<(Address, u64)>,
        proof_data: Option<Vec<u8>>,
    ) -> Transaction {
        Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: outputs
                .into_iter()
                .map(|(address, value)| Output {
                    address,
                    content: OutputContent::Value(sat(value)),
                })
                .collect(),
            data: TxData::SwapClaim {
                swap_id: swap_id.0,
                l2_claimer_address: declared_claimer,
                proof_data,
            },
        }
    }

    fn validate_claim(
        fx: &SwapFixture,
        tx: &Transaction,
        anchor: bitcoin::BlockHash,
    ) -> Result<(), Error> {
        let filled = FilledTransaction {
            spent_utxos: vec![fx.locked_output.clone()],
            transaction: tx.clone(),
        };
        let rotxn = fx.env.read_txn().unwrap();
        validate_block_transaction(&fx.state, &rotxn, tx, &filled, anchor)
    }

    /// The attack from the report: a block producer builds a claim on an
    /// open swap paying themselves, with no L1 payment anywhere. Without a
    /// proof there is nothing to check and the claim is invalid, on the block
    /// path, where it counts.
    #[test]
    fn block_validation_rejects_claim_without_proof() {
        let fx = swap_fixture(None);
        let attacker = Address([4u8; 20]);
        let tx = claim_tx(
            fx.swap.id,
            fx.outpoint,
            Some(attacker),
            vec![(attacker, 50_000)],
            None,
        );
        let result = validate_claim(&fx, &tx, NO_ANCHOR());
        assert!(
            matches!(result, Err(Error::MissingL1Proof)),
            "claim without proof must be rejected, got {result:?}"
        );
    }

    /// The honest case: the filler paid on L1 with a commitment to their L2
    /// address, the payment is deep enough below the anchor, and the claim
    /// pays that address the full amount.
    #[test]
    fn block_validation_accepts_claim_with_proof_paying_committed_claimer() {
        let fx = swap_fixture(None);
        let claimer = Address([7u8; 20]);
        let (proof, hashes) = install_payment(
            &fx.state,
            &fx.env,
            &l1_payment(&fx.swap, &claimer),
        );
        let tx = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(claimer, 50_000)],
            Some(proof),
        );
        // Anchored one block after the payment: two confirmations, as required.
        let result = validate_claim(&fx, &tx, hashes[1]);
        assert!(result.is_ok(), "honest claim must pass, got {result:?}");
    }

    /// The front-running variant: someone else takes the filler's proof
    /// (it is public once the L1 payment confirms) and builds a claim paying
    /// themselves. The proof names the filler, so the payout rule fails.
    #[test]
    fn block_validation_rejects_claim_paying_other_than_committed_claimer() {
        let fx = swap_fixture(None);
        let filler = Address([7u8; 20]);
        let attacker = Address([4u8; 20]);
        let (proof, hashes) =
            install_payment(&fx.state, &fx.env, &l1_payment(&fx.swap, &filler));
        for (declared, outputs, what) in [
            (
                Some(attacker),
                vec![(attacker, 50_000)],
                "declared and paid",
            ),
            (None, vec![(attacker, 50_000)], "paid"),
            (None, vec![(filler, 1), (attacker, 49_999)], "underpaid"),
        ] {
            let tx = claim_tx(
                fx.swap.id,
                fx.outpoint,
                declared,
                outputs,
                Some(proof.clone()),
            );
            let result = validate_claim(&fx, &tx, hashes[2]);
            assert!(
                matches!(result, Err(Error::InvalidTransaction(_))),
                "{what} attacker must be rejected, got {result:?}"
            );
        }
    }

    /// Confirmations are counted from the anchor, so the same proof is
    /// rejected in a block built against the payment's own mainchain block
    /// and accepted in one built two blocks later.
    #[test]
    fn block_validation_counts_confirmations_from_anchor() {
        let fx = swap_fixture(None);
        let claimer = Address([7u8; 20]);
        let (proof, hashes) = install_payment(
            &fx.state,
            &fx.env,
            &l1_payment(&fx.swap, &claimer),
        );
        let tx = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(claimer, 50_000)],
            Some(proof),
        );
        let early = validate_claim(&fx, &tx, hashes[0]);
        assert!(
            matches!(
                early,
                Err(Error::L1Proof(l1_proof::Error::NotEnoughConfirmations {
                    confirmations: 1,
                    required: 2
                }))
            ),
            "one confirmation must not satisfy two, got {early:?}"
        );
        assert!(validate_claim(&fx, &tx, hashes[2]).is_ok());
        // An anchor the node does not know is not a usable chain view.
        let unknown = validate_claim(
            &fx,
            &tx,
            bitcoin::BlockHash::from_byte_array([9u8; 32]),
        );
        assert!(matches!(
            unknown,
            Err(Error::L1Proof(l1_proof::Error::UnknownAnchor(_)))
        ));
    }

    /// A payment for a different swap, even to the same address and amount,
    /// proves nothing about this one.
    #[test]
    fn block_validation_rejects_proof_for_another_swap() {
        let fx = swap_fixture(None);
        let claimer = Address([7u8; 20]);
        let mut other = fx.swap.clone();
        other.id = SwapId([99u8; 32]);
        let (proof, hashes) =
            install_payment(&fx.state, &fx.env, &l1_payment(&other, &claimer));
        let tx = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(claimer, 50_000)],
            Some(proof),
        );
        let result = validate_claim(&fx, &tx, hashes[2]);
        assert!(
            matches!(
                result,
                Err(Error::L1Proof(l1_proof::Error::WrongSwap(_)))
            ),
            "proof for another swap must be rejected, got {result:?}"
        );
    }

    /// Pre-specified swaps: the payment must commit to the fixed recipient,
    /// and the claim must pay that recipient. A block producer who is the
    /// recipient still cannot force-complete the swap without paying.
    #[test]
    fn block_validation_pre_specified_swap_requires_payment_to_recipient() {
        let recipient = Address([3u8; 20]);
        let fx = swap_fixture(Some(recipient));
        let attacker = Address([4u8; 20]);

        // No proof at all: the "force-complete without paying" attack.
        let forced = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(recipient, 50_000)],
            None,
        );
        assert!(matches!(
            validate_claim(&fx, &forced, NO_ANCHOR()),
            Err(Error::MissingL1Proof)
        ));

        // A payment committing to someone else does not fill this swap.
        let (wrong_proof, hashes) = install_payment(
            &fx.state,
            &fx.env,
            &l1_payment(&fx.swap, &attacker),
        );
        let tx = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(recipient, 50_000)],
            Some(wrong_proof),
        );
        assert!(matches!(
            validate_claim(&fx, &tx, hashes[2]),
            Err(Error::L1Proof(l1_proof::Error::ClaimerMismatch { .. }))
        ));

        // The right payment, paying the recipient: valid. Paying anyone
        // else with it: invalid.
        let fx = swap_fixture(Some(recipient));
        let (proof, hashes) = install_payment(
            &fx.state,
            &fx.env,
            &l1_payment(&fx.swap, &recipient),
        );
        let honest = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(recipient, 50_000)],
            Some(proof.clone()),
        );
        assert!(validate_claim(&fx, &honest, hashes[2]).is_ok());
        let diverted = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(attacker, 50_000)],
            Some(proof),
        );
        assert!(matches!(
            validate_claim(&fx, &diverted, hashes[2]),
            Err(Error::InvalidTransaction(_))
        ));
    }

    /// The mempool path is the consensus path anchored at the tip: it needs
    /// no node-local `ReadyToClaim` state and no observed fill, only the
    /// proof, so honest claims relay on nodes that never watched L1.
    #[test]
    fn mempool_claim_validates_proof_against_tip_anchor() {
        let fx = swap_fixture(None);
        let claimer = Address([7u8; 20]);
        let (proof, hashes) = install_payment(
            &fx.state,
            &fx.env,
            &l1_payment(&fx.swap, &claimer),
        );
        {
            // Tip at height 10, built against the block two after the payment.
            let mut rwtxn = fx.env.write_txn().unwrap();
            fx.state.height.put(&mut rwtxn, &(), &10u32).unwrap();
            fx.state.put_main_anchor(&mut rwtxn, 10, hashes[2]).unwrap();
            rwtxn.commit().unwrap();
        }
        assert_eq!(fx.swap.state, SwapState::Pending, "no local fill observed");
        let tx = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(claimer, 50_000)],
            Some(proof.clone()),
        );
        let filled = FilledTransaction {
            spent_utxos: vec![fx.locked_output.clone()],
            transaction: tx.clone(),
        };
        let rotxn = fx.env.read_txn().unwrap();
        let result = validate_swap_claim(&fx.state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "mempool must accept a proven claim, got {result:?}"
        );

        let stolen = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(Address([4u8; 20]), 50_000)],
            Some(proof),
        );
        let filled = FilledTransaction {
            spent_utxos: vec![fx.locked_output.clone()],
            transaction: stolen.clone(),
        };
        assert!(
            validate_swap_claim(&fx.state, &rotxn, &stolen, &filled).is_err()
        );
    }

    /// A pre-specified swap already marked `ReadyToClaim` by local monitoring.
    fn ready_swap_state() -> (
        temp_dir::TempDir,
        Env,
        State,
        SwapId,
        OutPoint,
        Output,
        Address,
    ) {
        let recipient = Address([6u8; 20]);
        let SwapFixture {
            _dir,
            env,
            state,
            mut swap,
            outpoint,
            locked_output,
        } = swap_fixture(Some(recipient));
        swap.state = SwapState::ReadyToClaim;
        let mut rwtxn = env.write_txn().unwrap();
        state.save_swap(&mut rwtxn, &swap).unwrap();
        rwtxn.commit().unwrap();
        (
            _dir,
            env,
            state,
            swap.id,
            outpoint,
            locked_output,
            recipient,
        )
    }

    /// Set the tip height, so that `validating_height` reports `height`.
    fn set_height(env: &Env, state: &State, height: u32) {
        let mut rwtxn = env.write_txn().unwrap();
        // Validation looks at the *next* block, so store one below.
        state
            .height
            .put(&mut rwtxn, &(), &height.saturating_sub(1))
            .unwrap();
        rwtxn.commit().unwrap();
    }

    /// An open swap (`l2_recipient == None`). `reservation` is an on-chain
    /// `SwapAccept` — `(claimer, accepted_at_height)`.
    fn open_swap_state(
        reservation: Option<(Address, u32)>,
    ) -> (temp_dir::TempDir, Env, State, SwapId, OutPoint, Output) {
        let SwapFixture {
            _dir,
            env,
            state,
            swap,
            outpoint,
            locked_output,
        } = swap_fixture(None);
        if let Some((claimer, height)) = reservation {
            let mut rwtxn = env.write_txn().unwrap();
            state
                .reserve_swap(&mut rwtxn, &swap.id, claimer, height)
                .unwrap();
            rwtxn.commit().unwrap();
        }
        (_dir, env, state, swap.id, outpoint, locked_output)
    }

    fn accept_tx(
        swap_id: SwapId,
        claimer: Address,
        funding_owner: Address,
    ) -> (Transaction, FilledTransaction) {
        let outpoint = OutPoint::Regular {
            txid: Txid([42u8; 32]),
            vout: 0,
        };
        let tx = Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            proof: Default::default(),
            outputs: vec![Output {
                address: funding_owner,
                content: OutputContent::Value(sat(900)),
            }],
            data: TxData::SwapAccept {
                swap_id: swap_id.0,
                l2_claimer_address: claimer,
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![Output {
                address: funding_owner,
                content: OutputContent::Value(sat(1_000)),
            }],
            transaction: tx.clone(),
        };
        (tx, filled)
    }

    /// A reservation is coordination, not entitlement: a claim backed by a
    /// proof that commits to someone other than the reserver is still valid,
    /// because that someone is the one who actually paid.
    #[test]
    fn block_validation_pays_the_prover_even_when_another_reserved() {
        let reserver = Address([7u8; 20]);
        let payer = Address([9u8; 20]);
        let fx = {
            let (dir, env, state, _swap_id, outpoint, locked_output) =
                open_swap_state(Some((reserver, 10)));
            let rotxn = env.read_txn().unwrap();
            let swap =
                state.get_swap(&rotxn, &SwapId([8u8; 32])).unwrap().unwrap();
            drop(rotxn);
            SwapFixture {
                _dir: dir,
                env,
                state,
                swap,
                outpoint,
                locked_output,
            }
        };
        set_height(&fx.env, &fx.state, 12);
        let (proof, hashes) =
            install_payment(&fx.state, &fx.env, &l1_payment(&fx.swap, &payer));
        let to_payer = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(payer, 50_000)],
            Some(proof.clone()),
        );
        assert!(validate_claim(&fx, &to_payer, hashes[2]).is_ok());
        let to_reserver = claim_tx(
            fx.swap.id,
            fx.outpoint,
            None,
            vec![(reserver, 50_000)],
            Some(proof),
        );
        assert!(validate_claim(&fx, &to_reserver, hashes[2]).is_err());
    }

    #[test]
    fn swap_accept_reserves_open_swap() {
        let (_dir, env, state, swap_id, ..) = open_swap_state(None);
        set_height(&env, &state, 10);
        let claimer = Address([7u8; 20]);

        let (tx, filled) = accept_tx(swap_id, claimer, claimer);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(
            &state,
            &rotxn,
            &tx,
            &filled,
            NO_ANCHOR(),
        );
        assert!(result.is_ok(), "valid accept should pass, got {result:?}");
    }

    /// Proof of control: reserving for an address you do not own would let
    /// anyone park every open swap for free.
    #[test]
    fn swap_accept_rejects_claimer_without_matching_input() {
        let (_dir, env, state, swap_id, ..) = open_swap_state(None);
        set_height(&env, &state, 10);
        let claimer = Address([7u8; 20]);
        let someone_else = Address([9u8; 20]);

        let (tx, filled) = accept_tx(swap_id, claimer, someone_else);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(
            &state,
            &rotxn,
            &tx,
            &filled,
            NO_ANCHOR(),
        );
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "accept without an input from the claimer must be rejected, got {result:?}"
        );
    }

    /// First reservation wins while it is live.
    #[test]
    fn swap_accept_rejects_double_reservation() {
        let first = Address([7u8; 20]);
        let (_dir, env, state, swap_id, ..) =
            open_swap_state(Some((first, 10)));
        set_height(&env, &state, 12);
        let second = Address([9u8; 20]);

        let (tx, filled) = accept_tx(swap_id, second, second);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(
            &state,
            &rotxn,
            &tx,
            &filled,
            NO_ANCHOR(),
        );
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "second accept during a live reservation must be rejected, got {result:?}"
        );
    }

    /// ...but a lapsed reservation releases the swap, so a griefer can only
    /// stall the market for one window per fee paid.
    #[test]
    fn swap_accept_allowed_after_reservation_lapses() {
        let first = Address([7u8; 20]);
        let (_dir, env, state, swap_id, ..) =
            open_swap_state(Some((first, 10)));
        let window = ParentChainType::Regtest.accept_expiration_blocks();
        set_height(&env, &state, 10 + window);
        let second = Address([9u8; 20]);

        let (tx, filled) = accept_tx(swap_id, second, second);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(
            &state,
            &rotxn,
            &tx,
            &filled,
            NO_ANCHOR(),
        );
        assert!(
            result.is_ok(),
            "accept after the window should pass, got {result:?}"
        );
    }

    /// Pre-specified swaps already have a recipient; reserving one is
    /// meaningless and must not overwrite it.
    #[test]
    fn swap_accept_rejects_pre_specified_swap() {
        let (_dir, env, state, swap_id, ..) = pre_specified_swap_state();
        set_height(&env, &state, 10);
        let claimer = Address([7u8; 20]);

        let (tx, filled) = accept_tx(swap_id, claimer, claimer);
        let rotxn = env.read_txn().unwrap();
        let result = validate_block_transaction(
            &state,
            &rotxn,
            &tx,
            &filled,
            NO_ANCHOR(),
        );
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "accepting a pre-specified swap must be rejected, got {result:?}"
        );
    }

    /// Build a `SwapCreate` declaring `l2_amount`, escrowing `escrowed` in a
    /// `SwapPending` output and keeping `change` as a regular output, funded by
    /// a single input worth `l2_amount` plus a fee.
    fn swap_create_tx(
        l2_amount: u64,
        escrowed: u64,
        change: u64,
    ) -> (Transaction, FilledTransaction) {
        let sender = Address([12u8; 20]);
        let recipient = Address([13u8; 20]);
        let l1_recipient_address = L1_RECIPIENT.to_string();
        let l1_amount = sat(40_000);
        let swap_id = SwapId::from_l2_to_l1(
            &l1_recipient_address,
            l1_amount,
            &sender,
            Some(&recipient),
        );
        let funding = Output {
            address: sender,
            content: OutputContent::Value(sat(l2_amount + 1_000)),
        };
        let tx = Transaction {
            inputs: vec![(
                OutPoint::Regular {
                    txid: Txid([3u8; 32]),
                    vout: 0,
                },
                [0u8; 32],
            )],
            proof: Default::default(),
            outputs: vec![
                Output {
                    address: recipient,
                    content: OutputContent::SwapPending {
                        value: sat(escrowed),
                        swap_id: swap_id.0,
                    },
                },
                Output {
                    address: sender,
                    content: OutputContent::Value(sat(change)),
                },
            ],
            data: TxData::SwapCreate {
                swap_id: swap_id.0,
                parent_chain: ParentChainType::Regtest,
                l1_txid_bytes: vec![0u8; 32],
                required_confirmations: 1,
                l2_recipient: Some(recipient),
                l2_amount,
                l1_recipient_address,
                l1_amount: l1_amount.to_sat(),
            },
        };
        let filled = FilledTransaction {
            spent_utxos: vec![funding],
            transaction: tx.clone(),
        };
        (tx, filled)
    }

    /// A swap whose L1 leg could never be proven must not be created: the
    /// escrow could only ever come back through expiry. That covers chains
    /// the node holds no headers for, and recipient addresses a proof could
    /// not be matched against.
    #[test]
    fn swap_create_rejects_unprovable_chain_and_bad_recipient() {
        let (_dir, env, state) = test_state();
        let rotxn = env.read_txn().unwrap();

        let (mut tx, _) = swap_create_tx(10_000, 10_000, 500);
        let TxData::SwapCreate { parent_chain, .. } = &mut tx.data else {
            unreachable!()
        };
        *parent_chain = ParentChainType::BCH;
        let filled = FilledTransaction {
            spent_utxos: vec![Output {
                address: Address([12u8; 20]),
                content: OutputContent::Value(sat(11_000)),
            }],
            transaction: tx.clone(),
        };
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(ref msg)) if msg.contains("cannot be proven")),
            "BCH swap must be rejected, got {result:?}"
        );

        let (mut tx, _) = swap_create_tx(10_000, 10_000, 500);
        let TxData::SwapCreate {
            l1_recipient_address,
            swap_id,
            ..
        } = &mut tx.data
        else {
            unreachable!()
        };
        *l1_recipient_address = "not-an-address".to_string();
        *swap_id = SwapId::from_l2_to_l1(
            "not-an-address",
            sat(40_000),
            &Address([12u8; 20]),
            Some(&Address([13u8; 20])),
        )
        .0;
        let filled = FilledTransaction {
            spent_utxos: vec![Output {
                address: Address([12u8; 20]),
                content: OutputContent::Value(sat(11_000)),
            }],
            transaction: tx.clone(),
        };
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(ref msg)) if msg.contains("not a valid")),
            "bad recipient must be rejected, got {result:?}"
        );
    }

    /// A `SwapCreate` that declares an `l2_amount` larger than the value it
    /// actually escrows in `SwapPending` outputs must be rejected. Only those
    /// outputs are locked, so the remainder is change the creator keeps while
    /// the claim is still obliged to pay the recipient the declared amount.
    #[test]
    fn swap_create_rejects_under_escrowed_amount() {
        let (_dir, env, state) = test_state();
        let (tx, filled) = swap_create_tx(10_000, 1_000, 9_000);

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "under-escrowed SwapCreate should be rejected, got {result:?}"
        );
    }

    /// The wallet-shaped `SwapCreate` — the full `l2_amount` in a single
    /// `SwapPending` output plus a change output — must still be accepted.
    #[test]
    fn swap_create_accepts_fully_escrowed_amount() {
        let (_dir, env, state) = test_state();
        let (tx, filled) = swap_create_tx(10_000, 10_000, 500);

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            result.is_ok(),
            "fully escrowed SwapCreate should be accepted, got {result:?}"
        );
    }

    /// `SwapPending` outputs carrying a different swap's id do not count
    /// towards the escrow, since block connection does not lock them for this
    /// swap.
    #[test]
    fn swap_create_rejects_escrow_tagged_with_other_swap() {
        let (_dir, env, state) = test_state();
        let (mut tx, filled) = swap_create_tx(10_000, 10_000, 500);
        tx.outputs[0].content = OutputContent::SwapPending {
            value: sat(10_000),
            swap_id: [42u8; 32],
        };
        let filled = FilledTransaction {
            spent_utxos: filled.spent_utxos,
            transaction: tx.clone(),
        };

        let rotxn = env.read_txn().unwrap();
        let result = validate_swap_create(&state, &rotxn, &tx, &filled);
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "escrow tagged with another swap should not count, got {result:?}"
        );
    }

    /// Reversing the block that created a swap must always succeed, even when
    /// local L1 monitoring has already advanced the swap past `Pending` (e.g. to
    /// `ReadyToClaim`). `disconnect_tip` deletes the swap via
    /// `delete_swap_unchecked`, so that helper must never refuse based on state;
    /// otherwise a sidechain reorg that removes the creating block aborts and the
    /// node is wedged on the losing branch.
    #[test]
    fn delete_swap_unchecked_deletes_ready_to_claim_swap() {
        let (_dir, env, state, swap_id, ..) = ready_swap_state();

        let mut rwtxn = env.write_txn().unwrap();
        let result = state.delete_swap_unchecked(&mut rwtxn, &swap_id);
        assert!(
            result.is_ok(),
            "rollback deletion of a ReadyToClaim swap must succeed, got {result:?}"
        );
        assert!(
            state.get_swap(&rwtxn, &swap_id).unwrap().is_none(),
            "swap should be gone after rollback deletion"
        );
    }

    /// Deleting a swap record must never leave its escrow locked: a locked
    /// output whose swap is gone cannot be spent through the mempool and its
    /// claim is rejected by consensus, so the value would be stranded.
    #[test]
    fn delete_swap_unchecked_unlocks_outputs_of_existing_swap() {
        let (_dir, env, state, swap_id, outpoint, ..) = ready_swap_state();

        let mut rwtxn = env.write_txn().unwrap();
        assert_eq!(
            state.is_output_locked_to_swap(&rwtxn, &outpoint).unwrap(),
            Some(swap_id),
            "precondition: escrow is locked"
        );
        state.delete_swap_unchecked(&mut rwtxn, &swap_id).unwrap();
        assert_eq!(
            state.is_output_locked_to_swap(&rwtxn, &outpoint).unwrap(),
            None,
            "deleting the swap must release its locks"
        );
    }

    /// An output locked to a swap that does not exist is reported with the
    /// typed `OrphanedLock` error, which `Node::submit_transaction` turns
    /// into a cleanup-and-retry. A lock backed by a live swap is a plain
    /// rejection.
    #[test]
    fn spending_orphaned_lock_is_reported_as_orphaned() {
        let (_dir, env, state, swap_id, outpoint, ..) = ready_swap_state();
        let orphan_outpoint = OutPoint::Regular {
            txid: Txid([3u8; 32]),
            vout: 0,
        };
        let missing_swap = SwapId([99u8; 32]);
        let mut rwtxn = env.write_txn().unwrap();
        state
            .lock_output_to_swap(&mut rwtxn, &orphan_outpoint, &missing_swap)
            .unwrap();
        rwtxn.commit().unwrap();

        let spend = |outpoint| Transaction {
            inputs: vec![(outpoint, [0u8; 32])],
            ..Default::default()
        };
        let rotxn = env.read_txn().unwrap();
        let result =
            validate_no_locked_outputs(&state, &rotxn, &spend(orphan_outpoint));
        assert!(
            matches!(
                result,
                Err(Error::OrphanedLock { outpoint, swap_id })
                    if outpoint == orphan_outpoint && swap_id == missing_swap
            ),
            "expected OrphanedLock, got {result:?}"
        );
        let result =
            validate_no_locked_outputs(&state, &rotxn, &spend(outpoint));
        assert!(
            matches!(result, Err(Error::InvalidTransaction(_))),
            "a live lock is an ordinary rejection, got {result:?}"
        );
        let _ = swap_id;

        // Cleanup removes exactly the orphaned lock.
        let mut rwtxn = env.write_txn().unwrap();
        assert_eq!(state.cleanup_orphaned_locks(&mut rwtxn).unwrap(), 1);
        assert_eq!(
            state
                .is_output_locked_to_swap(&rwtxn, &orphan_outpoint)
                .unwrap(),
            None
        );
        assert_eq!(
            state.is_output_locked_to_swap(&rwtxn, &outpoint).unwrap(),
            Some(swap_id)
        );
    }
}
