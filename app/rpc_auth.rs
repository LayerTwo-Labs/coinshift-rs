use std::{
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    task::{Context, Poll},
};

use jsonrpsee::{
    server::middleware::rpc::{
        Batch, BatchEntry, BatchEntryErr, MethodResponse, Notification,
        Request, ResponseFuture, RpcServiceT,
    },
    types::{ErrorObject, ErrorObjectOwned},
};
use rand::Rng as _;
use subtle::ConstantTimeEq as _;

const COOKIE_FILE_NAME: &str = ".cookie";

const UNAUTHORIZED_CODE: i32 = -32001;

/// Methods that read public chain state. Every other method needs the cookie
/// token, so a new method is guarded by default.
const PUBLIC_METHODS: &[&str] = &[
    "get_best_mainchain_block_hash",
    "get_best_sidechain_block_hash",
    "get_block",
    "get_block_hash",
    "get_block_index",
    "get_bmm_inclusions",
    "get_swap_status",
    "getblockcount",
    "latest_failed_withdrawal_bundle_height",
    "list_mempool",
    "list_peers",
    "list_swaps",
    "list_swaps_by_recipient",
    "list_utxos",
    "mainchain_sync_progress",
    "openapi_schema",
    "pending_withdrawal_bundle",
    "sidechain_wealth",
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("RPC cookie file `{}` is empty", .cookie_path.display())]
    Empty { cookie_path: PathBuf },
    #[error("failed to read RPC cookie file `{}`", .cookie_path.display())]
    Read {
        cookie_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "RPC cookie file `{}` holds whitespace inside the token",
        .cookie_path.display()
    )]
    Whitespace { cookie_path: PathBuf },
    #[error("failed to write RPC cookie file `{}`", .cookie_path.display())]
    Write {
        cookie_path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn cookie_path(datadir: &Path) -> PathBuf {
    datadir.join(COOKIE_FILE_NAME)
}

fn parse_cookie(cookie_path: &Path, content: &str) -> Result<String, Error> {
    let token = content.trim();
    if token.is_empty() {
        return Err(Error::Empty {
            cookie_path: cookie_path.to_owned(),
        });
    }
    if token.contains(char::is_whitespace) {
        return Err(Error::Whitespace {
            cookie_path: cookie_path.to_owned(),
        });
    }
    Ok(token.to_owned())
}

/// Writes through a temp file and a rename, so a crash cannot leave an
/// empty cookie that the next start refuses.
fn write_cookie(cookie_path: &Path, token: &str) -> io::Result<()> {
    let tmp_path = cookie_path.with_extension("tmp");
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp_path)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, cookie_path)
}

/// Returns the token in the cookie file, and creates the file with a new
/// random token if it does not exist.
pub fn load_or_create_cookie(cookie_path: &Path) -> Result<String, Error> {
    match fs::read_to_string(cookie_path) {
        Ok(content) => parse_cookie(cookie_path, &content),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let mut bytes = [0u8; 32];
            rand::rng().fill_bytes(&mut bytes);
            let token = hex::encode(bytes);
            write_cookie(cookie_path, &token).map_err(|source| {
                Error::Write {
                    cookie_path: cookie_path.to_owned(),
                    source,
                }
            })?;
            tracing::info!(
                cookie_path = %cookie_path.display(),
                "Wrote new RPC cookie file"
            );
            Ok(token)
        }
        Err(source) => Err(Error::Read {
            cookie_path: cookie_path.to_owned(),
            source,
        }),
    }
}

#[derive(Clone, Copy, Debug)]
struct RpcAuthenticated;

fn bearer_token_matches(headers: &http::HeaderMap, token: &[u8]) -> bool {
    let Some(value) = headers.get(http::header::AUTHORIZATION) else {
        return false;
    };
    let Some(sent) = value.as_bytes().strip_prefix(b"Bearer ") else {
        return false;
    };
    bool::from(sent.ct_eq(token))
}

/// HTTP layer that marks a request with a valid bearer token. It never
/// rejects a request, because public methods need no token.
#[derive(Clone)]
pub struct BearerAuthLayer {
    token: Arc<[u8]>,
}

impl BearerAuthLayer {
    pub fn new(token: &str) -> Self {
        Self {
            token: Arc::from(token.as_bytes()),
        }
    }
}

impl<S> tower::Layer<S> for BearerAuthLayer {
    type Service = BearerAuth<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BearerAuth {
            inner,
            token: self.token.clone(),
        }
    }
}

#[derive(Clone)]
pub struct BearerAuth<S> {
    inner: S,
    token: Arc<[u8]>,
}

impl<S, B> tower::Service<http::Request<B>> for BearerAuth<S>
where
    S: tower::Service<http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: http::Request<B>) -> Self::Future {
        if bearer_token_matches(request.headers(), &self.token) {
            request.extensions_mut().insert(RpcAuthenticated);
        }
        self.inner.call(request)
    }
}

/// RPC layer that refuses a method outside [`PUBLIC_METHODS`] unless
/// [`BearerAuth`] marked the request.
#[derive(Clone)]
pub struct RequireAuthLayer {
    unauthorized: ErrorObjectOwned,
}

impl RequireAuthLayer {
    pub fn new(cookie_path: &Path) -> Self {
        let message = format!(
            "unauthorized: send Authorization: Bearer <token from {}>",
            cookie_path.display()
        );
        Self {
            unauthorized: ErrorObject::owned(
                UNAUTHORIZED_CODE,
                message,
                None::<()>,
            ),
        }
    }
}

impl<S> tower::Layer<S> for RequireAuthLayer {
    type Service = RequireAuth<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequireAuth {
            inner,
            unauthorized: self.unauthorized.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RequireAuth<S> {
    inner: S,
    unauthorized: ErrorObjectOwned,
}

fn is_refused(request: &Request<'_>) -> bool {
    !PUBLIC_METHODS.contains(&request.method_name())
        && request.extensions().get::<RpcAuthenticated>().is_none()
}

impl<S> RpcServiceT for RequireAuth<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            BatchResponse = MethodResponse,
            NotificationResponse = MethodResponse,
        > + Send
        + Sync
        + Clone
        + 'static,
{
    type MethodResponse = MethodResponse;
    type BatchResponse = MethodResponse;
    type NotificationResponse = MethodResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        if is_refused(&request) {
            ResponseFuture::ready(MethodResponse::error(
                request.id(),
                self.unauthorized.clone(),
            ))
        } else {
            ResponseFuture::future(self.inner.call(request))
        }
    }

    /// The root service does not route batch entries through `call`.
    fn batch<'a>(
        &self,
        mut batch: Batch<'a>,
    ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        for entry in batch.iter_mut() {
            let refused_id = match &*entry {
                Ok(BatchEntry::Call(request)) if is_refused(request) => {
                    Some(request.id())
                }
                _ => None,
            };
            if let Some(id) = refused_id {
                *entry = Err(BatchEntryErr::new(id, self.unauthorized.clone()));
            }
        }
        self.inner.batch(batch)
    }

    /// The root service never runs a notification, so it needs no check.
    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(notification)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::SocketAddr, path::Path};

    use jsonrpsee::{
        RpcModule,
        core::middleware::RpcServiceBuilder,
        server::{Server, ServerHandle},
    };
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::{
        BearerAuthLayer, Error, RequireAuthLayer, UNAUTHORIZED_CODE,
        load_or_create_cookie,
    };

    #[test]
    fn creates_cookie_then_reuses_it() -> anyhow::Result<()> {
        let datadir = tempfile::tempdir()?;
        let cookie_path = super::cookie_path(datadir.path());
        let token = load_or_create_cookie(&cookie_path)?;
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&cookie_path)?.permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(load_or_create_cookie(&cookie_path)?, token);
        Ok(())
    }

    #[test]
    fn refuses_empty_cookie() -> anyhow::Result<()> {
        let datadir = tempfile::tempdir()?;
        let cookie_path = super::cookie_path(datadir.path());
        fs::write(&cookie_path, " \n")?;
        assert!(matches!(
            load_or_create_cookie(&cookie_path),
            Err(Error::Empty { .. })
        ));
        fs::write(&cookie_path, "ab cd")?;
        assert!(matches!(
            load_or_create_cookie(&cookie_path),
            Err(Error::Whitespace { .. })
        ));
        Ok(())
    }

    async fn start_server(
        token: &str,
    ) -> anyhow::Result<(SocketAddr, ServerHandle)> {
        let server = Server::builder()
            .set_http_middleware(
                tower::ServiceBuilder::new().layer(BearerAuthLayer::new(token)),
            )
            .set_rpc_middleware(
                RpcServiceBuilder::new()
                    .layer(RequireAuthLayer::new(Path::new(".cookie"))),
            )
            .build(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await?;
        let addr = server.local_addr()?;
        let mut module = RpcModule::new(());
        module.register_method("getblockcount", |_, _, _| 7)?;
        module.register_method("transfer", |_, _, _| "txid")?;
        Ok((addr, server.start(module)))
    }

    async fn post(
        addr: SocketAddr,
        body: &str,
        token: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let authorization = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n{authorization}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        stream.write_all(request.as_bytes()).await?;
        let mut response = String::new();
        stream.read_to_string(&mut response).await?;
        let (_, body) = response
            .split_once("\r\n\r\n")
            .ok_or_else(|| anyhow::anyhow!("no body in `{response}`"))?;
        Ok(serde_json::from_str(body)?)
    }

    fn call(method: &str) -> String {
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#)
    }

    #[tokio::test]
    async fn guards_private_methods() -> anyhow::Result<()> {
        let token = "ab".repeat(32);
        let (addr, handle) = start_server(&token).await?;

        let response = post(addr, &call("transfer"), None).await?;
        assert_eq!(response["error"]["code"], UNAUTHORIZED_CODE);

        let response = post(addr, &call("transfer"), Some("cd")).await?;
        assert_eq!(response["error"]["code"], UNAUTHORIZED_CODE);

        let response =
            post(addr, &call("transfer"), Some(token.as_str())).await?;
        assert_eq!(response["result"], "txid");

        let response = post(addr, &call("getblockcount"), None).await?;
        assert_eq!(response["result"], 7);

        let batch = format!("[{},{}]", call("getblockcount"), call("transfer"));
        let response = post(addr, &batch, None).await?;
        assert_eq!(response[0]["result"], 7);
        assert_eq!(response[1]["error"]["code"], UNAUTHORIZED_CODE);

        handle.stop()?;
        Ok(())
    }
}
