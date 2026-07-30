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
    /// `Unreachable` is allowed: the node being briefly down is no reason to
    /// refuse a swap that will be filled minutes later. A chain that is
    /// unconfigured, switched off, or serving the wrong network is not, since
    /// no such swap could ever be detected.
    pub fn allows_swap_creation(&self) -> bool {
        matches!(
            self,
            Self::Probing | Self::Unreachable { .. } | Self::Healthy { .. }
        )
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
