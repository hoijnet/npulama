/// Integration tests for the npulama proxy.
///
/// Each test:
///   1. Starts a tiny mock upstream with axum on a random OS port.
///   2. Starts the proxy on another random OS port, pointing at the mock.
///   3. Sends real HTTP requests through the proxy.
///   4. Asserts on the response status / body / headers.
///   5. Shuts both servers down cleanly.
use axum::{routing::any, Router};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use npulama::{config::Config, proxy::start_proxy_with_listener};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Spin up a bare-bones upstream that echoes the request path back as JSON.
async fn start_mock_upstream() -> (u16, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let app = Router::new().fallback(any(|req: axum::extract::Request| async move {
        let path = req.uri().path().to_string();
        axum::Json(json!({ "upstream": "ok", "path": path }))
    }));

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async { let _ = rx.await; })
            .await
            .ok();
    });

    (port, tx)
}

/// Spin up an upstream that echoes the full request body back as JSON.
async fn start_body_echo_upstream() -> (u16, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let app = Router::new().fallback(any(|req: axum::extract::Request| async move {
        let body = axum::body::to_bytes(req.into_body(), 1024 * 1024).await.unwrap_or_default();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        axum::Json(json)
    }));

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async { let _ = rx.await; })
            .await
            .ok();
    });

    (port, tx)
}

/// Start the proxy on a random OS port pointing at the given upstream URL.
async fn start_proxy(
    config: Config,
    upstream_url: impl Into<String>,
) -> (u16, tokio::sync::oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, _handle) = start_proxy_with_listener(listener, config, upstream_url);
    (port, tx)
}

fn base_config() -> Config {
    let mut c = Config::default();
    c.require_auth = false;
    c
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_request_is_forwarded() {
    let (up_port, _up_stop) = start_mock_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);
    let (proxy_port, _proxy_stop) = start_proxy(base_config(), url).await;

    let resp = Client::new()
        .get(format!("http://127.0.0.1:{}/v1/models", proxy_port))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["upstream"], "ok");
    assert_eq!(body["path"], "/v1/models");
}

#[tokio::test]
async fn test_no_auth_required_by_default() {
    let (up_port, _up_stop) = start_mock_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);
    let (proxy_port, _proxy_stop) = start_proxy(base_config(), url).await;

    let resp = Client::new()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .json(&json!({"model": "x", "messages": []}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_auth_rejects_missing_token() {
    let (up_port, _up_stop) = start_mock_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);

    let mut config = base_config();
    config.require_auth = true;
    config.tokens = vec!["sk-validtoken".into()];

    let (proxy_port, _proxy_stop) = start_proxy(config, url).await;

    let resp = Client::new()
        .get(format!("http://127.0.0.1:{}/v1/models", proxy_port))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_api_key");
}

#[tokio::test]
async fn test_auth_rejects_wrong_token() {
    let (up_port, _up_stop) = start_mock_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);

    let mut config = base_config();
    config.require_auth = true;
    config.tokens = vec!["sk-validtoken".into()];

    let (proxy_port, _proxy_stop) = start_proxy(config, url).await;

    let resp = Client::new()
        .get(format!("http://127.0.0.1:{}/v1/models", proxy_port))
        .header("authorization", "Bearer sk-wrongtoken")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_auth_accepts_valid_token() {
    let (up_port, _up_stop) = start_mock_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);

    let mut config = base_config();
    config.require_auth = true;
    config.tokens = vec!["sk-validtoken".into()];

    let (proxy_port, _proxy_stop) = start_proxy(config, url).await;

    let resp = Client::new()
        .get(format!("http://127.0.0.1:{}/v1/models", proxy_port))
        .header("authorization", "Bearer sk-validtoken")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_cors_preflight_returns_200() {
    let (up_port, _up_stop) = start_mock_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);
    let (proxy_port, _proxy_stop) = start_proxy(base_config(), url).await;

    let resp = Client::new()
        .request(
            reqwest::Method::OPTIONS,
            format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port),
        )
        .header("origin", "http://localhost:3000")
        .header("access-control-request-method", "POST")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(resp.headers().contains_key("access-control-allow-origin"));
    assert!(resp.headers().contains_key("access-control-allow-methods"));
}

#[tokio::test]
async fn test_upstream_unreachable_returns_502() {
    let config = base_config();
    let (proxy_port, _proxy_stop) =
        start_proxy(config, "http://127.0.0.1:1").await; // nothing listening there

    let resp = Client::new()
        .get(format!("http://127.0.0.1:{}/v1/models", proxy_port))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 502);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "upstream_error");
}

#[tokio::test]
async fn test_max_completion_tokens_stripped() {
    let (up_port, _up_stop) = start_body_echo_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);
    let (proxy_port, _proxy_stop) = start_proxy(base_config(), url).await;

    let resp = Client::new()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .json(&json!({
            "model": "phi-4-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 32000,
            "temperature": 1.0
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    // max_completion_tokens must be removed before forwarding
    assert!(body.get("max_completion_tokens").is_none(), "max_completion_tokens should be stripped");
    // other fields must survive intact
    assert_eq!(body["model"], "phi-4-mini");
    assert_eq!(body["temperature"], 1.0);
}

#[tokio::test]
async fn test_body_without_max_completion_tokens_passes_unchanged() {
    let (up_port, _up_stop) = start_body_echo_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);
    let (proxy_port, _proxy_stop) = start_proxy(base_config(), url).await;

    let resp = Client::new()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .json(&json!({
            "model": "phi-4-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.7
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "phi-4-mini");
    assert_eq!(body["temperature"], 0.7);
}

#[tokio::test]
async fn test_path_and_query_are_forwarded() {
    let (up_port, _up_stop) = start_mock_upstream().await;
    let url = format!("http://127.0.0.1:{}", up_port);
    let (proxy_port, _proxy_stop) = start_proxy(base_config(), url).await;

    let resp = Client::new()
        .get(format!("http://127.0.0.1:{}/v1/models?limit=10", proxy_port))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["path"].as_str().unwrap().starts_with("/v1/models"));
}
