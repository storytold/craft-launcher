//! ArtCraft Launcher: install, update and open the Craft App Suite.
//!
//! The window can close to the tray (menu bar) and keep checking for updates, or quit entirely.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "macos")]
mod spaces;
mod tray;

use std::process::ExitCode;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};

use artcraft_launcher_native::single_instance::{self, Instance};
use artcraft_launcher_native::{NativeBackend, login};
use artcraft_launcher_ui_egui::theme::Tokens;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings, TitleBar};

const APP_ID: &str = "ai.storyteller.artcraft-launcher";
const SETTINGS_KEY: &str = "settings";

type Wake = Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("artcraft-launcher {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let hidden = args.iter().any(|a| a == login::HIDDEN_FLAG);

    let wake: Wake = Arc::new(OnceLock::new());
    let show_requests = match single_instance::claim(wake.clone()) {
        Instance::Secondary => return ExitCode::SUCCESS,
        Instance::Primary(rx) => rx,
    };

    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/app-icon/hicolor/256x256/apps/ai.storyteller.artcraft-launcher.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ArtCraft Launcher")
        .with_app_id(APP_ID)
        .with_inner_size([1120.0, 760.0])
        .with_min_inner_size([720.0, 500.0])
        .with_visible(!hidden);
    // The header is the title bar (see `DesktopHost::title_bar`).
    viewport = if cfg!(target_os = "macos") {
        viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false)
    } else {
        viewport.with_decorations(false)
    };
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, persist_window: true, ..Default::default() };
    let result = eframe::run_native(
        "ArtCraft Launcher",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let _ = wake.set(Box::new(move || ctx.request_repaint()));
            Ok(Box::new(Shell::new(cc, show_requests, hidden)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

struct DesktopHost {
    tray: bool,
    quit: bool,
}

impl Host for DesktopHost {
    fn title_bar(&self) -> TitleBar {
        if cfg!(target_os = "macos") { TitleBar::MacOs } else { TitleBar::Custom }
    }

    fn notify(&mut self, title: &str, body: &str) {
        let (title, body) = (title.to_string(), body.to_string());
        // Some platforms block while the notification is delivered.
        std::thread::spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.summary(&title).body(&body).appname("ArtCraft Launcher");
            #[cfg(windows)]
            n.app_id(APP_ID);
            #[cfg(all(unix, not(target_os = "macos")))]
            n.icon(APP_ID);
            if let Err(e) = n.show() {
                log::warn!("notification: {e}");
            }
        });
    }

    fn has_tray(&self) -> bool {
        self.tray
    }

    fn has_desktops(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn login_item(&self) -> Option<bool> {
        Some(login::is_enabled())
    }

    fn set_login_item(&mut self, on: bool) -> Result<(), String> {
        login::set_enabled(on)
    }

    fn quit(&mut self) {
        self.quit = true;
    }
}

struct Shell {
    ui: LauncherUi,
    tray: Option<tray::Tray>,
    host: DesktopHost,
    show_requests: Receiver<()>,
    visible: bool,
    quitting: bool,
    /// The "show on the current desktop" setting last applied to the window (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    applied_follow: Option<bool>,
}

impl Shell {
    fn new(cc: &eframe::CreationContext<'_>, show_requests: Receiver<()>, hidden: bool) -> Self {
        let ctx = cc.egui_ctx.clone();
        let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
        let waker_ctx = ctx.clone();
        let backend = NativeBackend::new(move || waker_ctx.request_repaint());
        let tray_ctx = ctx.clone();
        let tray = match tray::Tray::new(move || tray_ctx.request_repaint()) {
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!("no tray icon: {e}");
                None
            }
        };
        let host = DesktopHost { tray: tray.is_some(), quit: false };
        // Started hidden at login but there's no tray to come back from: show the window.
        let visible = !hidden || tray.is_none();
        if !visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        } else if hidden {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        Self { ui: LauncherUi::new(&ctx, Box::new(backend), settings), tray, host, show_requests, visible, quitting: false, applied_follow: None }
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.visible = true;
        // Before the window is ordered front, so it comes to this desktop rather than taking the
        // user to its own (macOS; the setting doesn't exist elsewhere).
        #[cfg(target_os = "macos")]
        {
            let on = self.ui.settings.show_on_current_desktop;
            spaces::follow_current_desktop(on);
            self.applied_follow = Some(on);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide(&mut self, ctx: &egui::Context) {
        self.visible = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui.logic(ctx, &mut self.host);
        #[cfg(target_os = "macos")]
        if self.applied_follow != Some(self.ui.settings.show_on_current_desktop) {
            let on = self.ui.settings.show_on_current_desktop;
            spaces::follow_current_desktop(on);
            self.applied_follow = Some(on);
        }
        if self.show_requests.try_iter().count() > 0 {
            self.show(ctx);
        }
        let actions = self.tray.as_ref().map(tray::Tray::actions).unwrap_or_default();
        for action in actions {
            match action {
                tray::TrayAction::Toggle if self.visible => self.hide(ctx),
                tray::TrayAction::Toggle | tray::TrayAction::Show => self.show(ctx),
                tray::TrayAction::Check => self.ui.check_now(),
                // Straight to the browser: works while the window is hidden in the tray.
                tray::TrayAction::Open(url) => {
                    if let Err(e) = artcraft_launcher_native::process::open(std::ffi::OsStr::new(url)) {
                        log::warn!("couldn't open {url}: {e}");
                    }
                }
                tray::TrayAction::Quit => self.quit(ctx),
            }
        }
        if let Some(t) = self.tray.as_mut() {
            t.set_updates(self.ui.update_count());
        }
        if self.host.quit {
            self.host.quit = false;
            self.quit(ctx);
        }
        // The close button hides to the tray unless the user chose to quit.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.tray.is_some() && self.ui.settings.close_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui.ui(ui, &mut self.host);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.ui.settings);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        (if visuals.dark_mode { Tokens::DARK.bg } else { Tokens::LIGHT.bg }).to_normalized_gamma_f32()
    }
}
