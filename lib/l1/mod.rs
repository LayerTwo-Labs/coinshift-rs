//! L1 parent-chain plumbing shared by the node, the RPC server, the CLI and the
//! GUI.
//!
//! Today this is configuration only. Later phases of
//! `docs/PARENT_CHAIN_ROADMAP.md` add chain identity probing, a per-chain health
//! registry, and the single swap observer alongside it.

pub mod config;

pub use config::{L1Auth, L1ChainConfig, L1ConfigFile};
