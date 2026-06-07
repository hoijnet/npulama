use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::Response,
    Router,
};
use futures_util::TryStreamExt;
use serde_json::json;
use std::{sync::{Arc, RwLock}, time::Duration};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

use crate::{config::Config, foundry::SharedUrl};

// ── Shared handler state ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ProxyState {
    upstream_url: SharedUrl,
    tokens: Arc<RwLock<Vec<String>>>,
    require_auth: Arc<RwLock<bool>>,
    client: reqwest::Client,
}

impl ProxyState {
    /// Build proxy state sharing the given upstream URL Arc with the foundry module.
    pub fn from_config(config: &Config, upstream: SharedUrl) -> Self {
        Self {
            upstream_url: upstream,
            tokens: Arc::new(RwLock::new(config.tokens.clone())),
            require_auth: Arc::new(RwLock::new(config.require_auth)),
            client: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(10))
                // Disable all auto-decompression. As a transparent proxy we
                // must not touch the response bytes — and SSE streams break if
                // reqwest tries to gzip-decode them (gzip needs per-event flush
                // which most servers don't do). Without this reqwest also adds
                // its own Accept-Encoding header, conflicting with the client's.
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .build()
                .expect("Failed to build HTTP client"),
        }
    }
}

// ── Public start functions ────────────────────────────────────────────────────

/// Normal GUI start: binds the port from config internally.
pub fn start_proxy(
    config: Config,
    upstream: SharedUrl,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), String>>) {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let bind_addr = config.bind_addr();
    let state = ProxyState::from_config(&config, upstream);

    let handle = tokio::spawn(async move {
        let listener = TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| format!("Failed to bind {}: {}", bind_addr, e))?;

        serve(listener, state, shutdown_rx).await
    });

    (shutdown_tx, handle)
}

/// Test start: caller supplies an already-bound TcpListener and the upstream URL.
#[allow(dead_code)]
pub fn start_proxy_with_listener(
    listener: TcpListener,
    config: Config,
    upstream_url: impl Into<String>,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), String>>) {
    let upstream = Arc::new(RwLock::new(upstream_url.into()));
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let state = ProxyState::from_config(&config, upstream);
    let handle = tokio::spawn(serve(listener, state, shutdown_rx));
    (shutdown_tx, handle)
}

// ── Internal serve loop ───────────────────────────────────────────────────────

async fn serve(
    listener: TcpListener,
    state: ProxyState,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let app = Router::new().fallback(proxy_handler).with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await
        .map_err(|e| format!("Server error: {}", e))
}

// ── Request handler ───────────────────────────────────────────────────────────

async fn proxy_handler(State(state): State<ProxyState>, req: Request) -> Response {
    let method = req.method().clone();

    // CORS preflight (silent — not interesting for debugging)
    if method == axum::http::Method::OPTIONS {
        return Response::builder()
            .status(200)
            .header("access-control-allow-origin", "*")
            .header("access-control-allow-headers", "authorization, content-type")
            .header("access-control-allow-methods", "GET, POST, PUT, DELETE, OPTIONS")
            .header("access-control-max-age", "86400")
            .body(Body::empty())
            .unwrap();
    }

    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    // Auth
    if *state.require_auth.read().unwrap() {
        let auth_ok = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| state.tokens.read().unwrap().contains(&t.to_string()))
            .unwrap_or(false);

        if !auth_ok {
            eprintln!("[npulama] 401 {} {} — invalid/missing token", method, path_and_query);
            return error_response(
                401,
                "invalid_api_key",
                "Invalid or missing API key. Provide a valid sk-... token.",
            );
        }
    }

    // Build upstream URL — guard against no model being loaded yet
    let upstream_base = state.upstream_url.read().unwrap().clone();
    if upstream_base.is_empty() {
        eprintln!("[npulama] ✗ 503 {} {} — no model loaded (upstream URL is empty)", method, path_and_query);
        return error_response(
            503,
            "model_not_loaded",
            "No model is loaded. Open the npulama window, load a model, and try again.",
        );
    }
    let upstream_url = format!(
        "{}{}",
        upstream_base.trim_end_matches('/'),
        path_and_query
    );

    // Read body
    let headers = req.headers().clone();
    let body_bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return error_response(400, "bad_request", "Failed to read request body"),
    };

    // Log every request
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            if v.len() > 14 { format!("{}…", &v[..14]) } else { v.to_string() }
        })
        .unwrap_or_else(|| "(none)".to_string());

    eprintln!(
        "[npulama] → {} {}  auth: {}",
        method, upstream_url, auth_header
    );
    // Transform the request body if needed (e.g. drop fields Foundry rejects).
    // `forward_body` is the exact byte sequence we send upstream from here on;
    // `body_bytes` is kept only for error-path logging.
    let forward_body = strip_unsupported_fields(&method, &path_and_query, body_bytes.clone());

    if !forward_body.is_empty() {
        eprintln!(
            "[npulama]   body: {}",
            truncate_utf8(&forward_body, 32_768)
        );
    }

    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);

    // Forward all request headers except those that must not be proxied or that
    // we manage ourselves:
    //   - hop-by-hop headers (RFC 7230 §6.1) are connection-scoped
    //   - host: reqwest derives it from the target URL
    //   - authorization: auth is terminated at this proxy
    //   - content-length: reqwest sets it from the sized body below, so it is
    //     always consistent with the bytes we actually send (the client's value
    //     may be stale after the body transform). Setting it here too would risk
    //     a conflicting/duplicate header.
    let mut upstream_req = state.client.request(reqwest_method, &upstream_url);
    for (name, value) in &headers {
        let n = name.as_str();
        if n == "host" || n == "authorization" || n == "content-length" || is_hop_by_hop(n) {
            continue;
        }
        upstream_req = upstream_req.header(n, value.as_bytes());
    }
    // Sized body → reqwest emits the matching Content-Length automatically.
    let upstream_req = upstream_req.body(forward_body);

    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[npulama] ✗ 502 — upstream unreachable: {}", e);
            return error_response(
                502,
                "upstream_error",
                &format!("Cannot reach upstream at {}: {}", upstream_base, e),
            );
        }
    };

    let status_u16 = upstream_resp.status().as_u16();
    let status = StatusCode::from_u16(status_u16)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    // On error responses: buffer body, log everything, forward to client.
    if !status.is_success() {
        let resp_headers = upstream_resp.headers().clone();
        let resp_bytes = upstream_resp.bytes().await.unwrap_or_default();
        eprintln!("[npulama] ✗ {} {} {}", status_u16, method, path_and_query);
        eprintln!("[npulama]   request : {}", truncate_utf8(&body_bytes, 32_768));
        eprintln!("[npulama]   response: {}", truncate_utf8(&resp_bytes, 32_768));

        let mut builder = Response::builder().status(status);
        for (name, value) in &resp_headers {
            if forward_response_header(name.as_str()) {
                builder = builder.header(name, value);
            }
        }
        return builder
            .header("access-control-allow-origin", "*")
            .header("access-control-allow-headers", "authorization, content-type")
            .header("access-control-allow-methods", "GET, POST, PUT, DELETE, OPTIONS")
            .body(Body::from(resp_bytes.to_vec()))
            .unwrap();
    }

    eprintln!("[npulama] ✓ {} {} {}", status_u16, method, path_and_query);

    let mut builder = Response::builder().status(status);
    for (name, value) in upstream_resp.headers() {
        if forward_response_header(name.as_str()) {
            builder = builder.header(name, value);
        }
    }
    builder = builder
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-headers", "authorization, content-type")
        .header("access-control-allow-methods", "GET, POST, PUT, DELETE, OPTIONS");

    let log_path = path_and_query.clone();
    let stream = upstream_resp
        .bytes_stream()
        .map_err(move |e| {
            eprintln!("[npulama] ✗ stream error on {}: {}", log_path, e);
            std::io::Error::new(std::io::ErrorKind::Other, e)
        });

    builder.body(Body::from_stream(stream)).unwrap()
}

/// Rewrite the body of chat completion requests to remove fields that
/// Foundry Local does not support. Returns the original bytes unchanged
/// if the request is not a chat completion or the body is not valid JSON.
fn strip_unsupported_fields(
    method: &axum::http::Method,
    path: &str,
    body: axum::body::Bytes,
) -> axum::body::Bytes {
    if method != axum::http::Method::POST || !path.starts_with("/v1/chat/completions") || body.is_empty() {
        return body;
    }
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(obj) = json.as_object_mut() else {
        return body;
    };

    // max_completion_tokens is an OpenAI v2 field Foundry does not recognise.
    // Only re-serialise when the field was actually present — otherwise return
    // the original bytes unchanged so Content-Length stays valid.
    if obj.remove("max_completion_tokens").is_none() {
        return body;
    }

    serde_json::to_vec(&json)
        .map(axum::body::Bytes::from)
        .unwrap_or(body)
}

/// Hop-by-hop headers (RFC 7230 §6.1) are scoped to a single transport
/// connection and must never be forwarded by a proxy in either direction.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Whether an upstream *response* header should be copied to the client.
/// Drops hop-by-hop headers and the CORS headers we set ourselves.
fn forward_response_header(name: &str) -> bool {
    !is_hop_by_hop(name)
        && !matches!(
            name,
            "access-control-allow-origin"
                | "access-control-allow-headers"
                | "access-control-allow-methods"
        )
}

fn truncate_utf8(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        s.into_owned()
    } else {
        format!("{}… ({} bytes total)", &s[..max], bytes.len())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn error_response(code: u16, error_type: &str, message: &str) -> Response {
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .header("access-control-allow-origin", "*")
        .body(Body::from(
            json!({
                "error": {
                    "message": message,
                    "type": "invalid_request_error",
                    "code": error_type
                }
            })
            .to_string(),
        ))
        .unwrap()
}
