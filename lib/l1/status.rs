//! Per-chain L1 health.

use serde::{Deserialize, Serialize};

use crate::types::ParentChainType;

/// Why a parent chain can or cannot currently be used.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum L1ChainHealth {
    /// No entry in the config file. Swaps on this chain stay `Pending`.
    Unconfigured,
    /// Configured but switched off by the operator.
    Disabled,
    /// Configured; the first identity probe has not finished yet.
    Probing,
    /// Configured but the endpoint did not answer.
    ///
    /// Transient: swap detection pauses and resumes when the node returns.
    Unreachable {
        error: String,
        consecutive_failures: u32,
    },
    /// The endpoint answered, but it is serving a different network.
    ///
    /// **Not** transient, and never usable: accepting L1 payments from the
    /// wrong chain would corrupt swap state.
    WrongChain { reason: String },
    /// Reachable and serving the expected network.
    Healthy {
        chain_name: String,
        block_height: u64,
    },
}

impl L1ChainHealth {
    /// Whether a client for this chain may be handed out.
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }

    /// Whether the operator asked for this chain at all.
    pub fn is_configured(&self) -> bool {
        !matches!(self, Self::Unconfigured)
    }

    /// Whether swaps may be created against this chain.
    ///
    /// Only [`Self::WrongChain`] is refused: accepting an L1 payment from the
    /// wrong network would corrupt swap state, and no correct swap could ever
    /// be filled there.
    ///
    /// Everything else is allowed, `Unconfigured` included. A swap on a chain
    /// with no endpoint simply is not detected automatically — its creator
    /// fills it with `update_swap_l1_txid` instead, which is a supported and
    /// tested workflow (see `integration_tests/l1_rpc_dependency.rs`, and the
    /// "or the user updates via update_swap_l1_txid" behaviour documented in
    /// `docs/COINSHIFT_HOW_IT_WORKS.md`). Refusing here would delete it.
    pub fn allows_swap_creation(&self) -> bool {
        !matches!(self, Self::WrongChain { .. })
    }

    /// Short human-readable summary, for the GUI and CLI.
    pub fn summary(&self, chain: ParentChainType) -> String {
        match self {
            Self::Unconfigured => {
                format!("{} is not configured", chain.ticker())
            }
            Self::Disabled => format!("{} is disabled", chain.ticker()),
            Self::Probing => format!("checking {}…", chain.ticker()),
            Self::Unreachable {
                error,
                consecutive_failures,
            } => format!(
                "{} unreachable ({consecutive_failures} failed checks): {error}",
                chain.ticker()
            ),
            Self::WrongChain { reason } => {
                format!("{} misconfigured: {reason}", chain.ticker())
            }
            Self::Healthy {
                chain_name,
                block_height,
            } => format!(
                "{} connected to {chain_name} at height {block_height}",
                chain.ticker()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unreachable() -> L1ChainHealth {
        L1ChainHealth::Unreachable {
            error: "connection refused".to_string(),
            consecutive_failures: 3,
        }
    }

    fn healthy() -> L1ChainHealth {
        L1ChainHealth::Healthy {
            chain_name: "regtest".to_string(),
            block_height: 101,
        }
    }

    #[test]
    fn only_a_wrong_network_blocks_swap_creation() {
        // An unconfigured chain must still accept swaps: they are filled
        // manually with update_swap_l1_txid, which is the workflow
        // integration_tests/l1_rpc_dependency.rs exercises. Refusing here would
        // delete that workflow, and every swap integration test with it.
        assert!(L1ChainHealth::Unconfigured.allows_swap_creation());
        assert!(L1ChainHealth::Disabled.allows_swap_creation());
        assert!(L1ChainHealth::Probing.allows_swap_creation());
        assert!(unreachable().allows_swap_creation());
        assert!(healthy().allows_swap_creation());

        // A node on the wrong network is the one case that must be refused:
        // accepting its payments would corrupt swap state.
        assert!(
            !L1ChainHealth::WrongChain {
                reason: "reports main, expected signet".to_string(),
            }
            .allows_swap_creation()
        );
    }

    #[test]
    fn only_healthy_chains_are_usable_for_detection() {
        assert!(healthy().is_healthy());
        for health in [
            L1ChainHealth::Unconfigured,
            L1ChainHealth::Disabled,
            L1ChainHealth::Probing,
            unreachable(),
            L1ChainHealth::WrongChain {
                reason: "x".to_string(),
            },
        ] {
            assert!(!health.is_healthy(), "{health:?}");
        }
    }

    #[test]
    fn unconfigured_is_the_only_state_that_is_not_configured() {
        assert!(!L1ChainHealth::Unconfigured.is_configured());
        assert!(L1ChainHealth::Disabled.is_configured());
        assert!(healthy().is_configured());
    }
}
