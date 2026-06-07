use std::sync::Arc;

use egui::{Color32, Context, FontId, RichText, ScrollArea, ViewportCommand};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use tokio::task::JoinHandle;
use tokio::sync::oneshot;

use crate::config::{CachedModel, Config};
use crate::foundry::{self, SdkStatus, SharedFoundryState, SharedUrl};
use crate::proxy::start_proxy;

pub struct App {
    config: Config,

    edit_port: String,
    edit_context: String,

    proxy_running: bool,
    proxy_shutdown: Option<oneshot::Sender<()>>,
    proxy_handle: Option<JoinHandle<Result<(), String>>>,
    proxy_error: Option<String>,

    show_token_full: Option<usize>,
    model_filter: String,

    foundry_state: SharedFoundryState,
    shared_url: SharedUrl,
    last_sdk_status: SdkStatus,
    auto_load_triggered: bool,

    _tray: TrayIcon,
    menu_show_id: tray_icon::menu::MenuId,
    menu_quit_id: tray_icon::menu::MenuId,
}

impl App {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        config: Config,
        foundry_state: SharedFoundryState,
        shared_url: SharedUrl,
    ) -> Self {
        let icon = build_tray_icon();
        let tray_menu = Menu::new();
        let show_item = MenuItem::new("Open", true, None);
        let quit_item = MenuItem::new("Exit", true, None);
        tray_menu
            .append_items(&[&show_item, &PredefinedMenuItem::separator(), &quit_item])
            .ok();
        let menu_show_id = show_item.id().clone();
        let menu_quit_id = quit_item.id().clone();
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("Foundry Local Proxy")
            .with_icon(icon)
            .build()
            .expect("Failed to create system tray icon");

        let edit_port = config.port.to_string();
        let edit_context = config.context_size.to_string();
        let context_size = config.context_size;

        // Pre-populate the live model list from the cached catalog so the UI
        // shows model names immediately and initialize() can annotate is_loaded.
        {
            let mut s = foundry_state.lock().unwrap();
            s.models = config.cached_catalog.iter().map(|m| foundry::ModelEntry {
                alias: m.alias.clone(),
                id: String::new(),
                device: m.device.clone(),
                is_cached: m.is_cached,
                is_loaded: false,
            }).collect();
        }

        // Start SDK init in background; callback saves catalog to config
        {
            let state_c = Arc::clone(&foundry_state);
            let url_c = Arc::clone(&shared_url);
            foundry::initialize(state_c, url_c, context_size, |_| {});
        }

        let mut app = Self {
            config,
            edit_port,
            edit_context,
            proxy_running: false,
            proxy_shutdown: None,
            proxy_handle: None,
            proxy_error: None,
            show_token_full: None,
            model_filter: String::new(),
            foundry_state,
            shared_url,
            last_sdk_status: SdkStatus::Uninitialized,
            auto_load_triggered: false,
            _tray: tray,
            menu_show_id,
            menu_quit_id,
        };

        if app.config.autostart {
            app.start_proxy();
        }

        app
    }

    fn start_proxy(&mut self) {
        if self.proxy_running {
            return;
        }
        self.proxy_error = None;
        let (tx, handle) = start_proxy(self.config.clone(), Arc::clone(&self.shared_url));
        self.proxy_shutdown = Some(tx);
        self.proxy_handle = Some(handle);
        self.proxy_running = true;
    }

    fn stop_proxy(&mut self) {
        if let Some(tx) = self.proxy_shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.proxy_handle.take() {
            h.abort();
        }
        self.proxy_running = false;
    }

    fn apply_port(&mut self) {
        if let Ok(port) = self.edit_port.trim().parse::<u16>() {
            self.config.port = port;
        } else {
            self.edit_port = self.config.port.to_string();
        }
        self.config.save();
        if self.proxy_running {
            self.stop_proxy();
            self.start_proxy();
        }
    }

    fn apply_context_size(&mut self) {
        if let Ok(n) = self.edit_context.trim().parse::<u32>() {
            self.config.context_size = n.clamp(2048, 131_072);
        }
        self.edit_context = self.config.context_size.to_string();
        self.config.save();
    }

    fn poll_proxy_error(&mut self) {
        if let Some(handle) = &self.proxy_handle {
            if handle.is_finished() {
                if let Some(handle) = self.proxy_handle.take() {
                    let result = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(handle)
                    });
                    self.proxy_running = false;
                    self.proxy_shutdown = None;
                    if let Ok(Err(e)) = result {
                        self.proxy_error = Some(e);
                    }
                }
            }
        }
    }

    /// Detect SDK status transitions and act on them.
    fn poll_sdk_transition(&mut self) {
        let current_status = self.foundry_state.lock().unwrap().status.clone();
        if current_status == self.last_sdk_status {
            return;
        }

        if current_status == SdkStatus::Ready {
            // Persist current catalog
            let models: Vec<CachedModel> = self
                .foundry_state
                .lock()
                .unwrap()
                .models
                .iter()
                .map(|m| CachedModel {
                    alias: m.alias.clone(),
                    device: m.device.clone(),
                    is_cached: m.is_cached,
                })
                .collect();
            self.config.cached_catalog = models;
            self.config.save();

            // Auto-load preferred model
            if !self.auto_load_triggered {
                if let Some(alias) = self.config.preferred_model.clone() {
                    self.auto_load_triggered = true;
                    self.trigger_load(alias);
                }
            }
        }

        self.last_sdk_status = current_status;
    }

    fn trigger_load(&self, alias: String) {
        let state_c = Arc::clone(&self.foundry_state);
        let url_c = Arc::clone(&self.shared_url);
        let ctx_size = self.config.context_size;
        foundry::load_model(alias, state_c, url_c, ctx_size, |_| {});
    }

    fn trigger_unload(&self, alias: String) {
        let state_c = Arc::clone(&self.foundry_state);
        let url_c = Arc::clone(&self.shared_url);
        let ctx_size = self.config.context_size;
        foundry::unload_model(alias, state_c, url_c, ctx_size, |_| {});
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Hide to tray on close
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }

        // Tray left-click
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }

        // Tray menu
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.menu_show_id {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            } else if event.id == self.menu_quit_id {
                self.stop_proxy();
                std::process::exit(0);
            }
        }

        self.poll_proxy_error();
        self.poll_sdk_transition();

        egui::CentralPanel::default().show(ctx, |ui| {
            ScrollArea::vertical().id_salt("main_scroll").show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading(RichText::new("Foundry Local Proxy").size(18.0).strong());
            ui.add_space(4.0);
            ui.separator();

            // ── Proxy status ─────────────────────────────────────────────
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let (dot_color, label) = if self.proxy_running {
                    (Color32::from_rgb(40, 200, 80), "Running")
                } else {
                    (Color32::from_rgb(200, 50, 50), "Stopped")
                };
                ui.colored_label(dot_color, "●");
                ui.label(RichText::new(label).strong());
            });

            if self.proxy_running {
                let url = self.config.proxy_url();
                ui.horizontal(|ui| {
                    ui.label("Endpoint:");
                    ui.monospace(format!("{}/v1", url));
                    if ui.small_button("Copy").clicked() {
                        ui.output_mut(|o| o.copied_text = format!("{}/v1", url));
                    }
                });
            }

            if let Some(err) = &self.proxy_error.clone() {
                ui.colored_label(Color32::RED, format!("Error: {}", err));
            }

            ui.add_space(4.0);
            ui.separator();

            // ── Model catalog ────────────────────────────────────────────
            ui.add_space(6.0);
            {
                let (sdk_status, models, progress_label, progress_pct, last_msg) = {
                    let s = self.foundry_state.lock().unwrap();
                    (
                        s.status.clone(),
                        s.models.clone(),
                        s.progress_label.clone(),
                        s.progress_pct,
                        s.last_message.clone(),
                    )
                };

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Models").strong());
                    ui.add_space(6.0);
                    let (badge_color, badge_text) = match &sdk_status {
                        SdkStatus::Uninitialized => (Color32::GRAY, "Uninitialized"),
                        SdkStatus::Initializing => (Color32::GOLD, "Initializing…"),
                        SdkStatus::Ready => (Color32::from_rgb(40, 200, 80), "Ready"),
                        SdkStatus::Error(_) => (Color32::RED, "Error"),
                    };
                    ui.colored_label(badge_color, badge_text);
                });

                if let SdkStatus::Error(e) = &sdk_status {
                    ui.colored_label(Color32::RED, e);
                }

                if let Some(label) = &progress_label {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(label);
                        let bar = egui::ProgressBar::new(progress_pct as f32 / 100.0)
                            .desired_width(160.0);
                        ui.add(bar);
                    });
                }

                if let Some(msg) = &last_msg {
                    ui.small(msg);
                }

                // Filter input
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.model_filter)
                            .hint_text("Filter models…")
                            .desired_width(f32::INFINITY),
                    );
                    if !self.model_filter.is_empty() && ui.small_button("✕").clicked() {
                        self.model_filter.clear();
                    }
                });

                // state.models is pre-seeded from cached_catalog on startup,
                // then updated by the SDK — always use it directly.
                let filter = self.model_filter.to_lowercase();
                let display_models: Vec<(String, String, bool, bool)> = models
                    .iter()
                    .filter(|m| filter.is_empty() || m.alias.to_lowercase().contains(&filter))
                    .map(|m| (m.alias.clone(), m.device.clone(), m.is_cached, m.is_loaded))
                    .collect();

                let mut load_alias: Option<String> = None;
                let mut unload_alias: Option<String> = None;
                let mut set_preferred: Option<String> = None;

                // ~6 rows visible; row height ≈ 22px + spacing
                ScrollArea::vertical()
                    .id_salt("model_list")
                    .max_height(6.0 * 26.0)
                    .show(ui, |ui| {
                for (alias, device, is_cached, is_loaded) in &display_models {
                    let is_preferred = self.config.preferred_model.as_deref() == Some(alias.as_str());
                    ui.horizontal(|ui| {
                        // Cached-on-disk indicator
                        let (cache_dot, cache_tip) = if *is_loaded {
                            (RichText::new("●").color(Color32::from_rgb(40, 200, 80)), "Loaded in memory")
                        } else if *is_cached {
                            (RichText::new("●").color(Color32::from_rgb(100, 160, 100)), "Cached on disk — ready to load")
                        } else {
                            (RichText::new("○").color(Color32::GRAY), "Not downloaded")
                        };
                        ui.label(cache_dot).on_hover_text(cache_tip);

                        // Device badge
                        let dev_color = match device.as_str() {
                            "NPU" => Color32::from_rgb(120, 80, 220),
                            "GPU" => Color32::from_rgb(220, 120, 30),
                            _ => Color32::from_rgb(80, 130, 180),
                        };
                        ui.colored_label(dev_color, format!("[{}]", device));

                        let alias_text = if is_preferred {
                            RichText::new(alias).strong()
                        } else {
                            RichText::new(alias)
                        };
                        ui.label(alias_text);

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if *is_loaded {
                                if ui.small_button("Unload").clicked() {
                                    unload_alias = Some(alias.clone());
                                }
                                ui.colored_label(Color32::from_rgb(40, 200, 80), "Active");
                            } else if *is_cached {
                                if ui.small_button("Load").on_hover_text("Load from disk into memory").clicked() {
                                    load_alias = Some(alias.clone());
                                }
                                if !is_preferred {
                                    if ui.small_button("★").on_hover_text("Set as default model").clicked() {
                                        set_preferred = Some(alias.clone());
                                    }
                                }
                            } else {
                                if ui.add(egui::Button::new(
                                    RichText::new("⬇ Download").color(Color32::WHITE))
                                    .fill(Color32::from_rgb(60, 100, 160)))
                                    .on_hover_text("Download model to disk, then load")
                                    .clicked()
                                {
                                    load_alias = Some(alias.clone());
                                }
                            }
                        });
                    });
                }
                    }); // end model ScrollArea

                if let Some(alias) = set_preferred {
                    self.config.preferred_model = Some(alias);
                    self.config.save();
                }
                if let Some(alias) = load_alias {
                    self.config.preferred_model = Some(alias.clone());
                    self.config.save();
                    self.trigger_load(alias);
                }
                if let Some(alias) = unload_alias {
                    self.config.preferred_model = None;
                    self.config.save();
                    self.trigger_unload(alias);
                }
            }

            ui.add_space(4.0);
            ui.separator();

            // ── Settings ─────────────────────────────────────────────────
            ui.add_space(6.0);
            ui.label(RichText::new("Settings").strong());

            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Port:");
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.edit_port);
                        if ui.small_button("Apply").clicked() {
                            self.apply_port();
                        }
                    });
                    ui.end_row();

                    ui.label("Bind address:");
                    let old_bind = self.config.bind_all;
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.config.bind_all, false, "localhost");
                        ui.radio_value(&mut self.config.bind_all, true, "0.0.0.0  (network)");
                    });
                    if self.config.bind_all != old_bind {
                        self.config.save();
                    }
                    ui.end_row();

                    ui.label("Auto-start:");
                    let old_auto = self.config.autostart;
                    ui.checkbox(&mut self.config.autostart, "Start proxy on launch");
                    if self.config.autostart != old_auto {
                        self.config.save();
                    }
                    ui.end_row();

                    ui.label("Context window:");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.edit_context)
                                .desired_width(60.0),
                        );
                        ui.label("tokens");
                        if ui.small_button("Apply").clicked() {
                            self.apply_context_size();
                        }
                    });
                    ui.end_row();

                    // Preset buttons
                    ui.label("");
                    ui.horizontal(|ui| {
                        for (label, val) in [("4K", 4096u32), ("8K", 8192), ("16K", 16384), ("32K", 32768), ("64K", 65536), ("128K", 131072)] {
                            if ui.small_button(label).clicked() {
                                self.config.context_size = val;
                                self.edit_context = val.to_string();
                                self.config.save();
                            }
                        }
                    });
                    ui.end_row();
                });

            ui.small(
                RichText::new("Context change takes effect on next model load.")
                    .color(Color32::GRAY),
            );

            ui.add_space(4.0);
            ui.separator();

            // ── Authentication ───────────────────────────────────────────
            ui.add_space(6.0);
            ui.label(RichText::new("Authentication").strong());
            let old_auth = self.config.require_auth;
            ui.checkbox(&mut self.config.require_auth, "Require Bearer token (sk-...)");
            if self.config.require_auth != old_auth {
                self.config.save();
            }

            if self.config.require_auth {
                ui.add_space(4.0);
                let height = (self.config.tokens.len() as f32 * 28.0 + 8.0).min(120.0);
                let mut to_delete: Option<usize> = None;
                let mut to_copy: Option<String> = None;

                ScrollArea::vertical()
                    .max_height(height)
                    .id_salt("token_scroll")
                    .show(ui, |ui| {
                        for (i, token) in self.config.tokens.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let display = if self.show_token_full == Some(i) {
                                    token.clone()
                                } else {
                                    format!("{}…", &token[..token.len().min(22)])
                                };
                                ui.monospace(
                                    RichText::new(&display)
                                        .font(FontId::monospace(11.0))
                                        .color(Color32::from_rgb(180, 220, 255)),
                                );
                                if ui.small_button("👁").on_hover_text("Show / hide").clicked() {
                                    self.show_token_full = if self.show_token_full == Some(i) {
                                        None
                                    } else {
                                        Some(i)
                                    };
                                }
                                if ui.small_button("Copy").clicked() {
                                    to_copy = Some(token.clone());
                                }
                                if ui.small_button("✖").clicked() {
                                    to_delete = Some(i);
                                }
                            });
                        }
                    });

                if let Some(t) = to_copy {
                    ui.output_mut(|o| o.copied_text = t);
                }
                if let Some(i) = to_delete {
                    self.config.tokens.remove(i);
                    if self.show_token_full == Some(i) {
                        self.show_token_full = None;
                    }
                    self.config.save();
                }

                if ui.button("＋ Generate Token").clicked() {
                    self.config.tokens.push(generate_token());
                    self.config.save();
                }

                if self.config.tokens.is_empty() {
                    ui.label(
                        RichText::new("No tokens — all requests will be rejected.")
                            .color(Color32::YELLOW)
                            .italics(),
                    );
                }
            }

            ui.add_space(4.0);
            ui.separator();

            // ── Start / Stop / Exit ──────────────────────────────────────
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.proxy_running {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("■  Stop").color(Color32::WHITE))
                                .fill(Color32::from_rgb(180, 40, 40)),
                        )
                        .clicked()
                    {
                        self.stop_proxy();
                    }
                } else if ui
                    .add(
                        egui::Button::new(RichText::new("▶  Start").color(Color32::WHITE))
                            .fill(Color32::from_rgb(40, 150, 40)),
                    )
                    .clicked()
                {
                    self.apply_port();
                    self.start_proxy();
                }

                ui.add_space(8.0);

                if ui
                    .add(
                        egui::Button::new(RichText::new("Exit").color(Color32::WHITE))
                            .fill(Color32::from_rgb(80, 80, 80)),
                    )
                    .on_hover_text("Stop proxy and quit")
                    .clicked()
                {
                    self.stop_proxy();
                    std::process::exit(0);
                }
            });

            ui.add_space(6.0);
            ui.label(
                RichText::new("Closing the window minimizes to tray.")
                    .small()
                    .color(Color32::GRAY),
            );
            }); // end main ScrollArea
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let s: String = (0..48)
        .map(|_| {
            let i: usize = rng.gen_range(0..36);
            if i < 10 { (b'0' + i as u8) as char } else { (b'a' + (i - 10) as u8) as char }
        })
        .collect();
    format!("sk-{}", s)
}

fn build_tray_icon() -> tray_icon::Icon {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let outer = (size as f32 / 2.0 - 1.0).powi(2);
    let inner = (size as f32 / 2.0 - 5.0).powi(2);
    for y in 0..size {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let d2 = dx * dx + dy * dy;
            if d2 <= outer {
                if d2 <= inner {
                    rgba[i] = 30; rgba[i+1] = 140; rgba[i+2] = 220; rgba[i+3] = 255;
                } else {
                    rgba[i] = 0; rgba[i+1] = 120; rgba[i+2] = 215; rgba[i+3] = 255;
                }
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, size, size).expect("Failed to build tray icon")
}
