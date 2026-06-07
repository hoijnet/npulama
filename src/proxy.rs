use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::Response,
    Router,
};
use futures_util::TryStreamExt;
use serde_json::json;
use std::sync::{Arc, RwLock};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

use crate::config::Config;

#[derive(Clone)]
struct ProxyState {
    upstream_url: Arc<RwLock<String>>,
    tokens: Arc<RwLock<Vec<String>>>,
    require_auth: Arc<RwLock<bool>>,
    client: reqwest::Client,
}

pub fn start_proxy(config: Config) -> (oneshot::Sender<()>, JoinHandle<Result<(), String>>) {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let bind_addr = config.bind_addr();

    let state = ProxyState {
        upstream_url: Arc::new(RwLock::new(config.upstream_url.clone())),
        tokens: Arc::new(RwLock::new(config.tokens.clone())),
        require_auth: Arc::new(RwLock::new(config.require_auth)),
        client: reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("Failed to build HTTP client"),
    };

    let handle = tokio::spawn(async move {
        let app = Router::new().fallback(proxy_handler).with_state(state);

        let listener = TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| format!("Failed to bind {}: {}", bind_addr, e))?;

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .map_err(|e| format!("Server error: {}", e))?;

        Ok(())
    });

    (shutdown_tx, handle)
}

async fn proxy_handler(State(state): State<ProxyState>, req: Request) -> Response {
    let method = req.method().clone();

    // CORS preflight
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

    // Auth check
    if *state.require_auth.read().unwrap() {
        let auth_ok = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| state.tokens.read().unwrap().contains(&t.to_string()))
            .unwrap_or(false);

        if !auth_ok {
            return error_response(
                401,
                "invalid_api_key",
                "Invalid or missing API key. Provide a valid sk-... token.",
            );
        }
    }

    // Build upstream URL
    let upstream_base = state.upstream_url.read().unwrap().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
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

    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);

    let mut upstream_req = state
        .client
        .request(reqwest_method, &upstream_url)
        .body(body_bytes);

    for (name, value) in &headers {
        let n = name.as_str();
        if n != "authorization" && n != "host" {
            upstream_req = upstream_req.header(n, value.as_bytes());
        }
    }

    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                502,
                "upstream_error",
                &format!("Cannot reach Foundry Local at {}: {}", upstream_base, e),
            )
        }
    };

    let status = StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = Response::builder().status(status);
    for (name, value) in upstream_resp.headers() {
        let n = name.as_str();
        if !matches!(
            n,
            "transfer-encoding"
                | "connection"
                | "keep-alive"
                | "access-control-allow-origin"
                | "access-control-allow-headers"
                | "access-control-allow-methods"
        ) {
            builder = builder.header(name, value);
        }
    }
    builder = builder
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-headers", "authorization, content-type")
        .header("access-control-allow-methods", "GET, POST, PUT, DELETE, OPTIONS");

    let stream = upstream_resp
        .bytes_stream()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));

    builder.body(Body::from_stream(stream)).unwrap()
}

fn error_response(code: u16, error_type: &str, message: &str) -> Response {
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
