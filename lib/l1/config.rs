//! The single definition of an L1 parent-chain endpoint: what it looks like,
//! where it is stored, and how it is read and written.
//!
//! Before this module the same three fields were declared in four places and the
//! config path was recomputed in six, with three unsynchronised writers. Every
//! reader and writer now goes through [`L1ConfigFile`].
//!
//! # File format
//!
//! Version 2 is `{ "version": 2, "chains": { "<ParentChainType>": { … } } }`.
//! Version 1 — the original, unversioned
//! `{ "<ParentChainType>": { "url", "user", "password" } }` — is still accepted
//! on read and upgraded in memory; the next [`L1ConfigFile::save`] rewrites the
//! file in the new shape.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::types::ParentChainType;

/// Current on-disk format version.
pub const CONFIG_VERSION: u32 = 2;

/// Request timeout used when a chain does not override it.
pub const DEFAULT_TIMEOUT_SECS: u64 = 10;

const APP_DIR: &str = "coinshift";
const CONFIG_FILE_NAME: &str = "l1_rpc_configs.json";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read L1 config at {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write L1 config at {path}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse L1 config at {path}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// The default on-disk location of the L1 config file.
///
/// This is the **only** place the path is computed. Linux
/// `~/.local/share/coinshift/`, macOS `~/Library/Application Support/coinshift/`,
/// Windows `%APPDATA%\coinshift\`.
pub fn default_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
        .join(CONFIG_FILE_NAME)
}

/// How to authenticate to a parent-chain RPC endpoint.
///
/// Bitcoin Core and its forks use HTTP basic auth. Hosted endpoints for other
/// chains generally do not — they take an API key in a header or a query
/// parameter — which is why this is an enum rather than a user/password pair.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum L1Auth {
    /// No authentication.
    #[default]
    None,
    /// HTTP basic auth (Bitcoin Core `rpcuser`/`rpcpassword`).
    Basic { user: String, password: String },
    /// `Authorization: Bearer <token>`.
    Bearer { token: String },
    /// An arbitrary header, e.g. `x-api-key`.
    Header { name: String, value: String },
    /// An arbitrary query parameter, e.g. `?api-key=…`.
    QueryParam { name: String, value: String },
}

impl L1Auth {
    /// Basic auth, normalising an empty user to [`L1Auth::None`].
    ///
    /// The pre-existing client skipped auth entirely when the user was empty;
    /// preserving that here keeps behaviour identical for configs that carry
    /// blank credentials.
    pub fn basic(user: impl Into<String>, password: impl Into<String>) -> Self {
        let user = user.into();
        if user.is_empty() {
            Self::None
        } else {
            Self::Basic {
                user,
                password: password.into(),
            }
        }
    }

    /// Basic-auth user, or `""` for any other scheme.
    pub fn basic_user(&self) -> &str {
        match self {
            Self::Basic { user, .. } => user,
            _ => "",
        }
    }

    /// Basic-auth password, or `""` for any other scheme.
    pub fn basic_password(&self) -> &str {
        match self {
            Self::Basic { password, .. } => password,
            _ => "",
        }
    }

    /// Whether this carries a secret that must not be exposed over the API.
    pub fn has_secret(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Connection settings for one parent chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct L1ChainConfig {
    /// Endpoint URL.
    ///
    /// Deliberately a `String` rather than a `url::Url`: `Url` normalises on
    /// parse (notably by appending a trailing slash to an empty path), which
    /// would break the exact-string comparison in
    /// `parent_chain_rpc::is_supported_l1_config`. That comparison goes away
    /// with the config allowlist, at which point this can be tightened.
    pub url: String,
    #[serde(default)]
    pub auth: L1Auth,
    /// Whether to use this chain. A disabled entry is kept but not polled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Overrides the expected genesis hash for custom signet/regtest networks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_genesis: Option<bitcoin::BlockHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

fn default_enabled() -> bool {
    true
}

impl L1ChainConfig {
    /// A config with basic auth and every other setting defaulted.
    pub fn basic(
        url: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            auth: L1Auth::basic(user, password),
            enabled: true,
            expected_genesis: None,
            poll_interval_secs: None,
            timeout_secs: None,
        }
    }

    /// Request timeout for this endpoint.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS))
    }
}

/// The parsed contents of the L1 config file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct L1ConfigFile {
    pub version: u32,
    pub chains: BTreeMap<ParentChainType, L1ChainConfig>,
}

impl Default for L1ConfigFile {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            chains: BTreeMap::new(),
        }
    }
}

/// Version 1 entry: a flat `{ url, user, password }`.
#[derive(Deserialize)]
struct LegacyChainConfig {
    url: String,
    #[serde(default)]
    user: String,
    #[serde(default)]
    password: String,
}

/// Accepts either on-disk shape. Tried in order, so a v2 file never falls
/// through to the legacy branch (its `version` key is not a chain name).
#[derive(Deserialize)]
#[serde(untagged)]
enum L1ConfigFileRepr {
    Versioned {
        version: u32,
        chains: BTreeMap<ParentChainType, L1ChainConfig>,
    },
    Legacy(BTreeMap<ParentChainType, LegacyChainConfig>),
}

impl<'de> Deserialize<'de> for L1ConfigFile {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        Ok(match L1ConfigFileRepr::deserialize(deserializer)? {
            L1ConfigFileRepr::Versioned { version, chains } => {
                Self { version, chains }
            }
            L1ConfigFileRepr::Legacy(legacy) => Self {
                version: CONFIG_VERSION,
                chains: legacy
                    .into_iter()
                    .map(|(chain, cfg)| {
                        (
                            chain,
                            L1ChainConfig::basic(
                                cfg.url,
                                cfg.user,
                                cfg.password,
                            ),
                        )
                    })
                    .collect(),
            },
        })
    }
}

impl L1ConfigFile {
    /// Read the config, failing on unreadable or malformed contents.
    ///
    /// A missing file is not an error — it means no chain is configured.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(Error::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        serde_json::from_str(&contents).map_err(|source| Error::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Read the config, falling back to an empty one and logging on failure.
    ///
    /// Callers on hot paths (block connect, GUI polling) use this so a damaged
    /// file degrades to "nothing configured" rather than taking the node down.
    /// Unlike the code this replaces, the failure is at least logged.
    pub fn load_or_default(path: &Path) -> Self {
        match Self::load(path) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "Ignoring unreadable L1 config; treating all chains as unconfigured"
                );
                Self::default()
            }
        }
    }

    /// Write the config atomically, readable only by the owner.
    ///
    /// The file holds RPC passwords and will hold API keys, so it is created
    /// with mode 0600 and installed with a rename so a crash mid-write cannot
    /// leave a truncated config behind.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let write_err = |source: std::io::Error| Error::Write {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(write_err)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|source| {
            Error::Parse {
                path: path.to_path_buf(),
                source,
            }
        })?;
        let tmp = path.with_extension("json.tmp");
        // Remove any stale temp file first: the 0600 mode below only applies
        // when the file is created, so reusing an existing one could keep
        // whatever permissions it already had.
        drop(std::fs::remove_file(&tmp));
        write_owner_only(&tmp, json.as_bytes()).map_err(write_err)?;
        std::fs::rename(&tmp, path).map_err(write_err)
    }

    pub fn get(&self, chain: ParentChainType) -> Option<&L1ChainConfig> {
        self.chains.get(&chain)
    }

    pub fn insert(&mut self, chain: ParentChainType, config: L1ChainConfig) {
        self.chains.insert(chain, config);
    }

    pub fn remove(&mut self, chain: ParentChainType) {
        self.chains.remove(&chain);
    }
}

/// Read the config for one chain from `path`, or `None` if absent.
pub fn load_chain_config(
    path: &Path,
    chain: ParentChainType,
) -> Option<L1ChainConfig> {
    L1ConfigFile::load_or_default(path).chains.remove(&chain)
}

#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("coinshift_l1_cfg_{name}.json"))
    }

    #[test]
    fn default_path_ends_with_the_expected_file() {
        let path = default_path();
        assert!(path.ends_with(format!("{APP_DIR}/{CONFIG_FILE_NAME}")));
    }

    #[test]
    fn reads_the_legacy_unversioned_format() {
        // The shape written by every released version so far.
        let legacy = serde_json::json!({
            "Signet": {
                "url": "http://localhost:38332",
                "user": "user",
                "password": "password"
            }
        })
        .to_string();
        let parsed: L1ConfigFile = serde_json::from_str(&legacy).unwrap();
        assert_eq!(parsed.version, CONFIG_VERSION);
        let signet = parsed.get(ParentChainType::Signet).unwrap();
        assert_eq!(signet.url, "http://localhost:38332");
        assert_eq!(
            signet.auth,
            L1Auth::Basic {
                user: "user".to_string(),
                password: "password".to_string()
            }
        );
        assert!(signet.enabled);
        assert_eq!(signet.timeout(), Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    #[test]
    fn legacy_empty_user_becomes_no_auth() {
        // The old client skipped basic auth when the user was empty; an entry
        // with blank credentials must keep behaving that way.
        let legacy = serde_json::json!({
            "Regtest": { "url": "http://127.0.0.1:18443", "user": "", "password": "" }
        })
        .to_string();
        let parsed: L1ConfigFile = serde_json::from_str(&legacy).unwrap();
        assert_eq!(
            parsed.get(ParentChainType::Regtest).unwrap().auth,
            L1Auth::None
        );
    }

    #[test]
    fn round_trips_the_versioned_format() {
        let mut config = L1ConfigFile::default();
        config.insert(
            ParentChainType::BCH,
            L1ChainConfig::basic("http://node:28332", "u", "p"),
        );
        config.insert(
            ParentChainType::Signet,
            L1ChainConfig {
                url: "http://localhost:38332".to_string(),
                auth: L1Auth::Header {
                    name: "x-api-key".to_string(),
                    value: "secret".to_string(),
                },
                enabled: false,
                expected_genesis: None,
                poll_interval_secs: Some(30),
                timeout_secs: Some(5),
            },
        );
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serde_json::from_str::<L1ConfigFile>(&json).unwrap(),
            config
        );
    }

    #[test]
    fn a_versioned_file_is_not_read_as_legacy() {
        let json = serde_json::json!({
            "version": 2,
            "chains": {
                "Signet": { "url": "http://localhost:38332", "auth": { "type": "none" } }
            }
        })
        .to_string();
        let parsed: L1ConfigFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.chains.len(), 1);
        assert_eq!(
            parsed.get(ParentChainType::Signet).unwrap().auth,
            L1Auth::None
        );
    }

    #[test]
    fn save_then_load_round_trips_and_upgrades_legacy_on_disk() {
        let path = temp_path("save_load");
        drop(std::fs::remove_file(&path));

        // Start from a legacy file on disk.
        std::fs::write(
            &path,
            serde_json::json!({
                "BCH": { "url": "http://node:28332", "user": "u", "password": "p" }
            })
            .to_string(),
        )
        .unwrap();

        let loaded = L1ConfigFile::load(&path).unwrap();
        loaded.save(&path).unwrap();

        // The rewritten file is v2 and still parses to the same value.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"version\""));
        assert_eq!(L1ConfigFile::load(&path).unwrap(), loaded);

        // The temp file must not survive a successful save.
        assert!(!path.with_extension("json.tmp").exists());

        drop(std::fs::remove_file(&path));
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_an_owner_only_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = temp_path("perms");
        drop(std::fs::remove_file(&path));
        L1ConfigFile::default().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "config may contain secrets");
        drop(std::fs::remove_file(&path));
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = Path::new("/nonexistent/coinshift/l1_rpc_configs.json");
        assert_eq!(L1ConfigFile::load(path).unwrap(), L1ConfigFile::default());
        assert!(load_chain_config(path, ParentChainType::Regtest).is_none());
    }

    #[test]
    fn malformed_file_errors_on_load_but_defaults_on_load_or_default() {
        let path = temp_path("malformed");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            L1ConfigFile::load(&path),
            Err(Error::Parse { .. })
        ));
        assert_eq!(
            L1ConfigFile::load_or_default(&path),
            L1ConfigFile::default()
        );
        drop(std::fs::remove_file(&path));
    }

    #[test]
    fn load_chain_config_only_returns_the_requested_chain() {
        let path = temp_path("one_chain");
        let mut config = L1ConfigFile::default();
        config.insert(
            ParentChainType::Signet,
            L1ChainConfig::basic("http://localhost:38332", "u", "p"),
        );
        config.save(&path).unwrap();
        assert!(load_chain_config(&path, ParentChainType::Signet).is_some());
        assert!(load_chain_config(&path, ParentChainType::Regtest).is_none());
        drop(std::fs::remove_file(&path));
    }
}
