#![feature(impl_trait_in_assoc_type)]
#![feature(trait_alias)]

/// Install the process-wide rustls crypto provider.
///
/// Call this once, first thing in `main`, before anything can build a TLS
/// config.
///
/// `lib` asks rustls for the `ring` backend and nothing here wants any other.
/// But Cargo unifies features across a build: compiling the workspace with
/// `--all-targets` also compiles `integration_tests`, whose dependency chain
/// (`bip300301_enforcer_lib` -> `bdk_electrum` -> `electrum-client`) turns on
/// rustls's `aws-lc-rs` backend. rustls then sees two candidate providers,
/// refuses to guess, and *panics* the first time a config is built.
///
/// For the node that first time is `net::make_server_endpoint`, so the failure
/// looks like a healthy build that dies on startup with "Could not
/// automatically determine the process-level CryptoProvider" — and only under
/// some build commands, which makes it a memorable afternoon. Choosing here
/// makes the binary independent of who else is in the build graph.
///
/// Idempotent: installing twice, or racing another installer, returns `Err`
/// and is deliberately ignored.
pub fn install_default_crypto_provider() {
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        tracing::debug!("rustls crypto provider was already installed");
    }
}

pub mod archive;
pub mod authorization;
pub mod htlc;
pub mod mempool;
pub mod miner;
pub mod net;
pub mod node;
pub mod parent_chain_rpc;
pub mod state;
pub mod types;
pub mod util;
pub mod wallet;

pub use heed;
