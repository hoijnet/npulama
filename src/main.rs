#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod config;
mod proxy;

use clap::Parser;

fn main() {
    let args = cli::Args::parse();
    let mut config = config::Config::load();
    args.apply_to(&mut config);

    if args.headless {
        run_headless(config);
        return;
    }

    // GUI mode: enter the tokio runtime so tokio::spawn works from egui callbacks.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");
    let _guard = rt.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Foundry Local Proxy")
            .with_inner_size([440.0, 580.0])
            .with_resizable(false)
            .with_maximize_button(false),
        ..Default::default()
    };

    eframe::run_native(
        "Foundry Local Proxy",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, config)))),
    )
    .expect("eframe error");
}

/// Headless mode: proxy only, no window, Ctrl-C to stop.
fn run_headless(config: config::Config) {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(async {
            let addr = config.bind_addr();
            let (shutdown_tx, handle) = proxy::start_proxy(config);

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
