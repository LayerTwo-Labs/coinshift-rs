//! L1 config allowlist and startup validation.
//!
//! Transitional module. The RPC client that used to live here now sits behind
//! [`crate::parent_chain::ParentChainClient`]; what remains is the closed
//! allowlist of predefined endpoints and the startup check built on it, both of
//! which Phase 3 of `docs/PARENT_CHAIN_ROADMAP.md` replaces with chain-identity
//! probing and a per-chain health registry.
//!
//! The client is re-exported under its old name so existing call sites keep
//! working for one release.

use std::path::Path;

use crate::{
    l1::config::{L1ChainConfig, L1ConfigFile},
    parent_chain::BitcoinCoreClient,
    types::ParentChainType,
};

pub use crate::parent_chain::{
    Error,
    bitcoin_core::{ScriptPubKey, TransactionInfo, Vin, Vout},
};

/// The Bitcoin Core client, under its historical name.
pub type ParentChainRpcClient = BitcoinCoreClient;

/// Predefined L1 configs that Coinshift supports. Users may only use these;
/// adding new nodes requires a new Coinshift release.
pub fn supported_l1_configs() -> Vec<(ParentChainType, L1ChainConfig)> {
    vec![
        (
            ParentChainType::Signet,
            L1ChainConfig::basic("http://localhost:38332", "user", "password"),
        ),
        (
            ParentChainType::BCH,
            L1ChainConfig::basic(
                "http://173.230.135.236:28332",
                "user",
                "password",
            ),
        ),
    ]
}

/// Parent chain types that are allowed for L1 config (and swap creation).
pub fn supported_l1_parent_chain_types() -> &'static [ParentChainType] {
    use ParentChainType::{BCH, Signet};
    &[Signet, BCH]
}

/// Detect whether the node at the given config is Bitcoin Signet or Bitcoin Cash testnet4
/// by calling getblockchaininfo and checking the "chain" field.
/// Returns the detected chain type and the raw "chain" string from the node.
pub fn detect_chain_type(
    config: &L1ChainConfig,
) -> Result<(ParentChainType, String), Error> {
    // The chain passed here only selects decimals and identity handling, neither
    // of which affects reading the raw chain name.
    let client =
        BitcoinCoreClient::new(ParentChainType::Signet, config.clone());
    let chain = client.get_blockchain_chain_name()?;
    let detected = match chain.as_str() {
        "signet" => ParentChainType::Signet,
        "testnet4" | "test4" => ParentChainType::BCH,
        _ => {
            return Err(Error::ChainMismatch {
                expected: ParentChainType::Signet, // arbitrary for this error
                chain: chain.clone(),
            });
        }
    };
    Ok((detected, chain))
}

/// Check that the given (parent_chain, config) is one of the supported predefined configs
/// (exact match on url and credentials).
///
/// Only the endpoint and its credentials are compared; the local knobs
/// (`enabled`, timeouts, poll interval) are the operator's to set.
pub fn is_supported_l1_config(
    parent_chain: ParentChainType,
    config: &L1ChainConfig,
) -> bool {
    supported_l1_configs()
        .into_iter()
        .any(|(chain, supported)| {
            chain == parent_chain
                && supported.url == config.url
                && supported.auth == config.auth
        })
}

/// Write or merge L1 config file with predefined configs for the given chains.
/// Merges with the existing file: keeps existing supported configs for chains
/// not in `chains_to_enable`, and adds/overwrites with the predefined config for
/// each chain in `chains_to_enable`.
pub fn write_l1_config_file(
    path: &Path,
    chains_to_enable: &[ParentChainType],
) -> std::io::Result<()> {
    let supported = supported_l1_configs();
    let mut config = L1ConfigFile::load_or_default(path);
    // Keep only existing entries that are supported (drop unsupported/custom)
    config
        .chains
        .retain(|chain, entry| is_supported_l1_config(*chain, entry));
    // Add or overwrite with predefined config for each requested chain
    for chain in chains_to_enable {
        if let Some((_, predefined)) =
            supported.iter().find(|(candidate, _)| candidate == chain)
        {
            config.insert(*chain, predefined.clone());
        }
    }
    config
        .save(path)
        .map_err(|err| std::io::Error::other(err.to_string()))
}

/// Validate the L1 config file: every entry must be one of the supported predefined configs,
/// and each node must report the expected chain (Signet or testnet4). Call before app start.
pub fn validate_l1_config_file(path: &Path) -> Result<(), Error> {
    // An unreadable or malformed file is treated as "nothing configured": it
    // will be overwritten the next time the user saves.
    for (parent_chain, entry) in L1ConfigFile::load_or_default(path).chains {
        if !is_supported_l1_config(parent_chain, &entry) {
            return Err(Error::UnsupportedL1Config);
        }
        let (detected, chain_name) = detect_chain_type(&entry)?;
        if detected != parent_chain {
            return Err(Error::ChainMismatch {
                expected: parent_chain,
                chain: chain_name,
            });
        }
    }
    Ok(())
}

/// Load the RPC config for one parent chain from the L1 config file.
pub fn load_rpc_config_from_path(
    path: &Path,
    parent_chain: ParentChainType,
) -> Option<L1ChainConfig> {
    crate::l1::config::load_chain_config(path, parent_chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l1::config::L1Auth;

    #[test]
    fn load_rpc_config_from_path_missing_file_returns_none() {
        let path = Path::new("/nonexistent/l1_rpc_configs.json");
        assert!(
            load_rpc_config_from_path(path, ParentChainType::Regtest).is_none()
        );
    }

    #[test]
    fn load_rpc_config_from_path_valid_file_returns_config() {
        let dir = std::env::temp_dir();
        let path = dir.join("coinshift_l1_rpc_test.json");
        let configs = serde_json::json!({
            "Regtest": { "url": "http://127.0.0.1:18443", "user": "u", "password": "p" }
        });
        std::fs::write(&path, configs.to_string()).unwrap();
        let cfg = load_rpc_config_from_path(&path, ParentChainType::Regtest);
        drop(std::fs::remove_file(&path)); // best-effort cleanup
        assert!(cfg.is_some());
        let cfg = cfg.unwrap();
        assert_eq!(cfg.url, "http://127.0.0.1:18443");
        assert_eq!(cfg.auth.basic_user(), "u");
        assert_eq!(cfg.auth.basic_password(), "p");
    }

    #[test]
    fn load_rpc_config_from_path_wrong_chain_returns_none() {
        let dir = std::env::temp_dir();
        let path = dir.join("coinshift_l1_rpc_test2.json");
        let configs = serde_json::json!({
            "Signet": { "url": "http://127.0.0.1:38332", "user": "u", "password": "p" }
        });
        std::fs::write(&path, configs.to_string()).unwrap();
        let cfg = load_rpc_config_from_path(&path, ParentChainType::Regtest);
        drop(std::fs::remove_file(&path)); // best-effort cleanup
        assert!(cfg.is_none());
    }

    #[test]
    fn supported_l1_configs_has_signet_and_bch() {
        let configs = supported_l1_configs();
        assert_eq!(configs.len(), 2);
        let (signet, bch): (Option<_>, Option<_>) = (
            configs.iter().find(|(c, _)| *c == ParentChainType::Signet),
            configs.iter().find(|(c, _)| *c == ParentChainType::BCH),
        );
        assert!(signet.is_some());
        assert!(bch.is_some());
        assert_eq!(signet.unwrap().1.url, "http://localhost:38332");
        assert_eq!(signet.unwrap().1.auth.basic_user(), "user");
        assert_eq!(signet.unwrap().1.auth.basic_password(), "password");
        assert_eq!(bch.unwrap().1.url, "http://173.230.135.236:28332");
    }

    #[test]
    fn is_supported_l1_config_exact_match_only() {
        let (_, signet_rpc) = supported_l1_configs()
            .into_iter()
            .find(|(c, _)| *c == ParentChainType::Signet)
            .unwrap();
        assert!(is_supported_l1_config(ParentChainType::Signet, &signet_rpc));
        let wrong_url = L1ChainConfig {
            url: "http://other:38332".to_string(),
            ..signet_rpc.clone()
        };
        assert!(!is_supported_l1_config(ParentChainType::Signet, &wrong_url));
        let wrong_password = L1ChainConfig {
            auth: L1Auth::basic("user", "hunter2"),
            ..signet_rpc.clone()
        };
        assert!(!is_supported_l1_config(
            ParentChainType::Signet,
            &wrong_password
        ));
        // Local-only knobs must not affect whether a config is accepted.
        let custom_timeout = L1ChainConfig {
            timeout_secs: Some(30),
            ..signet_rpc
        };
        assert!(is_supported_l1_config(
            ParentChainType::Signet,
            &custom_timeout
        ));
    }

    #[test]
    fn validate_l1_config_file_empty_or_missing_ok() {
        let path = Path::new("/nonexistent/l1_rpc_configs.json");
        assert!(validate_l1_config_file(path).is_ok());
    }

    #[test]
    fn validate_l1_config_file_unsupported_config_fails() {
        let dir = std::env::temp_dir();
        let path = dir.join("coinshift_l1_validate_unsupported.json");
        let configs = serde_json::json!({
            "Signet": { "url": "http://custom:38332", "user": "u", "password": "p" }
        });
        std::fs::write(&path, configs.to_string()).unwrap();
        let result = validate_l1_config_file(&path);
        drop(std::fs::remove_file(&path)); // best-effort cleanup
        assert!(matches!(result, Err(Error::UnsupportedL1Config)));
    }
}
