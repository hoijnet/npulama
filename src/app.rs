use egui::{Color32, Context, FontId, RichText, ScrollArea, ViewportCommand};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use tokio::task::JoinHandle;
use tokio::sync::oneshot;

use crate::config::Config;
use crate::proxy::start_proxy;

pub struct App {
    config: Config,

    // Pending edits (not yet applied)
    edit_port: String,
    edit_upstream: String,

    proxy_running: bool,
    proxy_shutdown: Option<oneshot::Sender<()>>,
    proxy_handle: Option<JoinHandle<Result<(), String>>>,
    proxy_error: Option<String>,

    show_token_full: Option<usize>,

    // Keep tray icon alive and store menu item IDs
    _tray: TrayIcon,
    menu_show_id: tray_icon::menu::MenuId,
    menu_quit_id: tray_icon::menu::MenuId,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        let icon = build_tray_icon();

        let tray_menu = Menu::new();
        let show_item = MenuItem::new("Open", true, None);
        let quit_item = MenuItem::new("Exit", true, None);
        tray_menu
            .append_items(&[
                &show_item,
                &PredefinedMenuItem::separator(),
                &quit_item,
            ])
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
        let edit_upstream = config.upstream_url.clone();

        let mut app = Self {
            config,
            edit_port,
            edit_upstream,
            proxy_running: false,
            proxy_shutdown: None,
            proxy_handle: None,
            proxy_error: None,
            show_token_full: None,
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
        let (tx, handle) = start_proxy(self.config.clone());
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

    fn apply_settings(&mut self) {
        if let Ok(port) = self.edit_port.trim().parse::<u16>() {
            self.config.port = port;
        } else {
            self.edit_port = self.config.port.to_string();
        }
        self.config.upstream_url = self.edit_upstream.trim().to_string();
        self.config.save();

        if self.proxy_running {
            self.stop_proxy();
            self.start_proxy();
        }
    }

    fn poll_proxy_error(&mut self) {
        if let Some(handle) = &self.proxy_handle {
            if handle.is_finished() {
                // Retrieve error if any
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
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Intercept window close → hide to tray
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }

        // Poll tray icon left-click
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = event
            {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }

        // Poll tray menu events
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.menu_show_id {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            } else if event.id == self.menu_quit_id {
                self.stop_proxy();
                std::process::exit(0);
            }
        }

        // Check if proxy task exited unexpectedly
        self.poll_proxy_error();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading(RichText::new("Foundry Local Proxy").size(18.0).strong());
            ui.add_space(4.0);
            ui.separator();

            // ── Status ──────────────────────────────────────────────────
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

            // ── Settings ────────────────────────────────────────────────
            ui.add_space(6.0);
            ui.label(RichText::new("Server Settings").strong());

            egui::Grid::new("settings_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Port:");
                    ui.text_edit_singleline(&mut self.edit_port);
                    ui.end_row();

                    ui.label("Bind address:");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.config.bind_all, false, "localhost");
                        ui.radio_value(&mut self.config.bind_all, true, "0.0.0.0  (network)");
                    });
                    ui.end_row();

                    ui.label("Foundry URL:");
                    ui.text_edit_singleline(&mut self.edit_upstream);
                    ui.end_row();

                    ui.label("Auto-start:");
                    ui.checkbox(&mut self.config.autostart, "Start proxy on launch");
                    ui.end_row();
                });

            if ui.button("Apply Settings").clicked() {
                self.apply_settings();
            }

            ui.add_space(4.0);
            ui.separator();

            // ── Authentication ───────────────────────────────────────────
            ui.add_space(6.0);
            ui.label(RichText::new("Authentication").strong());
            ui.checkbox(&mut self.config.require_auth, "Require Bearer token (sk-...)");

            if self.config.require_auth {
                ui.add_space(4.0);

                let height = (self.config.tokens.len() as f32 * 28.0 + 8.0).min(140.0);
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
                                if ui.small_button("👁").on_hover_text("Show / hide full token").clicked() {
                                    self.show_token_full = if self.show_token_full == Some(i) {
                                        None
                                    } else {
                                        Some(i)
                                    };
                                }
                                if ui.small_button("Copy").clicked() {
                                    to_copy = Some(token.clone());
                                }
                                if ui.small_button("✖").on_hover_text("Delete token").clicked() {
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
                    let token = generate_token();
                    self.config.tokens.push(token);
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
                } else {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("▶  Start").color(Color32::WHITE))
                                .fill(Color32::from_rgb(40, 150, 40)),
                        )
                        .clicked()
                    {
                        if let Ok(port) = self.edit_port.trim().parse::<u16>() {
                            self.config.port = port;
                        }
                        self.config.upstream_url = self.edit_upstream.trim().to_string();
                        self.start_proxy();
                    }
                }

                ui.add_space(8.0);

                if ui
                    .add(egui::Button::new(RichText::new("Exit").color(Color32::WHITE))
                        .fill(Color32::from_rgb(80, 80, 80)))
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
        });

        // Poll frequently enough to catch tray events quickly
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn generate_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let s: String = (0..48)
        .map(|_| {
            let i: usize = rng.gen_range(0..36);
            if i < 10 {
                (b'0' + i as u8) as char
            } else {
                (b'a' + (i - 10) as u8) as char
            }
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
                    // Inner fill: slightly lighter blue
                    rgba[i] = 30;
                    rgba[i + 1] = 140;
                    rgba[i + 2] = 220;
                    rgba[i + 3] = 255;
                } else {
                    // Ring: bright blue
                    rgba[i] = 0;
                    rgba[i + 1] = 120;
                    rgba[i + 2] = 215;
                    rgba[i + 3] = 255;
                }
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, size, size).expect("Failed to build tray icon")
}
