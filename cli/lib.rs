use std::{net::SocketAddr, time::Duration};

use clap::{Parser, Subcommand};
use http::HeaderMap;
use jsonrpsee::{core::client::ClientT, http_client::HttpClientBuilder};

use coinshift::types::{Address, ParentChainType, SwapId, Txid};
use coinshift_app_rpc_api::RpcClient;
use tracing_subscriber::{filter::Targets, layer::SubscriberExt as _};

fn parse_swap_id(s: &str) -> anyhow::Result<SwapId> {
    let bytes = hex::decode(s).map_err(|e| anyhow::anyhow!("invalid swap_id hex: {}", e))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("swap_id must be 32 bytes (64 hex chars)"))?;
    Ok(SwapId(arr))
}

fn parse_parent_chain(s: &str) -> anyhow::Result<ParentChainType> {
    match s.to_lowercase().as_str() {
        "btc" => Ok(ParentChainType::BTC),
        "bch" => Ok(ParentChainType::BCH),
        "ltc" => Ok(ParentChainType::LTC),
        "signet" => Ok(ParentChainType::Signet),
        "regtest" => Ok(ParentChainType::Regtest),
        _ => anyhow::bail!("unknown parent chain: {} (use btc, bch, ltc, signet, regtest)", s),
    }
}

#[derive(Clone, Debug, Subcommand)]
#[command(arg_required_else_help(true))]
pub enum Command {
    /// Get balance in sats
    Balance,
    /// Connect to a peer
    ConnectPeer { addr: SocketAddr },
    /// Create a swap (L2 → L1). Optional l2_recipient = open swap.
    CreateSwap {
        #[arg(long, value_parser = parse_parent_chain)]
        parent_chain: ParentChainType,
        #[arg(long)]
        l1_recipient_address: String,
        #[arg(long)]
        l1_amount_sats: u64,
        #[arg(long)]
        l2_recipient: Option<Address>,
        #[arg(long)]
        l2_amount_sats: u64,
        #[arg(long)]
        required_confirmations: Option<u32>,
        #[arg(long)]
        fee_sats: u64,
    },
    /// Deposit to address
    CreateDeposit {
        address: Address,
        #[arg(long)]
        value_sats: u64,
        #[arg(long)]
        fee_sats: u64,
    },
    /// Format a deposit address
    FormatDepositAddress { address: Address },
    /// Delete peer from known_peers DB.
    /// Connections to the peer are not terminated.
    ForgetPeer { addr: SocketAddr },
    /// Generate a mnemonic seed phrase
    GenerateMnemonic,
    /// Get the best mainchain block hash
    GetBestMainchainBlockHash,
    /// Get the best sidechain block hash
    GetBestSidechainBlockHash,
    /// Get the block with specified block hash, if it exists
    GetBlock {
        block_hash: coinshift::types::BlockHash,
    },
    /// Get mainchain blocks that commit to a specified block hash
    GetBmmInclusions {
        block_hash: coinshift::types::BlockHash,
    },
    /// Get a new address
    GetNewAddress,
    /// Get wallet addresses, sorted by base58 encoding
    GetWalletAddresses,
    /// Get wallet UTXOs
    GetWalletUtxos,
    /// Get the current block count
    GetBlockcount,
    /// Claim a swap (after L1 has required confirmations). For open swaps, pass l2_claimer_address.
    ClaimSwap {
        #[arg(long, value_parser = parse_swap_id)]
        swap_id: SwapId,
        #[arg(long)]
        l2_claimer_address: Option<Address>,
    },
    /// Get the height of the latest failed withdrawal bundle
    LatestFailedWithdrawalBundleHeight,
    /// List peers
    ListPeers,
    /// List all UTXOs
    ListUtxos,
    /// Recover wallet from mnemonic phrase (sets seed, then shows addresses and balance)
    RecoverFromMnemonic { mnemonic: String },
    /// Reconstruct all swaps from the blockchain
    ReconstructSwaps,
    /// Attempt to mine a sidechain block
    Mine {
        #[arg(long)]
        fee_sats: Option<u64>,
    },
    /// Get pending withdrawal bundle
    PendingWithdrawalBundle,
    /// Show OpenAPI schema
    #[command(name = "openapi-schema")]
    OpenApiSchema,
    /// Remove a tx from the mempool
    RemoveFromMempool { txid: Txid },
    /// Set the wallet seed from a mnemonic seed phrase
    SetSeedFromMnemonic { mnemonic: String },
    /// Get total sidechain wealth
    SidechainWealth,
    /// Stop the node
    Stop,
    /// Transfer funds to the specified address
    Transfer {
        dest: Address,
        #[arg(long)]
        value_sats: u64,
        #[arg(long)]
        fee_sats: u64,
    },
    /// Update swap with L1 txid and confirmation count (for open swaps, pass l2_claimer_address).
    UpdateSwapL1Txid {
        #[arg(long, value_parser = parse_swap_id)]
        swap_id: SwapId,
        #[arg(long)]
        l1_txid_hex: String,
        #[arg(long)]
        confirmations: u32,
        #[arg(long)]
        l2_claimer_address: Option<Address>,
    },
    /// Initiate a withdrawal to the specified mainchain address
    Withdraw {
        mainchain_address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
        #[arg(long)]
        amount_sats: u64,
        #[arg(long)]
        fee_sats: u64,
        #[arg(long)]
        mainchain_fee_sats: u64,
    },
}

#[derive(Clone, Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Base URL used for requests to the RPC server.
    #[arg(default_value = "http://localhost:6255", long)]
    pub rpc_url: url::Url,

    #[arg(long, help = "Timeout for RPC requests in seconds (default: 300)")]
    pub timeout: Option<u64>,

    #[arg(short, long, help = "Enable verbose HTTP output")]
    pub verbose: bool,

    /// Log level
    #[arg(default_value_t = tracing::Level::INFO, long)]
    pub log_level: tracing::Level,

    #[command(subcommand)]
    pub command: Command,
}
/// Handle a command, returning CLI output
async fn handle_command<RpcClient>(
    rpc_client: &RpcClient,
    command: Command,
) -> anyhow::Result<String>
where
    RpcClient: ClientT + Sync,
{
    Ok(match command {
        Command::Balance => {
            let balance = rpc_client.balance().await?;
            serde_json::to_string_pretty(&balance)?
        }
        Command::ConnectPeer { addr } => {
            let () = rpc_client.connect_peer(addr).await?;
            String::default()
        }
        Command::CreateSwap {
            parent_chain,
            l1_recipient_address,
            l1_amount_sats,
            l2_recipient,
            l2_amount_sats,
            required_confirmations,
            fee_sats,
        } => {
            let (swap_id, txid) = rpc_client
                .create_swap(
                    parent_chain,
                    l1_recipient_address,
                    l1_amount_sats,
                    l2_recipient,
                    l2_amount_sats,
                    required_confirmations,
                    fee_sats,
                )
                .await?;
            format!("Swap created: id={} txid={}", swap_id, txid)
        }
        Command::ClaimSwap {
            swap_id,
            l2_claimer_address,
        } => {
            let txid = rpc_client.claim_swap(swap_id, l2_claimer_address).await?;
            format!("Swap claimed: txid={}", txid)
        }
        Command::CreateDeposit {
            address,
            value_sats,
            fee_sats,
        } => {
            let txid = rpc_client
                .create_deposit(address, value_sats, fee_sats)
                .await?;
            format!("{txid}")
        }
        Command::FormatDepositAddress { address } => {
            rpc_client.format_deposit_address(address).await?
        }
        Command::ForgetPeer { addr } => {
            rpc_client.forget_peer(addr).await?;
            String::default()
        }
        Command::GetBlock { block_hash } => {
            let block = rpc_client.get_block(block_hash).await?;
            serde_json::to_string_pretty(&block)?
        }
        Command::GetBestMainchainBlockHash => {
            let block_hash = rpc_client.get_best_mainchain_block_hash().await?;
            serde_json::to_string_pretty(&block_hash)?
        }
        Command::GetBestSidechainBlockHash => {
            let block_hash = rpc_client.get_best_sidechain_block_hash().await?;
            serde_json::to_string_pretty(&block_hash)?
        }
        Command::GetBmmInclusions { block_hash } => {
            let bmm_inclusions =
                rpc_client.get_bmm_inclusions(block_hash).await?;
            serde_json::to_string_pretty(&bmm_inclusions)?
        }
        Command::GenerateMnemonic => rpc_client.generate_mnemonic().await?,
        Command::GetNewAddress => {
            let address = rpc_client.get_new_address().await?;
            format!("{address}")
        }
        Command::GetWalletAddresses => {
            let addresses = rpc_client.get_wallet_addresses().await?;
            serde_json::to_string_pretty(&addresses)?
        }
        Command::GetWalletUtxos => {
            let utxos = rpc_client.get_wallet_utxos().await?;
            serde_json::to_string_pretty(&utxos)?
        }
        Command::GetBlockcount => {
            let blockcount = rpc_client.getblockcount().await?;
            format!("{blockcount}")
        }
        Command::LatestFailedWithdrawalBundleHeight => {
            let height =
                rpc_client.latest_failed_withdrawal_bundle_height().await?;
            serde_json::to_string_pretty(&height)?
        }
        Command::ListPeers => {
            let peers = rpc_client.list_peers().await?;
            serde_json::to_string_pretty(&peers)?
        }
        Command::ListUtxos => {
            let utxos = rpc_client.list_utxos().await?;
            serde_json::to_string_pretty(&utxos)?
        }
        Command::RecoverFromMnemonic { mnemonic } => {
            rpc_client.set_seed_from_mnemonic(mnemonic).await?;
            let addresses = rpc_client.get_wallet_addresses().await?;
            let balance = rpc_client.balance().await?;
            let addrs_json = serde_json::to_string_pretty(&addresses)?;
            format!(
                "Recovery complete.\nAddresses:\n{}\nBalance: total {} sats, available {} sats",
                addrs_json,
                balance.total.to_sat(),
                balance.available.to_sat()
            )
        }
        Command::ReconstructSwaps => {
            let count = rpc_client.reconstruct_swaps().await?;
            format!("Reconstructed {} swaps from blockchain", count)
        }
        Command::Mine { fee_sats } => {
            let () = rpc_client.mine(fee_sats).await?;
            String::default()
        }
        Command::PendingWithdrawalBundle => {
            let withdrawal_bundle =
                rpc_client.pending_withdrawal_bundle().await?;
            serde_json::to_string_pretty(&withdrawal_bundle)?
        }
        Command::OpenApiSchema => {
            let openapi =
                <coinshift_app_rpc_api::RpcDoc as utoipa::OpenApi>::openapi();
            openapi.to_pretty_json()?
        }
        Command::RemoveFromMempool { txid } => {
            let () = rpc_client.remove_from_mempool(txid).await?;
            String::default()
        }
        Command::SetSeedFromMnemonic { mnemonic } => {
            let () = rpc_client.set_seed_from_mnemonic(mnemonic).await?;
            String::default()
        }
        Command::SidechainWealth => {
            let sidechain_wealth = rpc_client.sidechain_wealth_sats().await?;
            format!("{sidechain_wealth}")
        }
        Command::Stop => {
            let () = rpc_client.stop().await?;
            String::default()
        }
        Command::Transfer {
            dest,
            value_sats,
            fee_sats,
        } => {
            let txid = rpc_client.transfer(dest, value_sats, fee_sats).await?;
            format!("{txid}")
        }
        Command::UpdateSwapL1Txid {
            swap_id,
            l1_txid_hex,
            confirmations,
            l2_claimer_address,
        } => {
            rpc_client
                .update_swap_l1_txid(
                    swap_id,
                    l1_txid_hex,
                    confirmations,
                    l2_claimer_address,
                )
                .await?;
            format!("Swap {} updated with L1 txid and confirmations", swap_id)
        }
        Command::Withdraw {
            mainchain_address,
            amount_sats,
            fee_sats,
            mainchain_fee_sats,
        } => {
            let txid = rpc_client
                .withdraw(
                    mainchain_address,
                    amount_sats,
                    fee_sats,
                    mainchain_fee_sats,
                )
                .await?;
            format!("{txid}")
        }
    })
}

fn set_tracing_subscriber(log_level: tracing::Level) -> anyhow::Result<()> {
    let filter = Targets::new().with_default(log_level);

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_file(true)
        .with_line_number(true);

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer);
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

impl Cli {
    pub async fn run(self) -> anyhow::Result<String> {
        set_tracing_subscriber(self.log_level)?;

        const DEFAULT_TIMEOUT: u64 = 300;

        let request_id = uuid::Uuid::new_v4().as_simple().to_string();

        tracing::info!("request ID: {}", request_id);

        let builder = HttpClientBuilder::default()
            .request_timeout(Duration::from_secs(
                self.timeout.unwrap_or(DEFAULT_TIMEOUT),
            ))
            .set_rpc_middleware(
                jsonrpsee::core::middleware::RpcServiceBuilder::new()
                    .rpc_logger(1024),
            )
            .set_headers(HeaderMap::from_iter([(
                http::header::HeaderName::from_static("x-request-id"),
                http::header::HeaderValue::from_str(&request_id)?,
            )]));

        let client = builder.build(self.rpc_url)?;
        let result = handle_command(&client, self.command).await?;
        Ok(result)
    }
}
