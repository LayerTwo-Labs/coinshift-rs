//! Server side of RPC cookie authentication; see `coinshift_app_rpc_api::auth`.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use coinshift_app_rpc_api::auth::{COOKIE_USER, authorizes};
use http::{Request, Response, StatusCode, header::AUTHORIZATION};
use jsonrpsee::server::HttpBody;

/// The secret the server checks every request against.
#[derive(Clone)]
pub struct RpcAuth {
    user: String,
    secret: String,
}

impl RpcAuth {
    /// Generate a fresh secret and write it to `cookie_path`, readable by the
    /// owner only. The file is rewritten on every start, so a cookie from a
    /// previous run never stays valid.
    pub fn generate(cookie_path: &Path) -> anyhow::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|err| {
            anyhow::anyhow!("failed to draw RPC secret: {err}")
        })?;
        let secret = hex::encode(bytes);
        write_owner_only(cookie_path, &format!("{COOKIE_USER}:{secret}\n"))?;
        tracing::info!(
            path = %cookie_path.display(),
            "RPC authentication enabled; credentials written to cookie file"
        );
        Ok(Self {
            user: COOKIE_USER.to_owned(),
            secret,
        })
    }

    /// The `tower_http::validate_request` check: pass the request through
    /// when it carries the cookie credentials, otherwise answer `401`.
    pub fn validate<B>(
        &mut self,
        request: &mut Request<B>,
    ) -> Result<(), Response<HttpBody>> {
        if authorizes(
            request.headers().get(AUTHORIZATION),
            &self.user,
            &self.secret,
        ) {
            Ok(())
        } else {
            let mut response = Response::new(HttpBody::from(
                "unauthorized: send the credentials from the RPC cookie file \
                 as HTTP basic auth",
            ));
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            response.headers_mut().insert(
                http::header::WWW_AUTHENTICATE,
                http::HeaderValue::from_static("Basic realm=\"coinshift\""),
            );
            Err(response)
        }
    }
}

fn write_owner_only(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::{
            io::Write as _,
            os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
        };
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `mode` only applies on creation; an existing file keeps its bits.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

/// Refuse to expose the wallet on a network interface unless the operator
/// asked for it explicitly. Binding `0.0.0.0` used to be one flag away, with
/// no authentication behind it.
pub fn check_bind_address(
    rpc_addr: SocketAddr,
    allow_remote: bool,
) -> anyhow::Result<()> {
    if rpc_addr.ip().is_loopback() || allow_remote {
        return Ok(());
    }
    anyhow::bail!(
        "--rpc-addr {rpc_addr} is not a loopback address. The RPC server \
         controls the wallet; pass --rpc-allow-remote to expose it on a \
         network interface (keep cookie authentication on, and put TLS in \
         front of it)"
    )
}

/// Default cookie location inside the data directory.
pub fn default_cookie_path(datadir: &Path) -> PathBuf {
    datadir.join(coinshift_app_rpc_api::auth::COOKIE_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_cookie_is_owner_only_and_authorizes() {
        let dir = temp_dir::TempDir::new().unwrap();
        let path = default_cookie_path(dir.path());
        let mut auth = RpcAuth::generate(&path).unwrap();
        let (user, secret) =
            coinshift_app_rpc_api::auth::read_cookie(&path).unwrap();
        assert_eq!(user, COOKIE_USER);
        assert_eq!(secret.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let mut ok = Request::new(());
        ok.headers_mut().insert(
            AUTHORIZATION,
            coinshift_app_rpc_api::auth::basic_auth_header(&user, &secret),
        );
        assert!(auth.validate(&mut ok).is_ok());

        let mut anonymous = Request::new(());
        let response = auth.validate(&mut anonymous).unwrap_err();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // A restart rotates the secret.
        let _next = RpcAuth::generate(&path).unwrap();
        let (_, rotated) =
            coinshift_app_rpc_api::auth::read_cookie(&path).unwrap();
        assert_ne!(rotated, secret);
    }

    /// The layer as wired into a real jsonrpsee server: a request with the
    /// cookie succeeds, one without is refused before reaching the method.
    #[tokio::test]
    async fn server_refuses_requests_without_cookie() {
        use jsonrpsee::{
            core::client::ClientT as _, http_client::HttpClientBuilder,
            server::Server,
        };

        let dir = temp_dir::TempDir::new().unwrap();
        let cookie_path = default_cookie_path(dir.path());
        let mut auth = RpcAuth::generate(&cookie_path).unwrap();
        let http_middleware = tower::ServiceBuilder::new().layer(
            tower_http::validate_request::ValidateRequestHeaderLayer::custom(
                move |request: &mut Request<_>| auth.validate(request),
            ),
        );
        let server = Server::builder()
            .set_http_middleware(http_middleware)
            .build("127.0.0.1:0")
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();
        let mut module = jsonrpsee::RpcModule::new(());
        module.register_method("ping", |_, _, _| "pong").unwrap();
        let handle = server.start(module);

        let url = format!("http://{addr}");
        let anonymous = HttpClientBuilder::default().build(&url).unwrap();
        let err = anonymous
            .request::<String, _>("ping", jsonrpsee::rpc_params![])
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("401"),
            "expected 401 Unauthorized, got {err}"
        );

        let (user, secret) =
            coinshift_app_rpc_api::auth::read_cookie(&cookie_path).unwrap();
        let headers = http::HeaderMap::from_iter([(
            AUTHORIZATION,
            coinshift_app_rpc_api::auth::basic_auth_header(&user, &secret),
        )]);
        let authed = HttpClientBuilder::default()
            .set_headers(headers)
            .build(&url)
            .unwrap();
        let pong: String = authed
            .request("ping", jsonrpsee::rpc_params![])
            .await
            .unwrap();
        assert_eq!(pong, "pong");

        handle.stop().unwrap();
    }

    #[test]
    fn remote_bind_needs_opt_in() {
        let local: SocketAddr = "127.0.0.1:6255".parse().unwrap();
        let any: SocketAddr = "0.0.0.0:6255".parse().unwrap();
        assert!(check_bind_address(local, false).is_ok());
        assert!(check_bind_address(any, false).is_err());
        assert!(check_bind_address(any, true).is_ok());
    }
}
