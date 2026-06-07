/// Async wrapper around the foundry-local-sdk singleton.
///
/// Not in use currently, dead code generally, until put in use beyond proxying
///
/// All long-running SDK calls are async; we run them on the tokio thread pool
/// and push status updates back via a shared `FoundryState` mutex so the
/// egui render loop can read them at any frame.
use std::sync::{Arc, Mutex};

use foundry_local_sdk::{FoundryLocalConfig, FoundryLocalManager};

// ── Public state types ────────────────────────────────────────────────────────

#[derive(Default, Clone, Debug, PartialEq)]
pub enum SdkStatus {
    #[default]
    Uninitialized,
    Initializing,
    Ready,
    Error(String),
}

#[derive(Clone, Debug, Default)]
pub struct ModelEntry {
    pub alias: String,
    pub id: String,
    pub device: String,
    pub is_cached: bool,
    pub is_loaded: bool,
}

#[derive(Default)]
pub struct FoundryState {
    pub status: SdkStatus,
    pub models: Vec<ModelEntry>,
    pub service_url: Option<String>,
    /// Current background op label, e.g. "Downloading phi-4-mini  42.3 %"
    pub progress_label: Option<String>,
    pub progress_pct: f64,
    pub last_message: Option<String>,
}

pub type SharedFoundryState = Arc<Mutex<FoundryState>>;
/// Shared with the proxy so it reads the current upstream URL on every request.
pub type SharedUrl = Arc<std::sync::RwLock<String>>;

// ── Internal helpers ──────────────────────────────────────────────────────────

fn set_progress(state: &SharedFoundryState, label: impl Into<String>, pct: f64) {
    let mut s = state.lock().unwrap();
    s.progress_label = Some(label.into());
    s.progress_pct = pct;
}

fn clear_progress(state: &SharedFoundryState) {
    let mut s = state.lock().unwrap();
    s.progress_label = None;
    s.progress_pct = 0.0;
}

fn set_message(state: &SharedFoundryState, msg: impl Into<String>) {
    state.lock().unwrap().last_message = Some(msg.into());
}

fn get_manager(context_size: u32) -> Result<&'static FoundryLocalManager, String> {
    // Point at the shared Foundry Local cache so models downloaded via the CLI
    // (or any other app) are recognised as cached rather than re-downloaded.
    let cache_dir = dirs::home_dir()
        .map(|h| {
            h.join(".foundry")
                .join("cache")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default();

    FoundryLocalManager::create(
        FoundryLocalConfig::new("npulama")
            .model_cache_dir(cache_dir)
            .additional_setting("context_length", context_size.to_string()),
    )
    .map_err(|e| e.to_string())
}

/// Refresh the in-memory model list and return it for persistence.
async fn refresh_catalog(
    state: &SharedFoundryState,
    manager: &'static FoundryLocalManager,
) -> Vec<ModelEntry> {
    match manager.catalog().get_models().await {
        Ok(models) => {
            let mut entries = Vec::with_capacity(models.len());
            for m in &models {
                let is_cached = m.is_cached().await.unwrap_or(false);
                let is_loaded = m.is_loaded().await.unwrap_or(false);
                entries.push(ModelEntry {
                    alias: m.alias().to_string(),
                    id: m.id().to_string(),
                    device: m
                        .info()
                        .runtime
                        .as_ref()
                        .map(|r| format!("{:?}", r.device_type))
                        .unwrap_or_else(|| "CPU".to_string()),
                    is_cached,
                    is_loaded,
                });
            }
            state.lock().unwrap().models = entries.clone();
            entries
        }
        Err(e) => {
            set_message(state, format!("Catalog error: {}", e));
            vec![]
        }
    }
}

fn capture_url(
    manager: &'static FoundryLocalManager,
    state: &SharedFoundryState,
    upstream: &SharedUrl,
) {
    if let Ok(urls) = manager.urls() {
        if let Some(url) = urls.first() {
            *upstream.write().unwrap() = url.clone();
            state.lock().unwrap().service_url = Some(url.clone());
        }
    }
}

/// Query `/v1/models` and return the IDs of currently loaded models.
/// Empty vec means the service is unreachable or no model is loaded.
async fn query_loaded_model_ids(base_url: &str) -> Vec<String> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let Ok(resp) = reqwest::get(&url).await else {
        return vec![];
    };
    let Ok(json) = resp.json::<serde_json::Value>().await else {
        return vec![];
    };
    json["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m["id"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ── Public async launcher functions ──────────────────────────────────────────

/// Initialise the SDK, register EPs, and populate the catalog.
/// Returns the catalog entries via `on_catalog` callback so the caller can
/// persist them to config without needing to access the shared state.
pub fn initialize(
    state: SharedFoundryState,
    upstream: SharedUrl,
    context_size: u32,
    on_catalog: impl Fn(Vec<ModelEntry>) + Send + 'static,
) {
    tokio::spawn(async move {
        {
            let mut s = state.lock().unwrap();
            s.status = SdkStatus::Initializing;
            s.last_message = None;
        }

        let manager = match tokio::task::spawn_blocking(move || get_manager(context_size)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                state.lock().unwrap().status =
                    SdkStatus::Error(format!("Foundry Local not available: {}", e));
                return;
            }
            Err(e) => {
                state.lock().unwrap().status = SdkStatus::Error(format!("Spawn error: {}", e));
                return;
            }
        };

        // Register execution providers (NPU, OpenVINO, DirectML) — non-fatal
        {
            let state_p = Arc::clone(&state);
            let result = manager
                .download_and_register_eps_with_progress(None, move |ep: &str, pct: f64| {
                    set_progress(&state_p, format!("Registering EP: {}", ep), pct);
                })
                .await;
            clear_progress(&state);
            if let Err(e) = result {
                set_message(&state, format!("EP warning: {}", e));
            }
        }

        // Start (or re-attach to) the web service so urls() is populated.
        if let Err(e) = manager.start_web_service().await {
            set_message(&state, format!("Service start warning: {}", e));
        }
        capture_url(manager, &state, &upstream);

        // Query /v1/models to find what's already loaded.
        // - If nothing is loaded, clear the URL so the proxy returns 503.
        // - If models are loaded, mark them immediately in the state so the UI
        //   shows "● Active" before the full SDK catalog refresh completes.
        {
            let url = upstream.read().unwrap().clone();
            if !url.is_empty() {
                let loaded_ids = query_loaded_model_ids(&url).await;
                if loaded_ids.is_empty() {
                    *upstream.write().unwrap() = String::new();
                    state.lock().unwrap().service_url = None;
                } else {
                    // Annotate any models already in the state (from cached catalog
                    // pre-population) with their live is_loaded flag.
                    let mut s = state.lock().unwrap();
                    for m in &mut s.models {
                        m.is_loaded = loaded_ids.iter().any(|id| id.contains(&m.alias));
                    }
                }
            }
        }

        set_progress(&state, "Loading catalog…", 0.0);
        let entries = refresh_catalog(&state, manager).await;
        clear_progress(&state);
        on_catalog(entries);

        state.lock().unwrap().status = SdkStatus::Ready;
    });
}

/// Download (if needed) and load a model, then start the web service.
pub fn load_model(
    alias: String,
    state: SharedFoundryState,
    upstream: SharedUrl,
    context_size: u32,
    on_catalog: impl Fn(Vec<ModelEntry>) + Send + 'static,
) {
    tokio::spawn(async move {
        let manager = match tokio::task::spawn_blocking(move || get_manager(context_size)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                set_message(&state, format!("SDK error: {}", e));
                return;
            }
            Err(_) => return,
        };

        let model = match manager.catalog().get_model(&alias).await {
            Ok(m) => m,
            Err(e) => {
                set_message(&state, format!("Model '{}' not found: {}", alias, e));
                return;
            }
        };

        if !model.is_cached().await.unwrap_or(false) {
            let state_p = Arc::clone(&state);
            let alias_p = alias.clone();
            let result = model
                .download(Some(move |pct: f64| {
                    set_progress(&state_p, format!("Downloading {}…", alias_p), pct);
                }))
                .await;
            clear_progress(&state);
            if let Err(e) = result {
                set_message(&state, format!("Download failed: {}", e));
                return;
            }
        }

        if !model.is_loaded().await.unwrap_or(false) {
            set_progress(&state, format!("Loading {}…", alias), 0.0);
            if let Err(e) = model.load().await {
                clear_progress(&state);
                set_message(&state, format!("Load failed: {}", e));
                return;
            }
            clear_progress(&state);
        }

        if let Err(e) = manager.start_web_service().await {
            set_message(&state, format!("Service start failed: {}", e));
        }

        capture_url(manager, &state, &upstream);
        let entries = refresh_catalog(&state, manager).await;
        on_catalog(entries);
        set_message(&state, format!("✓ {} loaded and serving", alias));
    });
}

/// Unload a model. Stops the web service if no models remain loaded.
pub fn unload_model(
    alias: String,
    state: SharedFoundryState,
    upstream: SharedUrl,
    context_size: u32,
    on_catalog: impl Fn(Vec<ModelEntry>) + Send + 'static,
) {
    tokio::spawn(async move {
        let manager = match tokio::task::spawn_blocking(move || get_manager(context_size)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                set_message(&state, format!("SDK error: {}", e));
                return;
            }
            Err(_) => return,
        };

        let model = match manager.catalog().get_model(&alias).await {
            Ok(m) => m,
            Err(e) => {
                set_message(&state, format!("Model '{}' not found: {}", alias, e));
                return;
            }
        };

        if let Err(e) = model.unload().await {
            set_message(&state, format!("Unload failed: {}", e));
            return;
        }

        let still_loaded = manager
            .catalog()
            .get_loaded_models()
            .await
            .unwrap_or_default();
        if still_loaded.is_empty() {
            let _ = manager.stop_web_service().await;
            *upstream.write().unwrap() = String::new();
            state.lock().unwrap().service_url = None;
        }

        let entries = refresh_catalog(&state, manager).await;
        on_catalog(entries);
        set_message(&state, format!("✓ {} unloaded", alias));
    });
}
