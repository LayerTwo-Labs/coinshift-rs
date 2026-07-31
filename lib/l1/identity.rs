//! Deciding whether an endpoint is serving the chain it was configured for.
//!
//! This replaces the closed url/user/password allowlist. The allowlist could
//! only ever answer "is this one of the two endpoints we shipped"; identity
//! probing answers the question that actually matters — *is this node on the
//! network this swap expects* — which works for any endpoint the operator runs.
//!
//! # What can and cannot be distinguished
//!
//! The evidence is weaker than it looks. Bitcoin, Bitcoin Cash and Litecoin all
//! report their mainnet as `main`, and **Bitcoin Cash shares Bitcoin's genesis
//! block** because it forked from it at height 478558. So:
//!
//! - For chains the `bitcoin` crate models (BTC, Signet, Regtest) the expected
//!   genesis hash is *computed* from the crate rather than hardcoded, and a
//!   mismatch is conclusive.
//! - For BCH and LTC there is no such source, so identity rests on the reported
//!   network name unless the operator pins `expected_genesis` themselves.
//!
//! The realistic misconfiguration this catches — pointing a Signet swap at a
//! mainnet node, or a Regtest swap at a real network — is caught conclusively.

use crate::{parent_chain::ChainIdentity, types::ParentChainType};

/// Why an endpoint was rejected for a chain.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IdentityMismatch {
    #[error(
        "node reports network {reported:?}, but {chain:?} expects one of {expected:?}"
    )]
    NetworkName {
        chain: ParentChainType,
        reported: String,
        expected: &'static [&'static str],
    },
    #[error(
        "node reports genesis {reported}, but {chain:?} expects {expected}"
    )]
    Genesis {
        chain: ParentChainType,
        reported: String,
        expected: String,
    },
}

/// Network names a node may report for `chain`.
///
/// Bitcoin Cash Node reports testnet4 as either `testnet4` or `test4` depending
/// on version, so both are accepted.
pub fn accepted_network_names(
    chain: ParentChainType,
) -> &'static [&'static str] {
    match chain {
        ParentChainType::BTC | ParentChainType::LTC => &["main"],
        ParentChainType::BCH => &["main", "testnet4", "test4"],
        ParentChainType::Signet => &["signet"],
        ParentChainType::Regtest => &["regtest"],
        // Solana has no network-name concept; the genesis hash carries the
        // whole identity, and unlike BCH it is exact.
        ParentChainType::Solana | ParentChainType::SolanaDevnet => {
            &[crate::parent_chain::solana::SOLANA_NETWORK_NAME]
        }
    }
}

/// The genesis hash `chain` must report, when one can be established.
///
/// Computed from the `bitcoin` crate for the networks it models, so there are no
/// hand-copied constants to get wrong. Returns `None` for BCH and LTC, whose
/// genesis the crate cannot supply.
pub fn expected_genesis(chain: ParentChainType) -> Option<String> {
    // Bitcoin-family: computed from the crate, so there is no constant to get
    // wrong. Solana: verified against the public cluster endpoints, since
    // nothing in the tree can derive them.
    if let Some(network) = chain.bitcoin_network() {
        return Some(
            bitcoin::constants::genesis_block(network)
                .block_hash()
                .to_string(),
        );
    }
    match chain {
        ParentChainType::Solana => Some(SOLANA_MAINNET_GENESIS.to_string()),
        ParentChainType::SolanaDevnet => {
            Some(SOLANA_DEVNET_GENESIS.to_string())
        }
        _ => None,
    }
}

/// Genesis hash of Solana mainnet-beta.
pub const SOLANA_MAINNET_GENESIS: &str =
    "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";

/// Genesis hash of Solana devnet.
pub const SOLANA_DEVNET_GENESIS: &str =
    "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

/// Check the evidence an endpoint gave against what `chain` requires.
///
/// `configured_genesis` overrides the built-in expectation, which is how a
/// custom signet (`-signetchallenge`) or a non-default regtest is supported —
/// both have a genesis that depends on local parameters.
pub fn verify(
    chain: ParentChainType,
    identity: &ChainIdentity,
    configured_genesis: Option<String>,
) -> Result<(), IdentityMismatch> {
    let accepted = accepted_network_names(chain);
    if !accepted.contains(&identity.chain_name.as_str()) {
        return Err(IdentityMismatch::NetworkName {
            chain,
            reported: identity.chain_name.clone(),
            expected: accepted,
        });
    }

    let expected = configured_genesis.or_else(|| expected_genesis(chain));
    if let (Some(expected), Some(reported)) = (expected, &identity.genesis)
        && *reported != expected
    {
        return Err(IdentityMismatch::Genesis {
            chain,
            reported: reported.clone(),
            expected,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str, genesis: Option<&str>) -> ChainIdentity {
        ChainIdentity {
            chain_name: name.to_string(),
            genesis: genesis.map(str::to_string),
        }
    }

    fn genesis_of(chain: ParentChainType) -> String {
        expected_genesis(chain).unwrap()
    }

    #[test]
    fn accepts_a_node_on_the_configured_network() {
        for chain in [
            ParentChainType::BTC,
            ParentChainType::Signet,
            ParentChainType::Regtest,
        ] {
            let name = accepted_network_names(chain)[0];
            let id = identity(name, Some(&genesis_of(chain)));
            assert_eq!(verify(chain, &id, None), Ok(()), "{chain:?}");
        }
    }

    #[test]
    fn rejects_a_node_on_the_wrong_network() {
        // The realistic mistake: a Signet swap pointed at a mainnet node.
        let mainnet = identity("main", Some(&genesis_of(ParentChainType::BTC)));
        assert!(matches!(
            verify(ParentChainType::Signet, &mainnet, None),
            Err(IdentityMismatch::NetworkName { .. })
        ));
        assert!(matches!(
            verify(ParentChainType::Regtest, &mainnet, None),
            Err(IdentityMismatch::NetworkName { .. })
        ));
    }

    #[test]
    fn rejects_a_matching_name_with_the_wrong_genesis() {
        // Same network name, different chain: only genesis catches this.
        let wrong =
            identity("regtest", Some(&genesis_of(ParentChainType::Signet)));
        assert!(matches!(
            verify(ParentChainType::Regtest, &wrong, None),
            Err(IdentityMismatch::Genesis { .. })
        ));
    }

    #[test]
    fn a_configured_genesis_overrides_the_built_in_one() {
        // Custom signet: the genesis depends on -signetchallenge, so the
        // operator must be able to pin their own.
        let custom = genesis_of(ParentChainType::Regtest);
        let id = identity("signet", Some(&custom));
        assert!(matches!(
            verify(ParentChainType::Signet, &id, None),
            Err(IdentityMismatch::Genesis { .. })
        ));
        assert_eq!(
            verify(ParentChainType::Signet, &id, Some(custom.clone())),
            Ok(())
        );
    }

    #[test]
    fn bch_and_ltc_fall_back_to_the_network_name() {
        // The bitcoin crate cannot supply their genesis, and BCH mainnet shares
        // Bitcoin's anyway, so genesis is not usable evidence for them.
        assert!(expected_genesis(ParentChainType::BCH).is_none());
        assert!(expected_genesis(ParentChainType::LTC).is_none());

        let bch_testnet4 = identity("test4", None);
        assert_eq!(verify(ParentChainType::BCH, &bch_testnet4, None), Ok(()));
        assert_eq!(
            verify(ParentChainType::BCH, &identity("testnet4", None), None),
            Ok(())
        );
        assert!(matches!(
            verify(ParentChainType::LTC, &bch_testnet4, None),
            Err(IdentityMismatch::NetworkName { .. })
        ));
    }

    #[test]
    fn a_node_that_hides_its_genesis_is_still_usable() {
        let id = identity("signet", None);
        assert_eq!(verify(ParentChainType::Signet, &id, None), Ok(()));
    }

    #[test]
    fn btc_and_bch_mainnet_are_not_separable_by_genesis() {
        // Documents the limitation rather than pretending it away: BCH forked
        // from Bitcoin, so a BCH mainnet node presents Bitcoin's genesis and
        // calls itself "main".
        let btc_mainnet =
            identity("main", Some(&genesis_of(ParentChainType::BTC)));
        assert_eq!(verify(ParentChainType::BTC, &btc_mainnet, None), Ok(()));
        assert_eq!(verify(ParentChainType::BCH, &btc_mainnet, None), Ok(()));
    }
}
