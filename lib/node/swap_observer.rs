//! The single place Coinshift watches parent chains for swap fills.
//!
//! # Why this is its own task
//!
//! Detection used to run inside `connect_two_way_peg_data`, which means it made
//! blocking HTTP calls **while holding the LMDB write transaction** — one
//! `listunspent` plus one `getrawtransaction` per candidate, per pending swap,
//! per block. Against a localhost node that is milliseconds; against a remote,
//! rate-limited endpoint it is seconds, during which nothing else in the node
//! can take the write lock. Three further copies of the same polling logic
//! existed in the headless task and in two GUI views, one of which blocked the
//! egui thread on a `join()`.
//!
//! Doing it here costs nothing in correctness: L1 verification is explicitly
//! **not part of consensus** (see `state::swap::validate_swap_claim_consensus`),
//! so a node that has not yet seen a payment still accepts a block that claims
//! it. Detection only needs a tip to stamp on the swap, which it reads when it
//! applies the result.
//!
//! # Shape of a round
//!
//! 1. Snapshot the work list under a read transaction, then drop it.
//! 2. Do all network I/O with **no transaction held**, chains concurrently.
//! 3. Apply results in one short write transaction, re-reading each swap and
//!    only writing if it is still in the state the snapshot saw.
//!
//! Step 3 matters because block connect also writes swap rows (claims and
//! expiries) on a single-writer environment. Re-reading before writing means a
//! result computed against a stale snapshot is dropped rather than clobbering a
//! newer state.

use std::{sync::Arc, time::Duration};

use futures::future;

use crate::{
    l1::L1Registry,
    parent_chain::PaymentQuery,
    state::State,
    types::{Swap, SwapId, SwapState, SwapTxId},
};

/// How often to look for new fills.
pub const OBSERVE_INTERVAL: Duration = Duration::from_secs(10);

/// A swap awaiting an L1 payment, captured outside any transaction.
#[derive(Clone, Debug)]
struct PendingSwap {
    id: SwapId,
    query: PaymentQuery,
    chain: crate::types::ParentChainType,
    /// State when the snapshot was taken, used to detect concurrent writes.
    observed_state: SwapState,
    l1_txid: SwapTxId,
}

impl PendingSwap {
    fn from_swap(swap: &Swap) -> Self {
        Self {
            id: swap.id,
            query: PaymentQuery::for_swap(swap),
            chain: swap.parent_chain,
            observed_state: swap.state.clone(),
            l1_txid: swap.l1_txid.clone(),
        }
    }

    /// Whether no L1 transaction has been recorded for this swap yet.
    fn awaiting_first_payment(&self) -> bool {
        matches!(self.l1_txid, SwapTxId::Hash32(hash) if hash == [0u8; 32])
            || matches!(self.l1_txid, SwapTxId::Hash(ref bytes)
                if bytes.is_empty() || bytes.iter().all(|byte| *byte == 0))
    }
}

/// What a round decided about one swap.
struct Detection {
    swap: PendingSwap,
    txid: SwapTxId,
    confirmations: u32,
}

/// Watches every healthy parent chain for payments filling pending swaps.
pub struct SwapObserver {
    state: State,
    env: sneed::Env,
    l1: Arc<L1Registry>,
}

impl SwapObserver {
    pub fn new(state: State, env: sneed::Env, l1: Arc<L1Registry>) -> Self {
        Self { state, env, l1 }
    }

    /// Swaps that could still be filled, read and released immediately.
    fn snapshot(&self) -> Result<Vec<PendingSwap>, crate::state::Error> {
        let rotxn = self.env.read_txn().map_err(sneed::EnvError::from)?;
        let swaps = self.state.load_all_swaps(&rotxn)?;
        drop(rotxn);
        Ok(swaps
            .iter()
            .filter(|swap| {
                matches!(
                    swap.state,
                    SwapState::Pending | SwapState::WaitingConfirmations(..)
                )
            })
            .map(PendingSwap::from_swap)
            .collect())
    }

    /// Look for a payment filling `pending`, holding no transaction.
    async fn observe(&self, pending: PendingSwap) -> Option<Detection> {
        // Only chains the registry currently vouches for are consulted.
        let client = self.l1.verified_client(pending.chain)?;

        if pending.awaiting_first_payment() {
            let payments = client
                .find_payments(&pending.query)
                .await
                .inspect_err(|err| {
                    tracing::warn!(
                        swap_id = %pending.id,
                        chain = ?pending.chain,
                        error = %err,
                        "Failed to query L1 for swap; it stays pending until the endpoint recovers"
                    );
                })
                .ok()?;
            let payment = payments
                .into_iter()
                .find(|payment| payment.is_acceptable_for(&pending.query))?;
            Some(Detection {
                confirmations: payment.confirmations,
                txid: payment.txid,
                swap: pending,
            })
        } else {
            // Already detected: just refresh the confirmation count.
            let payment = client
                .get_payment(&pending.l1_txid, &pending.query)
                .await
                .inspect_err(|err| {
                    tracing::debug!(
                        swap_id = %pending.id,
                        error = %err,
                        "Failed to refresh confirmations (normal while the endpoint is down)"
                    );
                })
                .ok()??;
            Some(Detection {
                confirmations: payment.confirmations,
                txid: pending.l1_txid.clone(),
                swap: pending,
            })
        }
    }

    /// Apply a round's detections in one short write transaction.
    fn apply(
        &self,
        detections: Vec<Detection>,
    ) -> Result<usize, crate::state::Error> {
        if detections.is_empty() {
            return Ok(0);
        }
        let mut rwtxn = self.env.write_txn().map_err(sneed::EnvError::from)?;
        let (Some(block_hash), Some(block_height)) = (
            self.state.try_get_tip(&rwtxn)?,
            self.state.try_get_height(&rwtxn)?,
        ) else {
            // No tip yet: nothing to stamp a detection against.
            return Ok(0);
        };

        let mut applied = 0;
        for detection in detections {
            let Some(mut swap) =
                self.state.get_swap(&rwtxn, &detection.swap.id)?
            else {
                continue;
            };
            // Compare-and-set: a claim or expiry may have landed while we were
            // off doing network I/O, and it wins.
            if swap.state != detection.swap.observed_state {
                tracing::debug!(
                    swap_id = %swap.id,
                    "Swap changed while observing; discarding stale result"
                );
                continue;
            }

            let is_new = detection.swap.awaiting_first_payment();
            if is_new {
                // One L1 payment must not fill two swaps.
                if let Some(existing) = self.state.get_swap_by_l1_txid(
                    &rwtxn,
                    &swap.parent_chain,
                    &detection.txid,
                )? && existing.id != swap.id
                {
                    tracing::info!(
                        swap_id = %swap.id,
                        existing_swap_id = %existing.id,
                        "Rejecting L1 tx already associated with another swap"
                    );
                    continue;
                }
                swap.update_l1_txid(detection.txid.clone());
                swap.set_l1_txid_validation_block(block_hash, block_height);
                tracing::info!(
                    swap_id = %swap.id,
                    chain = ?swap.parent_chain,
                    confirmations = detection.confirmations,
                    "Detected L1 transaction for swap"
                );
            } else if detection.confirmations
                <= swap.state.current_confirmations().unwrap_or(0)
            {
                // Confirmations never go backwards.
                continue;
            }

            swap.state =
                if detection.confirmations >= swap.required_confirmations {
                    SwapState::ReadyToClaim
                } else {
                    SwapState::WaitingConfirmations(
                        detection.confirmations,
                        swap.required_confirmations,
                    )
                };
            self.state.save_swap(&mut rwtxn, &swap)?;
            applied += 1;
        }

        if applied > 0 {
            rwtxn.commit().map_err(sneed::RwTxnError::from)?;
        } else {
            drop(rwtxn);
        }
        Ok(applied)
    }

    /// One full observation round.
    pub async fn run_once(&self) -> Result<usize, crate::state::Error> {
        let pending = self.snapshot()?;
        if pending.is_empty() {
            return Ok(0);
        }
        // Chains are independent, so a slow endpoint delays only its own swaps.
        let detections: Vec<Detection> = future::join_all(
            pending.into_iter().map(|swap| self.observe(swap)),
        )
        .await
        .into_iter()
        .flatten()
        .collect();
        self.apply(detections)
    }

    /// Observe forever, absorbing every failure.
    pub async fn run(self) {
        loop {
            tokio::time::sleep(OBSERVE_INTERVAL).await;
            match self.run_once().await {
                Ok(0) => {}
                Ok(applied) => {
                    tracing::debug!(applied, "Updated swaps from L1")
                }
                Err(err) => {
                    // Never fatal: one bad round must not stop observation.
                    tracing::warn!(%err, "Swap observation round failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sneed::Env;

    use super::*;
    use crate::{
        parent_chain::{
            L1Payment, ParentChainClient,
            mock::{MockBehaviour, MockParentChainClient},
        },
        types::{Address, BlockHash, ParentChainType, SwapDirection},
    };

    const CHAIN: ParentChainType = ParentChainType::Regtest;
    const REQUIRED_CONFIRMATIONS: u32 = 3;

    fn test_env() -> (temp_dir::TempDir, Env, State) {
        let dir = temp_dir::TempDir::new().unwrap();
        let mut opts = heed::EnvOpenOptions::new();
        opts.map_size(10 * 1024 * 1024).max_dbs(State::NUM_DBS);
        let env = unsafe { Env::open(&opts, dir.path()) }.unwrap();
        let state = State::new(&env).unwrap();
        // The observer stamps detections with the tip, so there must be one.
        let mut rwtxn = env.write_txn().unwrap();
        state
            .tip
            .put(&mut rwtxn, &(), &BlockHash([7u8; 32]))
            .unwrap();
        state.height.put(&mut rwtxn, &(), &10u32).unwrap();
        rwtxn.commit().unwrap();
        (dir, env, state)
    }

    fn pending_swap(id: u8) -> Swap {
        Swap::new(
            SwapId([id; 32]),
            SwapDirection::L2ToL1,
            CHAIN,
            SwapTxId::Hash32([0u8; 32]),
            Some(REQUIRED_CONFIRMATIONS),
            Some(Address([3u8; 20])),
            bitcoin::Amount::from_sat(50_000),
            "rbtc-recipient".to_string(),
            bitcoin::Amount::from_sat(40_000),
            0,
            None,
            Some(Address([5u8; 20])),
        )
    }

    fn save(state: &State, env: &Env, swap: &Swap) {
        let mut rwtxn = env.write_txn().unwrap();
        state.save_swap(&mut rwtxn, swap).unwrap();
        rwtxn.commit().unwrap();
    }

    fn get(state: &State, env: &Env, id: &SwapId) -> Swap {
        let rotxn = env.read_txn().unwrap();
        state.get_swap(&rotxn, id).unwrap().unwrap()
    }

    fn paying(
        swap: &Swap,
        txid: SwapTxId,
        confirmations: u32,
        age: u64,
    ) -> MockBehaviour {
        MockBehaviour::Payments(vec![L1Payment {
            txid_display: txid.display_for_chain(CHAIN),
            txid,
            amount: swap.l1_amount.to_sat(),
            matches_query: true,
            sender: Some("mock-sender".to_string()),
            confirmations,
            age,
            included: true,
            height: Some(1),
        }])
    }

    fn observer(
        state: &State,
        env: &Env,
        behaviour: MockBehaviour,
    ) -> SwapObserver {
        let client: Arc<dyn ParentChainClient> =
            Arc::new(MockParentChainClient::new(CHAIN, behaviour));
        SwapObserver::new(
            state.clone(),
            env.clone(),
            Arc::new(L1Registry::with_verified_client(CHAIN, client)),
        )
    }

    #[tokio::test]
    async fn a_shallow_payment_is_detected_and_tracked() {
        let (_dir, env, state) = test_env();
        let swap = pending_swap(1);
        save(&state, &env, &swap);

        let txid = SwapTxId::Hash32([0xab; 32]);
        let applied = observer(&state, &env, paying(&swap, txid.clone(), 1, 1))
            .run_once()
            .await
            .unwrap();

        assert_eq!(applied, 1);
        let updated = get(&state, &env, &swap.id);
        assert_eq!(updated.l1_txid, txid);
        assert_eq!(
            updated.state,
            SwapState::WaitingConfirmations(1, REQUIRED_CONFIRMATIONS)
        );
        assert_eq!(
            updated.l1_txid_validated_at_height,
            Some(10),
            "detection records the tip it was observed against"
        );
    }

    #[tokio::test]
    async fn a_deep_enough_payment_becomes_claimable() {
        let (_dir, env, state) = test_env();
        let swap = pending_swap(2);
        save(&state, &env, &swap);

        observer(
            &state,
            &env,
            paying(
                &swap,
                SwapTxId::Hash32([0xcd; 32]),
                REQUIRED_CONFIRMATIONS,
                1,
            ),
        )
        .run_once()
        .await
        .unwrap();
        assert_eq!(get(&state, &env, &swap.id).state, SwapState::ReadyToClaim);
    }

    #[tokio::test]
    async fn a_payment_older_than_max_age_is_rejected() {
        let (_dir, env, state) = test_env();
        let swap = pending_swap(3);
        save(&state, &env, &swap);

        // Deeply confirmed but far too old: the guard against reusing an
        // unrelated historical transaction.
        let too_old = u64::from(CHAIN.max_l1_tx_age()) + 1;
        observer(
            &state,
            &env,
            paying(&swap, SwapTxId::Hash32([0xef; 32]), 1_000, too_old),
        )
        .run_once()
        .await
        .unwrap();
        let updated = get(&state, &env, &swap.id);
        assert_eq!(updated.state, SwapState::Pending);
        assert_eq!(updated.l1_txid, SwapTxId::Hash32([0u8; 32]));
    }

    #[tokio::test]
    async fn an_l1_txid_already_used_by_another_swap_is_rejected() {
        let (_dir, env, state) = test_env();
        let shared = SwapTxId::Hash32([0x11; 32]);

        let mut taken = pending_swap(4);
        taken.l1_txid = shared.clone();
        taken.state = SwapState::ReadyToClaim; // terminal, so not observed
        save(&state, &env, &taken);

        let swap = pending_swap(5);
        save(&state, &env, &swap);

        observer(
            &state,
            &env,
            paying(&swap, shared, REQUIRED_CONFIRMATIONS, 1),
        )
        .run_once()
        .await
        .unwrap();
        assert_eq!(
            get(&state, &env, &swap.id).state,
            SwapState::Pending,
            "one L1 payment must not fill two swaps"
        );
    }

    #[tokio::test]
    async fn confirmations_never_go_backwards() {
        let (_dir, env, state) = test_env();
        let txid = SwapTxId::Hash32([0x22; 32]);
        let mut swap = pending_swap(6);
        swap.l1_txid = txid.clone();
        swap.state = SwapState::WaitingConfirmations(5, REQUIRED_CONFIRMATIONS);
        save(&state, &env, &swap);

        observer(&state, &env, paying(&swap, txid, 2, 1))
            .run_once()
            .await
            .unwrap();
        assert_eq!(
            get(&state, &env, &swap.id).state,
            SwapState::WaitingConfirmations(5, REQUIRED_CONFIRMATIONS)
        );
    }

    #[tokio::test]
    async fn an_unreachable_chain_leaves_the_swap_pending() {
        let (_dir, env, state) = test_env();
        let swap = pending_swap(7);
        save(&state, &env, &swap);

        // An endpoint error must be absorbed, never propagated.
        let applied = observer(&state, &env, MockBehaviour::Unreachable)
            .run_once()
            .await
            .unwrap();
        assert_eq!(applied, 0);
        assert_eq!(get(&state, &env, &swap.id).state, SwapState::Pending);
    }

    #[tokio::test]
    async fn without_a_configured_chain_the_swap_stays_pending() {
        // The invariant the l1_rpc_dependency integration test asserts, covered
        // here without standing up a node.
        let (_dir, env, state) = test_env();
        let swap = pending_swap(8);
        save(&state, &env, &swap);

        let observer = SwapObserver::new(
            state.clone(),
            env.clone(),
            Arc::new(L1Registry::new(None)),
        );
        assert_eq!(observer.run_once().await.unwrap(), 0);
        let updated = get(&state, &env, &swap.id);
        assert_eq!(updated.state, SwapState::Pending);
        assert_eq!(updated.l1_txid, SwapTxId::Hash32([0u8; 32]));
    }

    #[tokio::test]
    async fn a_payment_for_a_different_amount_is_ignored() {
        let (_dir, env, state) = test_env();
        let swap = pending_swap(9);
        save(&state, &env, &swap);

        let mut wrong = pending_swap(9);
        wrong.l1_amount =
            bitcoin::Amount::from_sat(swap.l1_amount.to_sat() + 1);
        observer(
            &state,
            &env,
            paying(&wrong, SwapTxId::Hash32([0x33; 32]), 5, 1),
        )
        .run_once()
        .await
        .unwrap();
        assert_eq!(get(&state, &env, &swap.id).state, SwapState::Pending);
    }

    #[tokio::test]
    async fn a_result_computed_against_a_stale_snapshot_is_discarded() {
        // The reason this phase is riskier than the others: block connect can
        // write a swap row while the observer is off doing network I/O. The
        // observer must lose that race rather than overwrite a claim or expiry.
        let (_dir, env, state) = test_env();
        let swap = pending_swap(10);
        save(&state, &env, &swap);

        let observer = observer(
            &state,
            &env,
            paying(&swap, SwapTxId::Hash32([0x44; 32]), 1, 1),
        );
        let pending = observer.snapshot().unwrap();
        assert_eq!(pending.len(), 1);

        // Simulate a claim landing between the snapshot and the write.
        let mut claimed = swap.clone();
        claimed.state = SwapState::Completed;
        save(&state, &env, &claimed);

        let detections: Vec<Detection> =
            future::join_all(pending.into_iter().map(|p| observer.observe(p)))
                .await
                .into_iter()
                .flatten()
                .collect();
        assert_eq!(detections.len(), 1, "the payment was found");
        assert_eq!(
            observer.apply(detections).unwrap(),
            0,
            "but it must not overwrite the newer state"
        );
        assert_eq!(get(&state, &env, &swap.id).state, SwapState::Completed);
    }
}
