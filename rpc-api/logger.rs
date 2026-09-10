//! A JSON-RPC request/response logger that never writes secrets to the log.
//!
//! Used by both the server and the CLI client in place of jsonrpsee's
//! `rpc_logger`.
//!
//! jsonrpsee's built-in `RpcLogger` serialises the whole request, parameters
//! included, at `TRACE`. With `--log-level trace` (or `RUST_LOG=jsonrpsee=trace`)
//! a `set_seed_from_mnemonic` call therefore lands the wallet mnemonic in
//! stdout, the rolling log file and the GUI console buffer, and a
//! `generate_mnemonic` response does the same, on whichever side of the
//! connection has trace logging on. This layer logs the method name
//! and request id for every call, and only attaches parameters and responses
//! for methods that are known not to carry key material.

use std::future::Future;

use jsonrpsee::core::{
    middleware::{Batch, Notification, RpcServiceT},
    traits::ToJson,
};
use jsonrpsee::types::Request;
use tracing::Instrument as _;

/// Methods whose parameters or responses contain secrets. Neither side of
/// these calls is ever logged.
const SENSITIVE_METHODS: &[&str] = &[
    // params: mnemonic + passphrase
    "set_seed_from_mnemonic",
    // response: a fresh wallet-strength mnemonic
    "generate_mnemonic",
];

fn is_sensitive(method: &str) -> bool {
    SENSITIVE_METHODS.contains(&method)
}

/// Layer that installs [`RedactingRpcLogger`].
#[derive(Clone, Copy, Debug)]
pub struct RedactingRpcLoggerLayer {
    max_log_len: usize,
}

impl RedactingRpcLoggerLayer {
    pub fn new(max_log_len: usize) -> Self {
        Self { max_log_len }
    }
}

impl<S> tower::Layer<S> for RedactingRpcLoggerLayer {
    type Service = RedactingRpcLogger<S>;

    fn layer(&self, service: S) -> Self::Service {
        RedactingRpcLogger {
            service,
            max_log_len: self.max_log_len,
        }
    }
}

/// See the module docs.
#[derive(Clone, Debug)]
pub struct RedactingRpcLogger<S> {
    service: S,
    max_log_len: usize,
}

fn truncate_at_char_boundary(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        None => s,
        Some((idx, _)) => &s[..idx],
    }
}

fn json_for_log(
    json: &Result<Box<serde_json::value::RawValue>, serde_json::Error>,
    max: usize,
) -> String {
    match json {
        Ok(raw) => truncate_at_char_boundary(raw.get(), max).to_owned(),
        Err(_) => "<invalid JSON>".to_owned(),
    }
}

impl<S> RpcServiceT for RedactingRpcLogger<S>
where
    S: RpcServiceT + Send + Sync + Clone + 'static,
    S::MethodResponse: ToJson,
    S::BatchResponse: ToJson,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    #[tracing::instrument(
        name = "method_call",
        skip_all,
        fields(method = request.method_name(), id = %request.id),
        level = "trace"
    )]
    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        let sensitive = is_sensitive(request.method_name());
        if sensitive {
            tracing::trace!(target: "jsonrpsee", "request = <redacted>");
        } else {
            let params = request
                .params
                .as_ref()
                .map(|params| {
                    truncate_at_char_boundary(params.get(), self.max_log_len)
                        .to_owned()
                })
                .unwrap_or_default();
            tracing::trace!(target: "jsonrpsee", "request params = {params}");
        }

        let service = self.service.clone();
        let max = self.max_log_len;
        async move {
            let rp = service.call(request).await;
            if sensitive {
                tracing::trace!(target: "jsonrpsee", "response = <redacted>");
            } else {
                let json = json_for_log(&rp.to_json(), max);
                tracing::trace!(target: "jsonrpsee", "response = {json}");
            }
            rp
        }
        .in_current_span()
    }

    #[tracing::instrument(
        name = "batch",
        skip_all,
        fields(method = "batch", len = batch.len()),
        level = "trace"
    )]
    fn batch<'a>(
        &self,
        batch: Batch<'a>,
    ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        // A batch may mix sensitive and harmless calls, and the response is
        // one JSON array. Log only its size.
        tracing::trace!(target: "jsonrpsee", "batch request");
        let service = self.service.clone();
        async move {
            let rp = service.batch(batch).await;
            tracing::trace!(target: "jsonrpsee", "batch response");
            rp
        }
        .in_current_span()
    }

    #[tracing::instrument(
        name = "notification",
        skip_all,
        fields(method = &*n.method),
        level = "trace"
    )]
    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        tracing::trace!(target: "jsonrpsee", "notification");
        self.service.notification(n).in_current_span()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_methods_are_sensitive() {
        assert!(is_sensitive("set_seed_from_mnemonic"));
        assert!(is_sensitive("generate_mnemonic"));
        assert!(!is_sensitive("balance"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate_at_char_boundary("ボルテックス", 4), "ボルテッ");
        assert_eq!(truncate_at_char_boundary("abc", 10), "abc");
    }
}
