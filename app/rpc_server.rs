use std::net::SocketAddr;

use bitcoin::Amount;
use coinshift::{
    net::Peer,
    types::{
        Address, ParentChainType, PointedOutput, Swap, SwapId, SwapState,
        SwapTxId, Txid, WithdrawalBundle,
    },
    wallet::Balance,
};
use coinshift_app_rpc_api::RpcServer;
use jsonrpsee::{
    core::{RpcResult, async_trait, middleware::RpcServiceBuilder},
    server::Server,
    types::ErrorObject,
};
use tower_http::{
    request_id::{
        MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
    },
    trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer},
};

use crate::app::App;

pub struct RpcServerImpl {
    app: App,
}

fn custom_err_msg(err_msg: impl Into<String>) -> ErrorObject<'static> {
    ErrorObject::owned(-1, err_msg.into(), Option::<()>::None)
}

fn custom_err<Error>(error: Error) -> ErrorObject<'static>
where
    anyhow::Error: From<Error>,
{
    let error = anyhow::Error::from(error);
    custom_err_msg(format!("{error:#}"))
}
#[async_trait]
impl RpcServer for RpcServerImpl {
    async fn balance(&self) -> RpcResult<Balance> {
        self.app.wallet.get_balance().map_err(custom_err)
    }

    async fn create_deposit(
        &self,
        address: Address,
        value_sats: u64,
        fee_sats: u64,
    ) -> RpcResult<bitcoin::Txid> {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || {
            app.deposit(
                address,
                bitcoin::Amount::from_sat(value_sats),
                bitcoin::Amount::from_sat(fee_sats),
            )
            .map_err(custom_err)
        })
        .await
        .unwrap()
    }

    async fn connect_peer(&self, addr: SocketAddr) -> RpcResult<()> {
        self.app.node.connect_peer(addr).map_err(custom_err)
    }

    async fn format_deposit_address(
        &self,
        address: Address,
    ) -> RpcResult<String> {
        let deposit_address = address.format_for_deposit();
        Ok(deposit_address)
    }

    async fn forget_peer(&self, addr: SocketAddr) -> RpcResult<()> {
        match self.app.node.forget_peer(&addr) {
            Ok(_) => Ok(()),
            Err(err) => Err(custom_err(err)),
        }
    }

    async fn generate_mnemonic(&self) -> RpcResult<String> {
        let mnemonic = bip39::Mnemonic::new(
            bip39::MnemonicType::Words12,
            bip39::Language::English,
        );
        Ok(mnemonic.to_string())
    }

    async fn get_block(
        &self,
        block_hash: coinshift::types::BlockHash,
    ) -> RpcResult<Option<coinshift::types::Block>> {
        let Some(header) = self
            .app
            .node
            .try_get_header(block_hash)
            .map_err(custom_err)?
        else {
            return Ok(None);
        };
        let body = self.app.node.get_body(block_hash).map_err(custom_err)?;
        let block = coinshift::types::Block { header, body };
        Ok(Some(block))
    }

    async fn get_best_sidechain_block_hash(
        &self,
    ) -> RpcResult<Option<coinshift::types::BlockHash>> {
        self.app.node.try_get_tip().map_err(custom_err)
    }

    async fn get_best_mainchain_block_hash(
        &self,
    ) -> RpcResult<Option<bitcoin::BlockHash>> {
        let Some(sidechain_hash) =
            self.app.node.try_get_tip().map_err(custom_err)?
        else {
            // No sidechain tip, so no best mainchain block hash.
            return Ok(None);
        };
        let block_hash = self
            .app
            .node
            .get_best_main_verification(sidechain_hash)
            .map_err(custom_err)?;
        Ok(Some(block_hash))
    }

    async fn get_bmm_inclusions(
        &self,
        block_hash: coinshift::types::BlockHash,
    ) -> RpcResult<Vec<bitcoin::BlockHash>> {
        self.app
            .node
            .get_bmm_inclusions(block_hash)
            .map_err(custom_err)
    }

    async fn get_new_address(&self) -> RpcResult<Address> {
        self.app.wallet.get_new_address().map_err(custom_err)
    }

    async fn get_wallet_addresses(&self) -> RpcResult<Vec<Address>> {
        let addrs = self.app.wallet.get_addresses().map_err(custom_err)?;
        let mut res: Vec<_> = addrs.into_iter().collect();
        res.sort_by_key(|addr| addr.as_base58());
        Ok(res)
    }

    async fn get_wallet_utxos(&self) -> RpcResult<Vec<PointedOutput>> {
        let utxos = self.app.wallet.get_utxos().map_err(custom_err)?;
        let utxos = utxos
            .into_iter()
            .map(|(outpoint, output)| PointedOutput { outpoint, output })
            .collect();
        Ok(utxos)
    }

    async fn getblockcount(&self) -> RpcResult<u32> {
        let height = self.app.node.try_get_height().map_err(custom_err)?;
        let block_count = height.map_or(0, |height| height + 1);
        Ok(block_count)
    }

    async fn latest_failed_withdrawal_bundle_height(
        &self,
    ) -> RpcResult<Option<u32>> {
        let height = self
            .app
            .node
            .get_latest_failed_withdrawal_bundle_height()
            .map_err(custom_err)?;
        Ok(height)
    }

    async fn list_peers(&self) -> RpcResult<Vec<Peer>> {
        let peers = self.app.node.get_active_peers();
        Ok(peers)
    }

    async fn list_utxos(&self) -> RpcResult<Vec<PointedOutput>> {
        let utxos = self.app.node.get_all_utxos().map_err(custom_err)?;
        let res = utxos
            .into_iter()
            .map(|(outpoint, output)| PointedOutput { outpoint, output })
            .collect();
        Ok(res)
    }

    async fn mine(&self, fee: Option<u64>) -> RpcResult<()> {
        let fee = fee.map(bitcoin::Amount::from_sat);
        self.app
            .local_pool
            .spawn_pinned({
                let app = self.app.clone();
                move || async move { app.mine(fee).await.map_err(custom_err) }
            })
            .await
            .unwrap()
    }

    async fn pending_withdrawal_bundle(
        &self,
    ) -> RpcResult<Option<WithdrawalBundle>> {
        self.app
            .node
            .get_pending_withdrawal_bundle()
            .map_err(custom_err)
    }

    async fn openapi_schema(&self) -> RpcResult<utoipa::openapi::OpenApi> {
        let res = <coinshift_app_rpc_api::RpcDoc as utoipa::OpenApi>::openapi();
        Ok(res)
    }

    async fn remove_from_mempool(&self, txid: Txid) -> RpcResult<()> {
        self.app.node.remove_from_mempool(txid).map_err(custom_err)
    }

    async fn set_seed_from_mnemonic(&self, mnemonic: String) -> RpcResult<()> {
        let mnemonic =
            bip39::Mnemonic::from_phrase(&mnemonic, bip39::Language::English)
                .map_err(custom_err)?;
        let seed = bip39::Seed::new(&mnemonic, "");
        let seed_bytes: [u8; 64] = seed.as_bytes().try_into().map_err(
            |err: <[u8; 64] as TryFrom<&[u8]>>::Error| custom_err(err),
        )?;
        self.app.wallet.set_seed(&seed_bytes).map_err(custom_err)
    }

    async fn sidechain_wealth_sats(&self) -> RpcResult<u64> {
        let sidechain_wealth =
            self.app.node.get_sidechain_wealth().map_err(custom_err)?;
        Ok(sidechain_wealth.to_sat())
    }

    async fn stop(&self) {
        std::process::exit(0);
    }

    async fn transfer(
        &self,
        dest: Address,
        value_sats: u64,
        fee_sats: u64,
    ) -> RpcResult<Txid> {
        let accumulator =
            self.app.node.get_tip_accumulator().map_err(custom_err)?;
        let tx = self
            .app
            .wallet
            .create_transaction(
                &accumulator,
                dest,
                Amount::from_sat(value_sats),
                Amount::from_sat(fee_sats),
            )
            .map_err(custom_err)?;
        let txid = tx.txid();
        self.app.sign_and_send(tx).map_err(custom_err)?;
        Ok(txid)
    }

    async fn withdraw(
        &self,
        mainchain_address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
        amount_sats: u64,
        fee_sats: u64,
        mainchain_fee_sats: u64,
    ) -> RpcResult<Txid> {
        let accumulator =
            self.app.node.get_tip_accumulator().map_err(custom_err)?;
        let tx = self
            .app
            .wallet
            .create_withdrawal(
                &accumulator,
                mainchain_address,
                Amount::from_sat(amount_sats),
                Amount::from_sat(mainchain_fee_sats),
                Amount::from_sat(fee_sats),
            )
            .map_err(custom_err)?;
        let txid = tx.txid();
        self.app.sign_and_send(tx).map_err(custom_err)?;
        Ok(txid)
    }

    async fn create_swap(
        &self,
        parent_chain: ParentChainType,
        l1_recipient_address: String,
        l1_amount_sats: u64,
        l2_recipient: Option<Address>, // Optional - None = open swap
        l2_amount_sats: u64,
        required_confirmations: Option<u32>,
        fee_sats: u64,
    ) -> RpcResult<(SwapId, Txid)> {
        let accumulator =
            self.app.node.get_tip_accumulator().map_err(custom_err)?;

        // Create a closure that checks if an outpoint is locked to a swap
        // We create a new read transaction each time to avoid lifetime issues
        // This ensures we always read the latest state
        let node = &self.app.node;
        let is_locked = |outpoint: &coinshift::types::OutPoint| -> bool {
            let rotxn = match node.env().read_txn() {
                Ok(txn) => txn,
                Err(_) => {
                    tracing::warn!(
                        "Failed to create read transaction for locked output check"
                    );
                    return false;
                }
            };
            let state = node.state();
            match state.is_output_locked_to_swap(&rotxn, outpoint) {
                Ok(Some(_)) => {
                    tracing::debug!(outpoint = ?outpoint, "Output is locked to a swap");
                    true
                }
                Ok(None) => false,
                Err(err) => {
                    tracing::warn!(outpoint = ?outpoint, error = %err, "Error checking if output is locked");
                    false
                }
            }
        };

        let (tx, swap_id) = self
            .app
            .wallet
            .create_swap_create_tx(
                &accumulator,
                parent_chain,
                l1_recipient_address,
                Amount::from_sat(l1_amount_sats),
                l2_recipient, // Optional
                Amount::from_sat(l2_amount_sats),
                required_confirmations,
                Amount::from_sat(fee_sats),
                is_locked,
            )
            .map_err(custom_err)?;
        let txid = tx.txid();
        self.app.sign_and_send(tx).map_err(custom_err)?;
        Ok((swap_id, txid))
    }

    async fn reconstruct_swaps(&self) -> RpcResult<u32> {
        let mut rwtxn = self.app.node.env().write_txn().map_err(custom_err)?;
        let count = self
            .app
            .node
            .state()
            .reconstruct_swaps_from_blockchain(
                &mut rwtxn,
                self.app.node.archive(),
                None, // Use current tip
            )
            .map_err(custom_err)?;
        rwtxn.commit().map_err(custom_err)?;
        Ok(count)
    }

    async fn update_swap_l1_txid(
        &self,
        swap_id: SwapId,
        l1_txid_hex: String,
        confirmations: u32,
        l2_claimer_address: Option<Address>,
    ) -> RpcResult<()> {
        let l1_txid =
            SwapTxId::from_hex(&l1_txid_hex).map_err(custom_err_msg)?;

        let mut rwtxn = self.app.node.env().write_txn().map_err(custom_err)?;

        // Get current sidechain block hash and height for reference
        let block_hash = self
            .app
            .node
            .state()
            .try_get_tip(&rwtxn)
            .map_err(custom_err)?
            .ok_or_else(|| custom_err_msg("No tip found"))?;
        let block_height = self
            .app
            .node
            .state()
            .try_get_height(&rwtxn)
            .map_err(custom_err)?
            .ok_or_else(|| custom_err_msg("No tip height found"))?;

        self.app
            .node
            .state()
            .update_swap_l1_txid(
                &mut rwtxn,
                &swap_id,
                l1_txid,
                confirmations,
                None, // l1_claimer_address
                l2_claimer_address,
                block_hash,
                block_height,
            )
            .map_err(custom_err)?;
        rwtxn.commit().map_err(custom_err)?;
        Ok(())
    }

    async fn get_swap_status(
        &self,
        swap_id: SwapId,
    ) -> RpcResult<Option<Swap>> {
        let rotxn = self.app.node.env().read_txn().map_err(custom_err)?;
        let swap = self
            .app
            .node
            .state()
            .get_swap(&rotxn, &swap_id)
            .map_err(custom_err)?;
        Ok(swap)
    }

    async fn claim_swap(
        &self,
        swap_id: SwapId,
        l2_claimer_address: Option<Address>,
    ) -> RpcResult<Txid> {
        // Get swap to verify it's ready and get recipient
        let rotxn = self.app.node.env().read_txn().map_err(custom_err)?;
        let swap = self
            .app
            .node
            .state()
            .get_swap(&rotxn, &swap_id)
            .map_err(custom_err)?
            .ok_or_else(|| custom_err_msg("Swap not found"))?;

        if !matches!(swap.state, SwapState::ReadyToClaim) {
            return Err(custom_err_msg(format!(
                "Swap is not ready to claim (state: {:?})",
                swap.state
            )));
        }

        // Get locked outputs for this swap
        // Note: We must query the node directly, not the wallet, because the wallet
        // filters out SwapPending outputs. Locked outputs are identified by checking
        // if the output content is SwapPending with the matching swap_id.
        let all_utxos = self.app.node.get_all_utxos().map_err(custom_err)?;

        // Find locked outputs for this swap (same pattern as verify_swap_locks_utxos in integration tests)
        let mut locked_outputs = Vec::new();
        for (outpoint, output) in all_utxos {
            match &output.content {
                coinshift::types::OutputContent::SwapPending {
                    swap_id: locked_swap_id,
                    ..
                } => {
                    if *locked_swap_id == swap_id.0 {
                        tracing::info!(
                            "Found locked output for swap {}: {:?}",
                            swap_id,
                            outpoint
                        );
                        locked_outputs.push((outpoint, output));
                    } else {
                        tracing::debug!(
                            "Output {:?} is SwapPending for different swap_id: {:?}",
                            outpoint,
                            locked_swap_id
                        );
                    }
                }
                other => {
                    tracing::trace!(
                        "Output {:?} is not SwapPending (content: {:?})",
                        outpoint,
                        other
                    );
                }
            }
        }

        if locked_outputs.is_empty() {
            return Err(custom_err_msg(format!(
                "No locked outputs found for swap {}",
                swap_id
            )));
        }

        // Determine recipient: pre-specified uses swap.l2_recipient; open uses stored or provided claimer address
        let recipient = swap
            .l2_recipient
            .or(swap.l2_claimer_address)
            .or(l2_claimer_address)
            .ok_or_else(|| {
                custom_err_msg("Open swap requires l2_claimer_address (or set when L1 tx was submitted)")
            })?;

        // Add locked outputs to wallet temporarily so they can be used for signing
        // SwapPending outputs are normally filtered out, but we need them in the wallet
        // for the authorize() call to find the address and signing key
        use std::collections::HashMap;
        let locked_utxos: HashMap<_, _> =
            locked_outputs.iter().cloned().collect();
        self.app
            .wallet
            .put_utxos(&locked_utxos)
            .map_err(custom_err)?;
        tracing::debug!(
            swap_id = %swap_id,
            num_locked_outputs = locked_outputs.len(),
            "Added locked outputs to wallet for signing"
        );

        let accumulator =
            self.app.node.get_tip_accumulator().map_err(custom_err)?;
        let l2_claimer_for_tx =
            swap.l2_recipient.is_none().then_some(recipient);
        let tx = self
            .app
            .wallet
            .create_swap_claim_tx(
                &accumulator,
                swap_id,
                recipient,
                locked_outputs,
                l2_claimer_for_tx,
            )
            .map_err(custom_err)?;
        let txid = tx.txid();
        self.app.sign_and_send(tx).map_err(custom_err)?;
        Ok(txid)
    }

    async fn list_swaps(&self) -> RpcResult<Vec<Swap>> {
        let rotxn = self.app.node.env().read_txn().map_err(custom_err)?;
        let swaps = self
            .app
            .node
            .state()
            .load_all_swaps(&rotxn)
            .map_err(custom_err)?;
        Ok(swaps)
    }

    async fn list_swaps_by_recipient(
        &self,
        recipient: Address,
    ) -> RpcResult<Vec<Swap>> {
        let rotxn = self.app.node.env().read_txn().map_err(custom_err)?;
        let swaps = self
            .app
            .node
            .state()
            .get_swaps_by_recipient(&rotxn, &recipient)
            .map_err(custom_err)?;
        Ok(swaps)
    }
}

#[derive(Clone, Debug)]
struct RequestIdMaker;

impl MakeRequestId for RequestIdMaker {
    fn make_request_id<B>(
        &mut self,
        _: &http::Request<B>,
    ) -> Option<RequestId> {
        use uuid::Uuid;
        // the 'simple' format renders the UUID with no dashes, which
        // makes for easier copy/pasting.
        let id = Uuid::new_v4();
        let id = id.as_simple();
        let id = format!("req_{id}"); // prefix all IDs with "req_", to make them easier to identify

        let Ok(header_value) = http::HeaderValue::from_str(&id) else {
            return None;
        };

        Some(RequestId::new(header_value))
    }
}

pub async fn run_server(
    app: App,
    rpc_addr: SocketAddr,
) -> anyhow::Result<SocketAddr> {
    const REQUEST_ID_HEADER: &str = "x-request-id";

    // Ordering here matters! Order here is from official docs on request IDs tracings
    // https://docs.rs/tower-http/latest/tower_http/request_id/index.html#using-trace
    let tracer = tower::ServiceBuilder::new()
        .layer(SetRequestIdLayer::new(
            http::HeaderName::from_static(REQUEST_ID_HEADER),
            RequestIdMaker,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(move |request: &http::Request<_>| {
                    let request_id = request
                        .headers()
                        .get(http::HeaderName::from_static(REQUEST_ID_HEADER))
                        .and_then(|h| h.to_str().ok())
                        .filter(|s| !s.is_empty());

                    tracing::span!(
                        tracing::Level::DEBUG,
                        "request",
                        method = %request.method(),
                        uri = %request.uri(),
                        request_id , // this is needed for the record call below to work
                    )
                })
                .on_request(())
                .on_eos(())
                .on_response(
                    DefaultOnResponse::new().level(tracing::Level::INFO),
                )
                .on_failure(
                    DefaultOnFailure::new().level(tracing::Level::ERROR),
                ),
        )
        .layer(PropagateRequestIdLayer::new(http::HeaderName::from_static(
            REQUEST_ID_HEADER,
        )))
        .into_inner();

    let http_middleware = tower::ServiceBuilder::new().layer(tracer);
    let rpc_middleware = RpcServiceBuilder::new().rpc_logger(1024);

    let server = Server::builder()
        .set_http_middleware(http_middleware)
        .set_rpc_middleware(rpc_middleware)
        .build(rpc_addr)
        .await?;

    let addr = server.local_addr()?;

    let handle = server.start(RpcServerImpl { app }.into_rpc());

    // In this example we don't care about doing shutdown so let's it run forever.
    // You may use the `ServerHandle` to shut it down or manage it yourself.
    tokio::spawn(handle.stopped());

    Ok(addr)
}
