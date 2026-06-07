/// End-to-end test against a live Foundry Local instance.
///
/// Skipped automatically when Foundry Local is not running.
/// Run manually with:
///   cargo test --test e2e_foundry -- --nocapture
///
/// Requires phi-4-mini to be loaded: `foundry model load phi-4-mini`
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tokio::net::TcpListener;

use npulama::{config::Config, proxy::start_proxy_with_listener};

const MODEL: &str = "phi-4-mini";

// ── Foundry Local discovery ───────────────────────────────────────────────────

fn powershell(cmd: &str) -> Option<String> {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", cmd])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Returns the Foundry Local service URL, e.g. `http://127.0.0.1:52495`.
fn foundry_service_url() -> Option<String> {
    let output = powershell("foundry server status 2>&1 | Out-String")
        .or_else(|| powershell("foundry service status 2>&1 | Out-String"))?;

    output
        .split_whitespace()
        .find(|tok| tok.starts_with("http://127.0.0.1:") || tok.starts_with("http://localhost:"))
        .map(|s| s.trim_end_matches('/').to_string())
}

// ── Test helpers ──────────────────────────────────────────────────────────────

macro_rules! skip_if_none {
    ($expr:expr, $msg:literal) => {
        match $expr {
            Some(v) => v,
            None => {
                eprintln!("SKIP: {}", $msg);
                return;
            }
        }
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Ask phi-4-mini "What is the capital of Sweden?" via the proxy and assert
/// that the answer contains "Stockholm".
#[tokio::test]
async fn test_capital_of_sweden_via_proxy() {
    // ── 1. Discover live Foundry Local endpoint ───────────────────────────
    let foundry_url = skip_if_none!(
        foundry_service_url(),
        "Foundry Local is not running — start it with 'foundry server start'"
    );

    eprintln!("Foundry Local: {}  model: {}", foundry_url, MODEL);

    // ── 2. Start proxy pointing at Foundry Local ──────────────────────────
    let mut config = Config::default();
    config.upstream_url = foundry_url.clone();
    config.require_auth = false;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let (shutdown, _handle) = start_proxy_with_listener(listener, config);

    eprintln!("npulama proxy on 127.0.0.1:{}", proxy_port);

    // ── 3. Confirm the proxy can reach /v1/models ─────────────────────────
    let models_resp = Client::new()
        .get(format!("http://127.0.0.1:{}/v1/models", proxy_port))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("GET /v1/models via proxy failed");

    assert_eq!(models_resp.status(), 200, "Expected 200 from /v1/models via proxy");

    // ── 4. Send chat completions request ─────────────────────────────────
    let chat_resp = Client::new()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", proxy_port))
        .timeout(Duration::from_secs(120))
        .json(&serde_json::json!({
            "model": MODEL,
            "stream": false,
            "messages": [
                {
                    "role": "user",
                    "content": "What is the capital of Sweden? Reply with only the city name."
                }
            ],
            "max_tokens": 16,
            "temperature": 0
        }))
        .send()
        .await
        .expect("POST /v1/chat/completions via proxy failed");

    let status = chat_resp.status();
    let body_text = chat_resp.text().await.unwrap_or_default();
    eprintln!("chat completions → {} — body: {}", status, body_text);

    assert_eq!(
        status, 200,
        "Expected 200 from chat completions, got: {}\nbody: {}",
        status, body_text
    );

    // ── 5. Assert Stockholm is in the answer ─────────────────────────────
    let body: Value = serde_json::from_str(&body_text)
        .expect("Response body is not valid JSON");

    let content = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    eprintln!("Model answered: {:?}", content);

    assert!(
        content.to_lowercase().contains("stockholm"),
        "Expected 'Stockholm' in response, got: {:?}",
        content
    );

    shutdown.send(()).ok();
}
