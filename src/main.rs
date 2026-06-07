#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod proxy;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime");

    let _guard = rt.enter();

    let config = config::Config::load();

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
