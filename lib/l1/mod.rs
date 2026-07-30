//! L1 parent-chain plumbing shared by the node, the RPC server, the CLI and the
//! GUI.
//!
//! [`config`] defines what an endpoint is and where it is stored, [`identity`]
//! decides whether an endpoint is serving the chain it was configured for, and
//! [`registry`] holds the live clients and their health. Later phases of
//! `docs/PARENT_CHAIN_ROADMAP.md` add the single swap observer alongside them.

pub mod config;
pub mod identity;
pub mod registry;
pub mod status;

pub use config::{L1Auth, L1ChainConfig, L1ConfigFile};
pub use registry::L1Registry;
pub use status::L1ChainHealth;
