//! Tracking whether the BIP300 enforcer is reachable.
//!
//! # Why this replaces a boolean
//!
//! `App` used to carry `mainchain_reachable: Arc<AtomicBool>`, documented as
//! gating mining. It was written by the L1 sync task and **never read anywhere**
//! — it carried `#[allow(dead_code)]` and had done since it was added. Mining
//! discovered unreachability by failing a live call instead, which is the
//! correct behaviour; the flag just made it look like there was a check.
//!
//! What is actually needed is not a gate but a *report*: something the operator
//! and the GUI can consult to see whether the node is connected, and something
//! that can drive reconnection. That is what this is. It is deliberately not
//! consulted before mining — a cached value must never be able to refuse an
//! operation that would have succeeded.

use std::{sync::Arc, time::Duration};

use parking_lot::RwLock;

/// Backoff bounds for reconnection attempts.
pub const RECONNECT_MIN: Duration = Duration::from_secs(1);
pub const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Connection state of the enforcer's gRPC endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MainchainState {
    /// Never yet reached, or lost and being retried.
    Connecting { attempts: u32, last_error: String },
    /// Answering.
    Connected,
}

impl MainchainState {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

/// Shared, observable connection state for the enforcer.
#[derive(Clone, Debug)]
pub struct MainchainMonitor {
    state: Arc<RwLock<MainchainState>>,
    /// Whether the enforcer's optional wallet service was present when last
    /// checked. Without it there is no miner, so no mining or deposits.
    wallet_service: Arc<RwLock<bool>>,
}

impl MainchainMonitor {
    /// A monitor that has not yet reached the enforcer.
    pub fn new(initial_error: impl Into<String>) -> Self {
        Self {
            state: Arc::new(RwLock::new(MainchainState::Connecting {
                attempts: 0,
                last_error: initial_error.into(),
            })),
            wallet_service: Arc::new(RwLock::new(false)),
        }
    }

    /// A monitor that reached the enforcer during startup.
    pub fn connected(wallet_service: bool) -> Self {
        Self {
            state: Arc::new(RwLock::new(MainchainState::Connected)),
            wallet_service: Arc::new(RwLock::new(wallet_service)),
        }
    }

    pub fn state(&self) -> MainchainState {
        self.state.read().clone()
    }

    /// Whether the enforcer offers the wallet service, and so whether this node
    /// can mine or deposit.
    pub fn has_wallet_service(&self) -> bool {
        *self.wallet_service.read()
    }

    pub fn set_wallet_service(&self, present: bool) {
        *self.wallet_service.write() = present;
    }

    /// Record a successful call, logging only on an actual transition.
    pub fn record_connected(&self) {
        let mut state = self.state.write();
        if !state.is_connected() {
            tracing::info!("Mainchain enforcer is reachable");
            *state = MainchainState::Connected;
        }
    }

    /// Record a failed call, returning how long to wait before retrying.
    pub fn record_failure(&self, error: impl Into<String>) -> Duration {
        let error = error.into();
        let mut state = self.state.write();
        let attempts = match &*state {
            MainchainState::Connected => {
                tracing::warn!(
                    %error,
                    "Lost contact with the mainchain enforcer; mining and \
                     deposits are unavailable and block sync will stall until \
                     it returns"
                );
                1
            }
            MainchainState::Connecting { attempts, .. } => {
                attempts.saturating_add(1)
            }
        };
        *state = MainchainState::Connecting {
            attempts,
            last_error: error,
        };
        backoff(attempts)
    }
}

/// Exponential backoff from [`RECONNECT_MIN`] to [`RECONNECT_MAX`].
fn backoff(attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1).min(16);
    RECONNECT_MIN
        .saturating_mul(2u32.saturating_pow(exponent))
        .min(RECONNECT_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_flattens() {
        assert_eq!(backoff(1), RECONNECT_MIN);
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        // Never longer than the cap, and never overflows however long the
        // enforcer stays down.
        assert_eq!(backoff(100), RECONNECT_MAX);
        assert_eq!(backoff(u32::MAX), RECONNECT_MAX);
    }

    #[test]
    fn failures_accumulate_and_success_resets_them() {
        let monitor = MainchainMonitor::new("not tried yet");
        assert!(!monitor.state().is_connected());

        assert_eq!(monitor.record_failure("refused"), RECONNECT_MIN);
        assert_eq!(monitor.record_failure("refused"), Duration::from_secs(2));
        match monitor.state() {
            MainchainState::Connecting {
                attempts,
                last_error,
            } => {
                assert_eq!(attempts, 2);
                assert_eq!(last_error, "refused");
            }
            other => panic!("expected Connecting, got {other:?}"),
        }

        monitor.record_connected();
        assert!(monitor.state().is_connected());
        // A later failure starts the backoff over rather than resuming it.
        assert_eq!(monitor.record_failure("refused again"), RECONNECT_MIN);
    }

    #[test]
    fn wallet_service_can_appear_after_startup() {
        // The enforcer may come up without its wallet service and gain it
        // later; the node must be able to notice.
        let monitor = MainchainMonitor::connected(false);
        assert!(!monitor.has_wallet_service());
        monitor.set_wallet_service(true);
        assert!(monitor.has_wallet_service());
    }
}
