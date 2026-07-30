use std::path::Path;

use clap::Parser as _;
use coinshift::types::ParentChainType;
use mimalloc::MiMalloc;
use tokio::{signal::ctrl_c, sync::oneshot};
use tracing_subscriber::{
    Layer, filter as tracing_filter, fmt::format, layer::SubscriberExt,
};

mod app;
mod cli;
mod gui;
mod line_buffer;
mod rpc_server;
mod util;

use line_buffer::{LineBuffer, LineBufferWriter};
use util::saturating_pred_level;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn configure_mimalloc() {
    // Tune mimalloc via environment variables
    // SAFETY: called before any threads are started, so no other thread can
    // concurrently read or write the process environment.
    unsafe {
        // Limit how many abandoned pages stay cached before returning them to the OS.
        std::env::set_var("MIMALLOC_ABANDONED_PAGE_LIMIT", "4");
        // Reset abandoned pages so their physical memory is released quickly.
        std::env::set_var("MIMALLOC_ABANDONED_PAGE_RESET", "1");
        // Cap the number of huge arenas we keep around to bound reserved memory.
        std::env::set_var("MIMALLOC_ARENA_LIMIT", "4");
        // Allow mimalloc to allocate from any NUMA node for better balance.
        std::env::set_var("MIMALLOC_USE_NUMA_NODES", "all");
        // Commit new pages immediately to avoid first-use page fault latency.
        std::env::set_var("MIMALLOC_EAGER_COMMIT", "1");
        // Commit entire regions up front rather than piecemeal to reduce faults.
        std::env::set_var("MIMALLOC_EAGER_REGION_COMMIT", "1");
        // Keep up to 32 segments cached for quick reuse before releasing to the OS.
        std::env::set_var("MIMALLOC_SEGMENT_CACHE", "32");
        // Prefer allocating from large OS pages (2MB) when the system supports it.
        std::env::set_var("MIMALLOC_LARGE_OS_PAGES", "1");
        // Reserve a handful of huge OS pages at startup to back large heaps.
        std::env::set_var("MIMALLOC_RESERVE_HUGE_OS_PAGES", "4");
        // Keep freed pages committed instead of resetting to avoid extra madvise calls.
        std::env::set_var("MIMALLOC_PAGE_RESET", "0");
        // Likewise, keep whole segments committed to reduce release/reacquire churn.
        std::env::set_var("MIMALLOC_SEGMENT_RESET", "0");
    }
}

/// The empty string target `""` can be used to set a default level.
fn targets_directive_str<'a, Targets>(targets: Targets) -> String
where
    Targets: IntoIterator<Item = (&'a str, tracing::Level)>,
{
    targets
        .into_iter()
        .map(|(target, level)| {
            let level = level.as_str().to_ascii_lowercase();
            if target.is_empty() {
                level
            } else {
                format!("{target}={level}")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Must be held for the lifetime of the program in order to keep the file
/// logger alive.
type RollingLoggerGuard = tracing_appender::non_blocking::WorkerGuard;

/// Rolling file logger.
/// Returns a guard that must be held for the lifetime of the program in order
/// to keep the file logger alive.
fn rolling_logger<S>(
    log_dir: &Path,
    log_level: tracing::Level,
) -> anyhow::Result<(impl Layer<S>, RollingLoggerGuard)>
where
    S: tracing::Subscriber
        + for<'s> tracing_subscriber::registry::LookupSpan<'s>,
{
    const LOG_FILE_SUFFIX: &str = "log";
    let rolling_log_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_suffix(LOG_FILE_SUFFIX)
        .build(log_dir)?;
    let (non_blocking_rolling_log_writer, rolling_log_guard) =
        tracing_appender::non_blocking(rolling_log_appender);
    let level_filter = tracing_filter::Targets::new().with_default(log_level);

    let rolling_log_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_ansi(false)
        .with_writer(non_blocking_rolling_log_writer)
        .with_filter(level_filter);
    Ok((rolling_log_layer, rolling_log_guard))
}

// Configure loggers.
// If the file logger is set, returns a guard that must be held for the
// lifetime of the program in order to keep the file logger alive.
fn set_tracing_subscriber(
    log_dir: Option<&Path>,
    log_level: tracing::Level,
    log_level_file: tracing::Level,
) -> anyhow::Result<(LineBuffer, Option<RollingLoggerGuard>)> {
    let targets_filter = {
        let default_directives_str = targets_directive_str([
            ("", saturating_pred_level(log_level)),
            ("bip300301", log_level),
            ("jsonrpsee_core::tracing", log_level),
            (
                "h2::codec::framed_read",
                saturating_pred_level(saturating_pred_level(log_level)),
            ),
            (
                "h2::codec::framed_write",
                saturating_pred_level(saturating_pred_level(log_level)),
            ),
            ("coinshift", log_level),
            ("coinshift_app", log_level),
            (
                "tower::buffer::worker",
                saturating_pred_level(saturating_pred_level(log_level)),
            ),
        ]);
        let directives_str =
            match std::env::var(tracing_filter::EnvFilter::DEFAULT_ENV) {
                Ok(env_directives) => {
                    format!("{default_directives_str},{env_directives}")
                }
                Err(std::env::VarError::NotPresent) => default_directives_str,
                Err(err) => return Err(anyhow::Error::from(err)),
            };
        tracing_filter::EnvFilter::builder().parse(directives_str)?
    };
    // Adding source location here means that the file name + line number
    // is included, in such a way that it can be clicked on from within
    // the IDE, and you're sent right to the specific line of code. Very handy!
    let stdout_format = format().with_source_location(true);
    let mut stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .event_format(stdout_format);

    let is_terminal =
        std::io::IsTerminal::is_terminal(&stdout_layer.writer()());
    stdout_layer.set_ansi(is_terminal);
    let (rolling_log_layer, rolling_log_guard) = match log_dir {
        None => (None, None),
        Some(log_dir) => {
            let (layer, guard) = rolling_logger(log_dir, log_level_file)?;
            (Some(layer), Some(guard))
        }
    };

    let line_buffer = LineBuffer::default();
    let capture_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_line_number(true)
        .with_ansi(false)
        .with_writer(LineBufferWriter::from(&line_buffer));
    let tracing_subscriber = tracing_subscriber::registry()
        .with(targets_filter)
        .with(stdout_layer)
        .with(capture_layer)
        .with(rolling_log_layer);
    tracing::subscriber::set_global_default(tracing_subscriber)
        .expect("setting default subscriber failed");
    Ok((line_buffer, rolling_log_guard))
}

fn run_egui_app(
    config: crate::cli::Config,
    line_buffer: LineBuffer,
    app_result: Result<crate::app::App, crate::app::Error>,
) -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions::default();
    let rpc_addr = url::Url::parse(&format!("http://{}", config.rpc_addr))
        .expect("failed to parse rpc addr");
    eframe::run_native(
        "Coinshift",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(gui::EguiApp::new(
                app_result,
                config,
                cc,
                line_buffer,
                rpc_addr,
            )))
        }),
    )
}

use coinshift::l1::config::{
    L1Auth, L1ChainConfig, L1ConfigFile, default_path as l1_config_path,
};

/// Parse one `--l1 <chain>=<url>` argument.
///
/// Credentials ride in the URL's userinfo, which keeps one flag per chain
/// instead of the three that separate user and password fields would need.
fn parse_l1_arg(arg: &str) -> anyhow::Result<(ParentChainType, L1ChainConfig)> {
    let (chain, url) = arg.split_once('=').ok_or_else(|| {
        anyhow::anyhow!("expected <chain>=<url>, got '{arg}'")
    })?;
    let chain: ParentChainType = chain.parse().map_err(|_| {
        anyhow::anyhow!(
            "unknown parent chain '{chain}', use one of: {}",
            ParentChainType::all()
                .iter()
                .map(|chain| chain.to_string().to_lowercase())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let mut parsed = url::Url::parse(url)
        .map_err(|err| anyhow::anyhow!("invalid URL '{url}': {err}"))?;
    let auth =
        L1Auth::basic(parsed.username(), parsed.password().unwrap_or_default());
    // Strip the credentials back out so they are not duplicated in the URL.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    Ok((
        chain,
        L1ChainConfig {
            url: parsed.to_string(),
            auth,
            ..L1ChainConfig::basic("", "", "")
        },
    ))
}

/// Seed the L1 config with the endpoints that used to be built into the binary.
///
/// Until now the GUI silently fell back to two hardcoded endpoints for any
/// chain the user had not configured, so Signet and BCH appeared to work with
/// no configuration at all. That fallback is gone. Writing the same two entries
/// once, when there is no config file yet, keeps those users working — and
/// makes what was previously invisible into something they can see, edit, or
/// delete.
fn seed_legacy_l1_config() -> anyhow::Result<bool> {
    const LEGACY_ENDPOINTS: [(ParentChainType, &str); 2] = [
        (
            ParentChainType::Signet,
            "http://user:password@localhost:38332",
        ),
        (
            ParentChainType::BCH,
            "http://user:password@173.230.135.236:28332",
        ),
    ];

    let path = l1_config_path();
    if path.exists() {
        return Ok(false);
    }
    let mut config = L1ConfigFile::default();
    for (chain, url) in LEGACY_ENDPOINTS {
        let (_, entry) = parse_l1_arg(&format!("{chain}={url}"))?;
        config.insert(chain, entry);
    }
    config.save(&path)?;
    Ok(true)
}

/// Merge `--l1` arguments into the config file, leaving other chains alone.
fn write_l1_config_from_flags(args: &[String]) -> anyhow::Result<Vec<String>> {
    let path = l1_config_path();
    let mut config = L1ConfigFile::load(&path)?;
    let mut written = Vec::new();
    for arg in args {
        let (chain, chain_config) = parse_l1_arg(arg)?;
        written.push(format!("{chain} -> {}", chain_config.url));
        config.insert(chain, chain_config);
    }
    config.save(&path)?;
    Ok(written)
}

fn main() -> anyhow::Result<()> {
    // Configure the allocator before Tokio spins up worker threads.
    configure_mimalloc();
    let cli = cli::Cli::parse();

    // Handle init subcommand: write L1 config and exit
    if let Some(cli::AppSubcommand::Init { l1 }) = &cli.command {
        let written = write_l1_config_from_flags(l1)?;
        tracing_subscriber::fmt()
            .with_writer(std::io::stdout)
            .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
            .with_target(false)
            .init();
        tracing::info!(
            "L1 config written to {}: {}",
            l1_config_path().display(),
            if written.is_empty() {
                "no chains given".to_string()
            } else {
                written.join(", ")
            }
        );
        return Ok(());
    }

    let seeded_legacy_config = seed_legacy_l1_config()?;
    if !cli.run.l1.is_empty() {
        write_l1_config_from_flags(&cli.run.l1)?;
    }
    let config = cli.run.get_config()?;
    let (line_buffer, _rolling_log_guard) = set_tracing_subscriber(
        config.log_dir.as_deref(),
        config.log_level,
        config.log_level_file,
    )?;

    if seeded_legacy_config {
        tracing::warn!(
            path = %l1_config_path().display(),
            "No L1 config found; wrote the endpoints that were previously built \
             into the binary (Bitcoin Signet on localhost, Bitcoin Cash \
             testnet4 on a third-party host). Review or delete them — Coinshift \
             trusts whatever these endpoints say about L1 payments."
        );
    }

    let (app_tx, app_rx) = oneshot::channel::<anyhow::Error>();

    let app = app::App::new(&config).inspect(|app| {
        // spawn rpc server
        app.runtime.spawn({
            let app = app.clone();
            async move {
                tracing::info!("starting RPC server at `{}`", config.rpc_addr);
                if let Err(err) =
                    rpc_server::run_server(app, config.rpc_addr).await
                {
                    app_tx.send(err).expect("failed to send error to app");
                }
            }
        });
    });

    if !config.headless {
        // For GUI mode we want the GUI to start, even if the app fails to start.
        // User can update L1 config in the L1 Config pane and retry startup.
        return run_egui_app(config, line_buffer, app)
            .map_err(|e| anyhow::anyhow!("failed to run egui app: {e:#}"));
    }

    tracing::info!("Running in headless mode");
    drop(line_buffer);

    // If we're headless, we want to exit hard if the app fails to start.
    let app = app?;

    app.runtime.block_on(async move {
        tokio::select! {
            Ok(_) = ctrl_c() => {
                tracing::info!("Shutting down due to process interruption");
                Ok(())
            }
            Ok(err) = app_rx => {
                Err(anyhow::anyhow!("received error from RPC server: {err:#} ({err:?})"))
            }
        }
    })
}
