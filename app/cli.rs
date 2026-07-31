use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Deref,
    path::PathBuf,
    sync::LazyLock,
    time::Duration,
};

use clap::{Arg, Parser, Subcommand};
use coinshift::types::{Network, THIS_SIDECHAIN};

use crate::util::saturating_pred_level;

const fn ipv4_socket_addr(ipv4_octets: [u8; 4], port: u16) -> SocketAddr {
    let [a, b, c, d] = ipv4_octets;
    let ipv4 = Ipv4Addr::new(a, b, c, d);
    SocketAddr::new(IpAddr::V4(ipv4), port)
}

static DEFAULT_DATA_DIR: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| match dirs::data_dir() {
        None => {
            tracing::warn!("Failed to resolve default data dir");
            None
        }
        Some(data_dir) => Some(data_dir.join("coinshift")),
    });

const DEFAULT_NET_ADDR: SocketAddr =
    ipv4_socket_addr([0, 0, 0, 0], 4000 + THIS_SIDECHAIN as u16);

const DEFAULT_RPC_ADDR: SocketAddr =
    ipv4_socket_addr([127, 0, 0, 1], 6000 + THIS_SIDECHAIN as u16);

/// Implement arg manually so that there is only a default if we can resolve
/// the default data dir
#[derive(Clone, Debug)]
#[repr(transparent)]
struct DatadirArg(PathBuf);

impl clap::FromArgMatches for DatadirArg {
    fn from_arg_matches(
        matches: &clap::ArgMatches,
    ) -> Result<Self, clap::Error> {
        let mut matches = matches.clone();
        Self::from_arg_matches_mut(&mut matches)
    }

    fn from_arg_matches_mut(
        matches: &mut clap::ArgMatches,
    ) -> Result<Self, clap::Error> {
        let datadir = matches
            .remove_one::<PathBuf>("DATADIR")
            .expect("`datadir` is required");
        Ok(Self(datadir))
    }

    fn update_from_arg_matches(
        &mut self,
        matches: &clap::ArgMatches,
    ) -> Result<(), clap::Error> {
        let mut matches = matches.clone();
        self.update_from_arg_matches_mut(&mut matches)
    }

    fn update_from_arg_matches_mut(
        &mut self,
        matches: &mut clap::ArgMatches,
    ) -> Result<(), clap::Error> {
        if let Some(datadir) = matches.remove_one("DATADIR") {
            self.0 = datadir;
        }
        Ok(())
    }
}

impl clap::Args for DatadirArg {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        cmd.arg({
            let arg = Arg::new("DATADIR")
                .value_parser(clap::builder::PathBufValueParser::new())
                .long("datadir")
                .short('d')
                .help("Data directory for storing blockchain and wallet data");
            match DEFAULT_DATA_DIR.deref() {
                None => arg.required(true),
                Some(datadir) => {
                    arg.required(false).default_value(datadir.as_os_str())
                }
            }
        })
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        Self::augment_args(cmd)
    }
}

/// Optional subcommand: init writes L1 config and exits.
#[derive(Clone, Debug, Subcommand)]
pub(super) enum AppSubcommand {
    /// Write L1 RPC config and exit, without starting the app.
    Init {
        /// Parent chain endpoint, as `<chain>=<url>`. Repeatable.
        ///
        /// Credentials go in the URL, e.g.
        /// `--l1 signet=http://user:password@localhost:38332`.
        #[arg(long = "l1", value_name = "CHAIN=URL")]
        l1: Vec<String>,
    },
}

#[derive(Clone, Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub(super) struct Cli {
    #[command(flatten)]
    pub(super) run: RunArgs,
    #[command(subcommand)]
    pub(super) command: Option<AppSubcommand>,
}

#[derive(Clone, Debug, Parser)]
pub(super) struct RunArgs {
    /// Data directory for storing blockchain and wallet data
    #[command(flatten)]
    datadir: DatadirArg,
    /// If specified, the gui will not launch.
    #[arg(long)]
    headless: bool,
    /// Directory in which to store log files.
    /// Defaults to `<DATADIR>/logs/v<VERSION>`, where `<DATADIR>` is coinshift's data
    /// directory, and `<VERSION>` is the coinshift app version.
    /// By default, only logs at the WARN level and above are logged to file.
    /// If set to the empty string, logging to file will be disabled.
    #[arg(long)]
    log_dir: Option<PathBuf>,

    /// Log level for logs that get written to file
    #[arg(default_value_t = tracing::Level::WARN, long)]
    log_level_file: tracing::Level,

    /// Log level
    #[arg(default_value_t = tracing::Level::DEBUG, long)]
    log_level: tracing::Level,

    /// Connect to mainchain node gRPC server running at this URL
    #[arg(default_value = "http://localhost:50051", long)]
    mainchain_grpc_url: url::Url,

    /// How long to wait for the mainchain enforcer during startup, in seconds.
    #[arg(default_value_t = 30, long)]
    mainchain_connect_timeout: u64,

    /// Refuse to start unless the mainchain enforcer answers.
    ///
    /// Off by default: the node starts and keeps retrying, so it can be brought
    /// up alongside the enforcer without ordering the two. Turn this on to
    /// restore the previous behaviour of exiting when the enforcer is absent.
    #[arg(long)]
    require_mainchain: bool,

    /// Path to a mnemonic seed phrase
    #[arg(long)]
    mnemonic_seed_phrase_path: Option<PathBuf>,
    /// Socket address to use for P2P networking
    #[arg(default_value_t = DEFAULT_NET_ADDR, long, short)]
    net_addr: SocketAddr,
    /// Set the network. Setting this may affect other defaults.
    #[arg(default_value_t, long, value_enum)]
    network: Network,
    /// Socket address to host the RPC server
    #[arg(default_value_t = DEFAULT_RPC_ADDR, long, short)]
    rpc_addr: SocketAddr,

    /// Parent chain endpoint to write to the L1 config before start, as
    /// `<chain>=<url>`. Repeatable.
    ///
    /// Credentials go in the URL, e.g.
    /// `--l1 signet=http://user:password@localhost:38332`.
    #[arg(long = "l1", value_name = "CHAIN=URL")]
    pub(super) l1: Vec<String>,

    /// Refuse to start if any configured parent chain is unreachable or is
    /// serving the wrong network.
    ///
    /// Off by default: a parent chain being down pauses detection for that
    /// chain only, and the node stays up. Turn this on for supervised
    /// deployments that would rather fail fast.
    #[arg(long)]
    strict_l1_config: bool,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub datadir: PathBuf,
    pub headless: bool,
    /// If None, logging to file should be disabled.
    pub log_dir: Option<PathBuf>,
    pub log_level: tracing::Level,
    pub log_level_file: tracing::Level, // Level for logs that get written to file
    pub mainchain_grpc_url: url::Url,
    pub mnemonic_seed_phrase_path: Option<PathBuf>,
    pub net_addr: SocketAddr,
    pub network: Network,
    pub rpc_addr: SocketAddr,
    pub strict_l1_config: bool,
    pub mainchain_connect_timeout: Duration,
    pub require_mainchain: bool,
}

impl RunArgs {
    pub fn get_config(self) -> anyhow::Result<Config> {
        let log_dir = match self.log_dir {
            None => {
                let version_dir_name =
                    format!("v{}", env!("CARGO_PKG_VERSION"));
                let log_dir =
                    self.datadir.0.join("logs").join(version_dir_name);
                Some(log_dir)
            }
            Some(log_dir) => {
                if log_dir.as_os_str().is_empty() {
                    None
                } else {
                    Some(log_dir)
                }
            }
        };
        let log_level = if self.headless {
            self.log_level
        } else {
            saturating_pred_level(self.log_level)
        };
        Ok(Config {
            datadir: self.datadir.0,
            headless: self.headless,
            log_dir,
            log_level,
            log_level_file: self.log_level_file,
            mainchain_grpc_url: self.mainchain_grpc_url,
            mnemonic_seed_phrase_path: self.mnemonic_seed_phrase_path,
            net_addr: self.net_addr,
            network: self.network,
            rpc_addr: self.rpc_addr,
            strict_l1_config: self.strict_l1_config,
            mainchain_connect_timeout: Duration::from_secs(
                self.mainchain_connect_timeout,
            ),
            require_mainchain: self.require_mainchain,
        })
    }
}
