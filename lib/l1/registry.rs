//! Per-chain L1 clients and their health.
//!
//! # Why this exists
//!
//! L1 config used to be validated once, at process start, and a chain that was
//! configured but unreachable made the node refuse to boot — while a chain that
//! was not configured at all was completely fine. That asymmetry meant an
//! operator could be locked out of their own node by an unrelated service being
//! down.
//!
//! The registry moves the check from *boot* to *use*. Startup never touches the
//! network. A background task probes each configured endpoint and records its
//! health, and [`L1Registry::verified_client`] hands out a client only for a
//! chain that is currently known-good. That is strictly stronger than the old
//! check: it also catches an endpoint that starts serving the wrong network
//! *after* startup, which a boot-time check cannot.
//!
//! The non-corruption property is structural rather than a matter of
//! discipline: `verified_client` is the only way to obtain a client from the
//! registry, so there is no path from a `WrongChain` entry to a swap-state
//! write.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::RwLock;

use crate::{
    l1::{
        config::{L1ChainConfig, L1ConfigFile},
        identity,
        status::L1ChainHealth,
    },
    parent_chain::{ParentChainClient, client_for},
    types::ParentChainType,
};

/// How often the health task re-probes each configured endpoint.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// How long a successful probe stays valid.
///
/// Longer than [`PROBE_INTERVAL`] so an ordinary slow round does not make
/// chains flap, but finite so that a wedged health task pauses detection
/// instead of letting it run on stale assurance.
pub const HEALTH_TTL: Duration = Duration::from_secs(120);

struct ChainEntry {
    config: L1ChainConfig,
    health: L1ChainHealth,
    client: Arc<dyn ParentChainClient>,
    /// When the last successful probe completed.
    verified_at: Option<Instant>,
    consecutive_failures: u32,
}

/// The set of configured parent chains, their clients, and their health.
pub struct L1Registry {
    path: Option<PathBuf>,
    entries: RwLock<BTreeMap<ParentChainType, ChainEntry>>,
}

impl L1Registry {
    /// A registry backed by the config file at `path`.
    ///
    /// `None` means no configuration at all, which is a supported way to run:
    /// every chain reports [`L1ChainHealth::Unconfigured`] and swaps stay
    /// `Pending`. Reads the file but performs no network I/O.
    pub fn new(path: Option<PathBuf>) -> Self {
        let registry = Self {
            path,
            entries: RwLock::new(BTreeMap::new()),
        };
        registry.reload();
        registry
    }

    /// Re-read the config file, keeping health for entries that did not change.
    ///
    /// A changed endpoint drops its previous verdict, so an edited config can
    /// never inherit the health of the endpoint it replaced.
    pub fn reload(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let file = L1ConfigFile::load_or_default(path);
        let mut entries = self.entries.write();
        entries.retain(|chain, entry| {
            file.get(*chain)
                .is_some_and(|config| *config == entry.config)
        });
        for (chain, config) in file.chains {
            if entries.contains_key(&chain) {
                continue;
            }
            let health = if config.enabled {
                L1ChainHealth::Probing
            } else {
                L1ChainHealth::Disabled
            };
            entries.insert(
                chain,
                ChainEntry {
                    client: client_for(chain, &config),
                    config,
                    health,
                    verified_at: None,
                    consecutive_failures: 0,
                },
            );
        }
    }

    /// Chains that are configured and enabled, with the client to probe them.
    fn probe_targets(
        &self,
    ) -> Vec<(ParentChainType, Arc<dyn ParentChainClient>, Option<String>)>
    {
        self.entries
            .read()
            .iter()
            .filter(|(_, entry)| entry.config.enabled)
            .map(|(chain, entry)| {
                (
                    *chain,
                    entry.client.clone(),
                    entry.config.expected_genesis.clone(),
                )
            })
            .collect()
    }

    /// Probe every configured chain and record the outcome.
    pub async fn probe_all(&self) {
        for (chain, client, expected_genesis) in self.probe_targets() {
            let health = match client.identify().await {
                Err(err) => {
                    let consecutive_failures = self
                        .entries
                        .read()
                        .get(&chain)
                        .map_or(0, |entry| entry.consecutive_failures)
                        .saturating_add(1);
                    L1ChainHealth::Unreachable {
                        error: err.to_string(),
                        consecutive_failures,
                    }
                }
                Ok(reported) => {
                    match identity::verify(chain, &reported, expected_genesis) {
                        Err(mismatch) => L1ChainHealth::WrongChain {
                            reason: mismatch.to_string(),
                        },
                        Ok(()) => match client.tip().await {
                            Ok(block_height) => L1ChainHealth::Healthy {
                                chain_name: reported.chain_name,
                                block_height,
                            },
                            Err(err) => L1ChainHealth::Unreachable {
                                error: err.to_string(),
                                consecutive_failures: 1,
                            },
                        },
                    }
                }
            };
            self.record(chain, health);
        }
    }

    fn record(&self, chain: ParentChainType, health: L1ChainHealth) {
        let mut entries = self.entries.write();
        let Some(entry) = entries.get_mut(&chain) else {
            // The config changed under us; the next probe round covers it.
            return;
        };
        match &health {
            L1ChainHealth::Healthy { .. } => {
                if !entry.health.is_healthy() {
                    tracing::info!(?chain, "L1 chain is healthy");
                }
                entry.verified_at = Some(Instant::now());
                entry.consecutive_failures = 0;
            }
            L1ChainHealth::WrongChain { reason } => {
                if entry.health != health {
                    tracing::warn!(
                        ?chain,
                        %reason,
                        "L1 endpoint is serving the wrong network; it will not be used"
                    );
                }
                entry.verified_at = None;
            }
            L1ChainHealth::Unreachable {
                error,
                consecutive_failures,
            } => {
                if *consecutive_failures == 1 {
                    tracing::warn!(
                        ?chain,
                        %error,
                        "L1 endpoint unreachable; swap detection paused for this chain"
                    );
                }
                entry.consecutive_failures = *consecutive_failures;
                entry.verified_at = None;
            }
            _ => {}
        }
        entry.health = health;
    }

    /// A client for `chain`, but only while that chain is known-good.
    ///
    /// This is the **only** way to obtain a client from the registry. It
    /// returns `None` unless the chain last probed healthy and that result is
    /// still within [`HEALTH_TTL`], so a wrong-chain or stale endpoint cannot
    /// reach swap state.
    pub fn verified_client(
        &self,
        chain: ParentChainType,
    ) -> Option<Arc<dyn ParentChainClient>> {
        let entries = self.entries.read();
        let entry = entries.get(&chain)?;
        if !entry.health.is_healthy() {
            return None;
        }
        let fresh = entry
            .verified_at
            .is_some_and(|at| at.elapsed() < HEALTH_TTL);
        if !fresh {
            tracing::debug!(
                ?chain,
                "Skipping L1 lookup: health check is stale"
            );
            return None;
        }
        Some(entry.client.clone())
    }

    /// Current health of `chain`.
    pub fn health(&self, chain: ParentChainType) -> L1ChainHealth {
        self.entries
            .read()
            .get(&chain)
            .map_or(L1ChainHealth::Unconfigured, |entry| entry.health.clone())
    }

    /// Health of every chain, including those with no configuration.
    pub fn statuses(&self) -> Vec<(ParentChainType, L1ChainHealth)> {
        let entries = self.entries.read();
        ParentChainType::all()
            .iter()
            .map(|chain| {
                let health = entries
                    .get(chain)
                    .map_or(L1ChainHealth::Unconfigured, |entry| {
                        entry.health.clone()
                    });
                (*chain, health)
            })
            .collect()
    }

    /// A registry with one chain already vouched for, backed by `client`.
    ///
    /// Test-only: lets the swap observer be exercised against a scripted parent
    /// chain without a config file or a real endpoint.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn with_verified_client(
        chain: ParentChainType,
        client: Arc<dyn ParentChainClient>,
    ) -> Self {
        let config = L1ChainConfig::basic("http://mock", "", "");
        let entry = ChainEntry {
            config,
            health: L1ChainHealth::Healthy {
                chain_name: "mock".to_string(),
                block_height: 0,
            },
            client,
            verified_at: Some(Instant::now()),
            consecutive_failures: 0,
        };
        Self {
            path: None,
            entries: RwLock::new(BTreeMap::from([(chain, entry)])),
        }
    }

    /// Chains that are configured but not currently usable, for `--strict-l1-config`.
    pub fn unhealthy_configured(
        &self,
    ) -> Vec<(ParentChainType, L1ChainHealth)> {
        self.statuses()
            .into_iter()
            .filter(|(_, health)| {
                health.is_configured()
                    && !health.is_healthy()
                    && *health != L1ChainHealth::Disabled
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l1::config::L1ChainConfig;

    fn temp_config(name: &str, chains: &[(ParentChainType, bool)]) -> PathBuf {
        let path = std::env::temp_dir()
            .join(format!("coinshift_l1_registry_{name}.json"));
        let mut file = L1ConfigFile::default();
        for (chain, enabled) in chains {
            file.insert(
                *chain,
                L1ChainConfig {
                    enabled: *enabled,
                    ..L1ChainConfig::basic("http://127.0.0.1:1", "u", "p")
                },
            );
        }
        file.save(&path).unwrap();
        path
    }

    #[test]
    fn no_config_path_means_every_chain_is_unconfigured() {
        let registry = L1Registry::new(None);
        for (chain, health) in registry.statuses() {
            assert_eq!(health, L1ChainHealth::Unconfigured, "{chain:?}");
            assert!(registry.verified_client(chain).is_none());
        }
        assert!(registry.unhealthy_configured().is_empty());
    }

    #[test]
    fn construction_performs_no_network_io_and_starts_probing() {
        // The endpoint below is deliberately dead. Construction must still
        // succeed instantly -- this is the startup trap that used to make the
        // node refuse to boot.
        let path = temp_config("probing", &[(ParentChainType::Signet, true)]);
        let registry = L1Registry::new(Some(path.clone()));
        assert_eq!(
            registry.health(ParentChainType::Signet),
            L1ChainHealth::Probing
        );
        // Not yet verified, so no client is handed out.
        assert!(registry.verified_client(ParentChainType::Signet).is_none());
        drop(std::fs::remove_file(&path));
    }

    #[test]
    fn a_disabled_chain_is_never_probed_or_used() {
        let path = temp_config("disabled", &[(ParentChainType::BCH, false)]);
        let registry = L1Registry::new(Some(path.clone()));
        assert_eq!(
            registry.health(ParentChainType::BCH),
            L1ChainHealth::Disabled
        );
        assert!(registry.probe_targets().is_empty());
        assert!(registry.verified_client(ParentChainType::BCH).is_none());
        // Disabled is a deliberate choice, not a fault, so strict mode ignores it.
        assert!(registry.unhealthy_configured().is_empty());
        drop(std::fs::remove_file(&path));
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_is_recorded_not_fatal() {
        let path =
            temp_config("unreachable", &[(ParentChainType::Regtest, true)]);
        let registry = L1Registry::new(Some(path.clone()));
        registry.probe_all().await;
        assert!(matches!(
            registry.health(ParentChainType::Regtest),
            L1ChainHealth::Unreachable { .. }
        ));
        assert!(registry.verified_client(ParentChainType::Regtest).is_none());
        assert_eq!(registry.unhealthy_configured().len(), 1);
        drop(std::fs::remove_file(&path));
    }

    #[tokio::test]
    async fn repeated_failures_are_counted() {
        let path = temp_config("failures", &[(ParentChainType::Regtest, true)]);
        let registry = L1Registry::new(Some(path.clone()));
        registry.probe_all().await;
        registry.probe_all().await;
        match registry.health(ParentChainType::Regtest) {
            L1ChainHealth::Unreachable {
                consecutive_failures,
                ..
            } => assert_eq!(consecutive_failures, 2),
            other => panic!("expected Unreachable, got {other:?}"),
        }
        drop(std::fs::remove_file(&path));
    }

    #[tokio::test]
    async fn reload_drops_health_when_the_endpoint_changes() {
        let path = temp_config("reload", &[(ParentChainType::Signet, true)]);
        let registry = L1Registry::new(Some(path.clone()));
        registry.probe_all().await;
        assert!(matches!(
            registry.health(ParentChainType::Signet),
            L1ChainHealth::Unreachable { .. }
        ));

        // Point the chain somewhere else: the old verdict must not carry over.
        let mut file = L1ConfigFile::default();
        file.insert(
            ParentChainType::Signet,
            L1ChainConfig::basic("http://127.0.0.1:2", "u", "p"),
        );
        file.save(&path).unwrap();
        registry.reload();
        assert_eq!(
            registry.health(ParentChainType::Signet),
            L1ChainHealth::Probing
        );
        drop(std::fs::remove_file(&path));
    }

    #[tokio::test]
    async fn reload_keeps_health_for_an_unchanged_entry() {
        let path = temp_config("stable", &[(ParentChainType::Signet, true)]);
        let registry = L1Registry::new(Some(path.clone()));
        registry.probe_all().await;
        let before = registry.health(ParentChainType::Signet);
        registry.reload();
        assert_eq!(registry.health(ParentChainType::Signet), before);
        drop(std::fs::remove_file(&path));
    }

    #[test]
    fn removing_a_chain_from_the_config_unconfigures_it() {
        let path = temp_config("removed", &[(ParentChainType::BCH, true)]);
        let registry = L1Registry::new(Some(path.clone()));
        assert!(registry.health(ParentChainType::BCH).is_configured());
        L1ConfigFile::default().save(&path).unwrap();
        registry.reload();
        assert_eq!(
            registry.health(ParentChainType::BCH),
            L1ChainHealth::Unconfigured
        );
        drop(std::fs::remove_file(&path));
    }
}
