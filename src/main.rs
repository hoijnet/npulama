#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod config;
mod foundry;
mod proxy;

use std::sync::{Arc, RwLock};

use clap::Parser;

fn main() {
    let args = cli::Args::parse();
    let mut config = config::Config::load();
    args.apply_to(&mut config);

    let shared_url: foundry::SharedUrl = Arc::new(RwLock::new(String::new()));
    let foundry_state: foundry::SharedFoundryState = Default::default();

    if args.headless {
        run_headless(config, foundry_state, shared_url);
        return;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");
    let _guard = rt.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Foundry Local Proxy")
            .with_inner_size([460.0, 740.0])
            .with_min_inner_size([380.0, 400.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "Foundry Local Proxy",
        options,
        Box::new(|cc| {
            Ok(Box::new(app::App::new(cc, config, foundry_state, shared_url)))
        }),
    )
    .expect("eframe error");
}

fn run_headless(
    config: config::Config,
    foundry_state: foundry::SharedFoundryState,
    shared_url: foundry::SharedUrl,
) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(async {
            let context_size = config.context_size;
            let preferred = config.preferred_model.clone();
            let addr = config.bind_addr();

            // Initialize SDK
            let state_c = Arc::clone(&foundry_state);
            let url_c = Arc::clone(&shared_url);
            foundry::initialize(state_c, url_c, context_size, |_| {});

            // Wait for SDK ready
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                let status = foundry_state.lock().unwrap().status.clone();
                match status {
                    foundry::SdkStatus::Ready => break,
                    foundry::SdkStatus::Error(e) => {
                        eprintln!("Foundry SDK error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }

            // Auto-load preferred model
            if let Some(alias) = preferred {
                eprintln!("Loading model: {}", alias);
                let state_c = Arc::clone(&foundry_state);
                let url_c = Arc::clone(&shared_url);
                foundry::load_model(alias, state_c, url_c, context_size, |_| {});
                // Wait for URL to populate
                for _ in 0..60 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    if !shared_url.read().unwrap().is_empty() {
                        break;
                    }
                }
            }

            let (shutdown_tx, handle) = proxy::start_proxy(config, Arc::clone(&shared_url));
            eprintln!("npulama headless — proxy listening on {}", addr);
            eprintln!("Press Ctrl-C to stop.");

            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl-C");

            eprintln!("\nShutting down…");
            let _ = shutdown_tx.send(());
            let _ = handle.await;
        });
}
