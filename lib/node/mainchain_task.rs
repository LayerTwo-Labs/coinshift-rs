//! Task to communicate with mainchain node

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bitcoin::{self, hashes::Hash as _};
use futures::{
    StreamExt,
    channel::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
};
use sneed::{EnvError, RwTxnError};
use thiserror::Error;
use tokio::{
    spawn,
    task::{self, JoinHandle},
};

use crate::{
    archive::{self, Archive},
    types::proto::{self, mainchain},
};

/// Request data from the mainchain node
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Request {
    /// Request missing mainchain ancestor header/infos
    AncestorInfos(bitcoin::BlockHash),
}

/// Error included in a response
#[derive(Debug, Error)]
pub enum ResponseError {
    #[error("Archive error")]
    Archive(#[from] archive::Error),
    #[error("Database env error")]
    DbEnv(#[from] EnvError),
    #[error("Database write error")]
    DbWrite(#[from] sneed::rwtxn::Error),
    #[error("CUSF Mainchain proto error")]
    Mainchain(#[from] proto::Error),
}

/// Response indicating that a request has been fulfilled
#[derive(Debug)]
pub(super) enum Response {
    /// Response bool indicates if the requested header was available
    AncestorInfos(bitcoin::BlockHash, Result<bool, ResponseError>),
}

impl From<&Response> for Request {
    fn from(resp: &Response) -> Self {
        match resp {
            Response::AncestorInfos(block_hash, _) => {
                Request::AncestorInfos(*block_hash)
            }
        }
    }
}

#[derive(Debug, Error)]
enum Error {
    #[error("Send response error")]
    SendResponse(Response),
    #[error("Send response error (oneshot)")]
    SendResponseOneshot(Response),
}

struct MainchainTask<Transport = tonic::transport::Channel> {
    env: sneed::Env,
    archive: Archive,
    mainchain: proto::mainchain::ValidatorClient<Transport>,
    // receive a request, and optional oneshot sender to send the result to
    // instead of sending on `response_tx`
    request_rx: UnboundedReceiver<(Request, Option<oneshot::Sender<Response>>)>,
    response_tx: UnboundedSender<Response>,
}

impl<Transport> MainchainTask<Transport>
where
    Transport: proto::Transport,
{
    /// Request ancestor header info and block info from the mainchain node,
    /// including the specified header.
    /// Returns `false` if the specified block was not available.
    async fn request_ancestor_infos(
        env: &sneed::Env,
        archive: &Archive,
        cusf_mainchain: &mut proto::mainchain::ValidatorClient<Transport>,
        block_hash: bitcoin::BlockHash,
    ) -> Result<bool, ResponseError> {
        let start_time = Instant::now();
        if block_hash == bitcoin::BlockHash::all_zeros() {
            return Ok(true);
        } else {
            let rotxn = env.read_txn().map_err(EnvError::from)?;
            if archive
                .try_get_main_header_info(&rotxn, &block_hash)?
                .is_some()
            {
                tracing::debug!(%block_hash, "request_ancestor_infos: block already in archive");
                return Ok(true);
            }
        }
        tracing::info!(%block_hash, "request_ancestor_infos: Starting to request ancestor headers/info");
        let mut current_block_hash = block_hash;
        let mut current_height = None;
        let mut block_infos =
            Vec::<(mainchain::BlockHeaderInfo, mainchain::BlockInfo)>::new();
        const LOG_PROGRESS_INTERVAL: Duration = Duration::from_secs(5);
        const BATCH_REQUEST_SIZE: u32 = 1000;
        let mut progress_logged = Instant::now();
        let mut batch_count = 0;
        loop {
            batch_count += 1;
            let batch_start = Instant::now();
            if let Some(current_height) = current_height {
                let now = Instant::now();
                if now.duration_since(progress_logged) >= LOG_PROGRESS_INTERVAL
                {
                    progress_logged = now;
                    tracing::info!(
                        %block_hash,
                        batch = batch_count,
                        blocks_collected = block_infos.len(),
                        current_block = %current_block_hash,
                        height_remaining = current_height,
                        elapsed_secs = start_time.elapsed().as_secs_f64(),
                        "request_ancestor_infos: Requesting ancestor headers batch"
                    );
                }
                tracing::debug!(%block_hash, batch = batch_count, "requesting ancestor headers: {current_block_hash}({current_height})")
            } else {
                tracing::info!(
                    %block_hash,
                    batch = batch_count,
                    "request_ancestor_infos: Requesting first batch from tip"
                );
            }
            let Some(block_infos_resp) = cusf_mainchain
                .get_block_infos(current_block_hash, BATCH_REQUEST_SIZE - 1)
                .await?
            else {
                tracing::warn!(%block_hash, "request_ancestor_infos: Block not available from mainchain");
                return Ok(false);
            };
            let batch_elapsed = batch_start.elapsed();
            let batch_size = block_infos_resp.len();
            {
                let (current_header, _) = block_infos_resp.last();
                current_block_hash = current_header.prev_block_hash;
                current_height = current_header.height.checked_sub(1);
            }
            block_infos.extend(block_infos_resp);
            tracing::info!(
                %block_hash,
                batch = batch_count,
                batch_size = batch_size,
                total_blocks = block_infos.len(),
                batch_elapsed_secs = batch_elapsed.as_secs_f64(),
                next_height = ?current_height,
                "request_ancestor_infos: Received batch, processing next"
            );
            if current_block_hash == bitcoin::BlockHash::all_zeros() {
                tracing::info!(%block_hash, "request_ancestor_infos: Reached genesis block");
                break;
            } else {
                let rotxn = env.read_txn().map_err(EnvError::from)?;
                if archive
                    .try_get_main_header_info(&rotxn, &current_block_hash)?
                    .is_some()
                {
                    tracing::info!(
                        %block_hash,
                        found_at = %current_block_hash,
                        "request_ancestor_infos: Found existing block in archive, stopping"
                    );
                    break;
                }
            }
        }
        let fetch_elapsed = start_time.elapsed();
        tracing::info!(
            %block_hash,
            total_blocks = block_infos.len(),
            batches = batch_count,
            fetch_elapsed_secs = fetch_elapsed.as_secs_f64(),
            "request_ancestor_infos: Finished fetching, reversing and storing"
        );
        block_infos.reverse();
        // Writing all headers during IBD can starve archive readers.
        let store_start = Instant::now();
        tracing::info!(%block_hash, "request_ancestor_infos: Starting to store ancestor headers/info to archive");
        let stored_count: usize =
            task::block_in_place(|| -> Result<usize, ResponseError> {
                let mut rwtxn = env.write_txn().map_err(EnvError::from)?;
                let mut stored_count = 0;
                for (header_info, block_info) in block_infos {
                    let () = archive
                        .put_main_header_info(&mut rwtxn, &header_info)?;
                    let () = archive.put_main_block_info(
                        &mut rwtxn,
                        header_info.block_hash,
                        &block_info,
                    )?;
                    stored_count += 1;
                    if stored_count % 1000 == 0 {
                        tracing::info!(
                            %block_hash,
                            stored = stored_count,
                            "request_ancestor_infos: Stored {} blocks so far",
                            stored_count
                        );
                    }
                }
                rwtxn.commit().map_err(RwTxnError::from)?;
                Ok(stored_count)
            })?;
        let store_elapsed = store_start.elapsed();
        let total_elapsed = start_time.elapsed();
        tracing::info!(
            %block_hash,
            blocks_stored = stored_count,
            store_elapsed_secs = store_elapsed.as_secs_f64(),
            total_elapsed_secs = total_elapsed.as_secs_f64(),
            "request_ancestor_infos: Successfully stored all ancestor headers/info"
        );
        Ok(true)
    }

    async fn run(mut self) -> Result<(), Error> {
        while let Some((request, response_tx)) = self.request_rx.next().await {
            match request {
                Request::AncestorInfos(main_block_hash) => {
                    let res = Self::request_ancestor_infos(
                        &self.env,
                        &self.archive,
                        &mut self.mainchain,
                        main_block_hash,
                    )
                    .await;
                    let response =
                        Response::AncestorInfos(main_block_hash, res);
                    if let Some(response_tx) = response_tx {
                        response_tx
                            .send(response)
                            .map_err(Error::SendResponseOneshot)?;
                    } else {
                        self.response_tx.unbounded_send(response).map_err(
                            |err| Error::SendResponse(err.into_inner()),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Handle to the task to communicate with mainchain node.
/// Task is aborted on drop.
#[derive(Clone)]
pub(super) struct MainchainTaskHandle {
    task: Arc<JoinHandle<()>>,
    // send a request, and optional oneshot sender to receive the result on the
    // corresponding oneshot receiver
    request_tx:
        mpsc::UnboundedSender<(Request, Option<oneshot::Sender<Response>>)>,
}

impl MainchainTaskHandle {
    pub fn new<Transport>(
        env: sneed::Env,
        archive: Archive,
        mainchain: mainchain::ValidatorClient<Transport>,
    ) -> (Self, mpsc::UnboundedReceiver<Response>)
    where
        Transport: proto::Transport + Send + 'static,
        <Transport as tonic::client::GrpcService<tonic::body::BoxBody>>::Future:
            Send,
    {
        let (request_tx, request_rx) = mpsc::unbounded();
        let (response_tx, response_rx) = mpsc::unbounded();
        let task = MainchainTask {
            env,
            archive,
            mainchain,
            request_rx,
            response_tx,
        };
        let task = spawn(async move {
            if let Err(err) = task.run().await {
                let err = anyhow::Error::from(err);
                tracing::error!("Mainchain task error: {err:#}");
            }
        });
        let task_handle = MainchainTaskHandle {
            task: Arc::new(task),
            request_tx,
        };
        (task_handle, response_rx)
    }

    /// Send a request
    pub fn request(&self, request: Request) -> Result<(), Request> {
        self.request_tx
            .unbounded_send((request, None))
            .map_err(|err| {
                let (request, _) = err.into_inner();
                request
            })
    }

    /// Send a request, and receive the response on a oneshot receiver instead
    /// of the response stream
    pub fn request_oneshot(
        &self,
        request: Request,
    ) -> Result<oneshot::Receiver<Response>, Request> {
        let (oneshot_tx, oneshot_rx) = oneshot::channel();
        let () = self
            .request_tx
            .unbounded_send((request, Some(oneshot_tx)))
            .map_err(|err| {
                let (request, _) = err.into_inner();
                request
            })?;
        Ok(oneshot_rx)
    }
}

impl Drop for MainchainTaskHandle {
    // If only one reference exists (ie. within self), abort the net task.
    fn drop(&mut self) {
        // use `Arc::get_mut` since `Arc::into_inner` requires ownership of the
        // Arc, and cloning would increase the reference count
        if let Some(task) = Arc::get_mut(&mut self.task) {
            task.abort()
        }
    }
}
