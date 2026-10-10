//! [`LauncherUi`]: state, event handling and the page layout.

use std::time::Duration;

use artcraft_launcher_core::protocol::{Backend, BackendOptions, CANCELLED, Capabilities, Command, Event, LatestRelease, PAUSED, Source};
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::status::AppStatus;
use artcraft_launcher_core::{Platform, SUITE};

use crate::model::AppEntry;
use crate::settings::{Filter, Settings, SortKey, ViewMode};
use crate::theme::{self, Tokens};

/// Who draws the window's title bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TitleBar {
    /// Nothing to draw: the OS draws one, or there's no window (the web build).
    #[default]
    Native,
    /// The window has no OS title bar: the header is the title bar (drag it to move the window,
    /// double-click to maximize), with minimize, maximize and close at its right end, and the
    /// window's edges resize it.
    Custom,
    /// macOS: the header runs under a transparent title bar, with the traffic lights at its left.
    MacOs,
}

/// What only the shell can do. Every method has a do-nothing default (the web build).
pub trait Host {
    /// Who draws the title bar.
    fn title_bar(&self) -> TitleBar {
        TitleBar::Native
    }
    /// Shows a system notification.
    fn notify(&mut self, _title: &str, _body: &str) {}
    /// A tray (menu bar) icon exists, so closing the window can keep the launcher running.
    fn has_tray(&self) -> bool {
        false
    }
    /// The OS has desktops (macOS Spaces) the window can be shown on, so the "show on the current
    /// desktop" setting applies.
    fn has_desktops(&self) -> bool {
        false
    }
    /// `Some(enabled)` where starting at login is supported.
    fn login_item(&self) -> Option<bool> {
        None
    }
    fn set_login_item(&mut self, _on: bool) -> Result<(), String> {
        Err("not supported here".into())
    }
    /// Quit the launcher for real (not just hide to the tray).
    fn quit(&mut self) {}
}

pub(crate) struct Toast {
    pub text: String,
    pub error: bool,
    pub until: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dialog {
    Notes(String),
    ConfirmUninstall(String),
    Settings,
}

pub struct LauncherUi {
    pub(crate) backend: Box<dyn Backend>,
    pub settings: Settings,
    pub(crate) entries: Vec<AppEntry>,
    pub(crate) caps: Capabilities,
    pub(crate) platform: Option<Platform>,
    pub(crate) checking: bool,
    pub(crate) last_source: Option<Source>,
    pub(crate) rate_limited_until: Option<i64>,
    pub(crate) launcher_update: Option<Release>,
    /// The launcher's own update, while it downloads and installs.
    pub(crate) launcher_job: Option<artcraft_launcher_core::protocol::JobPhase>,
    /// The new launcher is in place: quit so it can start.
    restart_requested: bool,
    pub(crate) search: String,
    pub(crate) toasts: Vec<Toast>,
    pub(crate) dialog: Option<Dialog>,
    started: bool,
    applied_theme: Option<theme::ThemeChoice>,
    /// Fonts registered with `set_fonts` only exist from the next frame on; drawing with the named
    /// weights before then would abort.
    fonts_ready: bool,
    sent_options: Option<BackendOptions>,
    /// Unix seconds of the last look at what's installed (startup counts).
    last_detect: i64,
    /// Whether the window had focus last frame, to re-check when the user comes back to it.
    was_focused: bool,
}

/// How often to look again at what's installed, to notice apps installed or removed outside
/// the launcher. A detection pass lists packages and reads a few files: cheap, but not free.
const RESCAN_EVERY: i64 = 5 * 60;
/// Coming back to the window re-checks, but not more often than this.
const RESCAN_ON_FOCUS_AFTER: i64 = 30;

impl LauncherUi {
    pub fn new(ctx: &egui::Context, backend: Box<dyn Backend>, settings: Settings) -> Self {
        theme::install_fonts(ctx);
        egui_extras::install_image_loaders(ctx);
        let caps = backend.capabilities();
        let platform = backend.platform();
        Self {
            backend,
            settings,
            entries: SUITE.iter().map(AppEntry::new).collect(),
            caps,
            platform,
            checking: false,
            last_source: None,
            rate_limited_until: None,
            launcher_update: None,
            launcher_job: None,
            restart_requested: false,
            search: String::new(),
            toasts: Vec::new(),
            dialog: None,
            started: false,
            applied_theme: None,
            fonts_ready: false,
            sent_options: None,
            last_detect: 0,
            was_focused: true,
        }
    }

    /// Installed apps with an update waiting (the tray shows the count).
    pub fn update_count(&self) -> usize {
        self.entries.iter().filter(|e| e.status(self.platform, self.caps.manage_apps) == AppStatus::UpdateAvailable).count()
    }

    /// Looks for updates now (the tray's "Check for Updates").
    pub fn check_now(&mut self) {
        self.backend.send(Command::CheckForUpdates { force: true });
        self.backend.send(Command::Detect);
        self.last_detect = self.backend.now();
    }

    pub(crate) fn send(&mut self, command: Command) {
        self.backend.send(command);
    }

    pub(crate) fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>, error: bool) {
        let now = ctx.input(|i| i.time);
        self.toasts.push(Toast { text: text.into(), error, until: now + if error { 8.0 } else { 4.5 } });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    pub(crate) fn entry(&self, id: &str) -> Option<&AppEntry> {
        self.entries.iter().find(|e| e.app.id == id)
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut AppEntry> {
        self.entries.iter_mut().find(|e| e.app.id == id)
    }

    /// Non-drawing work: runs every frame and also while the window is hidden in the tray.
    pub fn logic(&mut self, ctx: &egui::Context, host: &mut dyn Host) {
        let options = BackendOptions { kinds: self.settings.preferred_kinds.clone(), prereleases: self.settings.prereleases };
        if self.sent_options.as_ref() != Some(&options) {
            self.backend.send(Command::Configure(options.clone()));
            self.sent_options = Some(options);
        }
        if !self.started {
            self.started = true;
            self.backend.send(Command::CheckForUpdates { force: false });
        }
        let now = self.backend.now();
        if self.last_detect == 0 {
            // The backend looks once at startup by itself.
            self.last_detect = now;
        }
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let came_back = focused && !self.was_focused && now - self.last_detect >= RESCAN_ON_FOCUS_AFTER;
        self.was_focused = focused;
        if came_back || now - self.last_detect >= RESCAN_EVERY {
            self.last_detect = now;
            self.backend.send(Command::Detect);
        }
        if let Some(every) = self.settings.check_every.seconds()
            && !self.checking
            && self.settings.last_check.is_none_or(|t| now - t >= every)
        {
            self.backend.send(Command::CheckForUpdates { force: false });
        }
        let mut changed = false;
        for event in self.backend.poll() {
            changed |= self.handle(ctx, event);
        }
        if changed {
            self.announce_updates(host);
        }
        if self.restart_requested {
            // The helper is waiting for this process to exit before it starts the new launcher.
            self.restart_requested = false;
            host.quit();
        }
        // Wake up now and then for scheduled checks, even in the tray.
        ctx.request_repaint_after(Duration::from_secs(60));
    }

    /// Returns whether app state changed.
    fn handle(&mut self, ctx: &egui::Context, event: Event) -> bool {
        match event {
            Event::Latest { app, latest } => {
                if let Some(e) = self.entry_mut(&app) {
                    // A failed check keeps the release we already know about.
                    if !(matches!(latest, LatestRelease::Failed(_)) && matches!(e.latest, Some(LatestRelease::Found(_)))) {
                        e.latest = Some(latest);
                    }
                }
                true
            }
            Event::Detected { app, installed } => {
                if let Some(e) = self.entry_mut(&app) {
                    e.installed = installed;
                    e.detected = true;
                }
                true
            }
            Event::Progress { app, phase } => {
                if app == artcraft_launcher_core::catalog::LAUNCHER_ID {
                    self.launcher_job = Some(phase);
                } else if let Some(e) = self.entry_mut(&app) {
                    e.job = Some(phase);
                }
                false
            }
            Event::LauncherRestarting => {
                self.restart_requested = true;
                false
            }
            Event::Paused { app, download } => {
                if let Some(e) = self.entry_mut(&app) {
                    e.paused = download;
                }
                false
            }
            Event::JobFinished { app, result } => {
                if app == artcraft_launcher_core::catalog::LAUNCHER_ID {
                    self.launcher_job = None;
                }
                if let Some(e) = self.entry_mut(&app) {
                    e.job = None;
                }
                match result {
                    Ok(msg) => self.toast(ctx, msg, false),
                    // The paused card says it all.
                    Err(e) if e == PAUSED => {}
                    Err(e) if e == CANCELLED => self.toast(ctx, "Download cancelled", false),
                    Err(e) => self.toast(ctx, e, true),
                }
                true
            }
            Event::LauncherUpdate(r) => {
                self.launcher_update = Some(r);
                false
            }
            Event::CheckStarted => {
                self.checking = true;
                false
            }
            Event::CheckFinished { at, source, rate_limited_until } => {
                self.checking = false;
                self.settings.last_check = Some(at);
                self.last_source = Some(source);
                self.rate_limited_until = rate_limited_until;
                true
            }
        }
    }

    /// One notification per new version of an installed app.
    fn announce_updates(&mut self, host: &mut dyn Host) {
        let mut fresh = Vec::new();
        for e in &self.entries {
            if e.status(self.platform, self.caps.manage_apps) != AppStatus::UpdateAvailable {
                continue;
            }
            let Some(v) = e.latest_label() else { continue };
            if self.settings.notified.get(e.app.id) != Some(&v) {
                fresh.push((e.app.id, e.app.name, v));
            }
        }
        if fresh.is_empty() {
            return;
        }
        if self.settings.notify {
            let (title, body) = match fresh.as_slice() {
                [(_, name, v)] => (format!("{name} {v} is available"), "Open ArtCraft Launcher to update.".to_string()),
                many => {
                    (format!("{} updates are available", many.len()), many.iter().map(|(_, n, v)| format!("{n} {v}")).collect::<Vec<_>>().join(", "))
                }
            };
            host.notify(&title, &body);
        }
        for (id, _, version) in fresh {
            self.settings.notified.insert(id.to_string(), version);
        }
    }

    /// Draws the launcher.
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) {
        let ctx = ui.ctx().clone();
        if !self.fonts_ready {
            self.fonts_ready = true;
            ctx.request_repaint();
            return;
        }
        if self.applied_theme != Some(self.settings.theme) {
            theme::apply(&ctx, self.settings.theme);
            self.applied_theme = Some(self.settings.theme);
        }
        let t = Tokens::get(&ctx);
        if self.entries.iter().any(|e| e.job.is_some()) || self.checking {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // The bars' separator lines are the hairlines under the header and over the footer.
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(t.bg))
            .exact_size(crate::views::HEADER_H)
            .resizable(false)
            .show(ui, |ui| self.header(ui, host.title_bar()));
        egui::Panel::bottom("footer")
            .frame(egui::Frame::new().fill(t.bg).inner_margin(egui::Margin::symmetric(20, 0)))
            .exact_size(crate::views::FOOTER_H)
            .resizable(false)
            .show(ui, |ui| self.footer(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(t.bg).inner_margin(egui::Margin { left: 20, right: 20, top: 18, bottom: 0 }))
            .show(ui, |ui| {
                self.toolbar(ui);
                ui.add_space(18.0);
                let ids = self.visible_ids();
                // The table is wider than a narrow window: let it scroll sideways too.
                let scroll = match self.settings.view {
                    ViewMode::Grid => egui::ScrollArea::vertical(),
                    ViewMode::Table => egui::ScrollArea::both(),
                };
                scroll.auto_shrink(false).show(ui, |ui| {
                    if ids.is_empty() {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            crate::widgets::hud(ui, "No apps match", t.muted);
                        });
                    } else {
                        match self.settings.view {
                            ViewMode::Grid => self.grid(ui, &ids),
                            ViewMode::Table => self.table(ui, &ids),
                        }
                    }
                    self.trademark_notice(ui);
                    ui.add_space(16.0);
                });
            });
        self.dialogs(&ctx, host);
        self.draw_toasts(&ctx);
        if host.title_bar() == TitleBar::Custom {
            crate::widgets::window_edges(&ctx);
        }
    }

    /// The ids to show, filtered and sorted.
    pub(crate) fn visible_ids(&self) -> Vec<&'static str> {
        let needle = self.search.trim().to_lowercase();
        let mut list: Vec<(usize, &AppEntry)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let s = e.status(self.platform, self.caps.manage_apps);
                let keep = match self.settings.filter {
                    Filter::All => true,
                    Filter::Installed => s.is_installed(),
                    Filter::Updates => s == AppStatus::UpdateAvailable,
                    Filter::NotInstalled => !s.is_installed(),
                };
                keep && (needle.is_empty()
                    || e.app.name.to_lowercase().contains(&needle)
                    || e.app.tagline.to_lowercase().contains(&needle)
                    || e.app.like.to_lowercase().contains(&needle))
            })
            .collect();
        let platform = self.platform;
        let manage = self.caps.manage_apps;
        let desc = self.settings.sort_descending;
        let dir = |o: std::cmp::Ordering| if desc { o.reverse() } else { o };
        // Empty cells (not installed, no release) go last whichever way the column sorts.
        let opt = |a: Option<std::cmp::Ordering>, a_some: bool, b_some: bool| match (a_some, b_some) {
            (true, true) => dir(a.unwrap_or(std::cmp::Ordering::Equal)),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => std::cmp::Ordering::Equal,
        };
        let installed = |e: &AppEntry| e.installed.as_ref().and_then(|i| i.version.clone());
        let latest = |e: &AppEntry| e.release().and_then(|r| r.version.clone());
        list.sort_by(|(ia, a), (ib, b)| {
            let ord = match self.settings.sort {
                SortKey::Suite => dir(ia.cmp(ib)),
                SortKey::Name => dir(a.app.name.cmp(b.app.name)),
                SortKey::InspiredBy => dir(a.app.like.cmp(b.app.like)),
                SortKey::Status => {
                    dir(crate::model::status_rank(&a.status(platform, manage)).cmp(&crate::model::status_rank(&b.status(platform, manage))))
                }
                SortKey::Installed => {
                    let (x, y) = (installed(a), installed(b));
                    opt(x.as_ref().zip(y.as_ref()).map(|(x, y)| x.cmp_precedence(y)), x.is_some(), y.is_some())
                }
                SortKey::Latest => {
                    let (x, y) = (latest(a), latest(b));
                    opt(x.as_ref().zip(y.as_ref()).map(|(x, y)| x.cmp_precedence(y)), x.is_some(), y.is_some())
                }
                SortKey::Released => {
                    let at = |e: &AppEntry| e.release().and_then(|r| r.published_at);
                    let (x, y) = (at(a), at(b));
                    opt(x.zip(y).map(|(x, y)| x.cmp(&y)), x.is_some(), y.is_some())
                }
            };
            // Ties keep suite order.
            ord.then(ia.cmp(ib))
        });
        list.into_iter().map(|(_, e)| e.app.id).collect()
    }

    pub(crate) fn count(&self, filter: Filter) -> usize {
        self.entries
            .iter()
            .filter(|e| {
                let s = e.status(self.platform, self.caps.manage_apps);
                match filter {
                    Filter::All => true,
                    Filter::Installed => s.is_installed(),
                    Filter::Updates => s == AppStatus::UpdateAvailable,
                    Filter::NotInstalled => !s.is_installed(),
                }
            })
            .count()
    }

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let t = Tokens::get(ctx);
        let anchor = egui::vec2(-20.0, -(crate::views::FOOTER_H + 12.0));
        egui::Area::new(egui::Id::new("toasts")).anchor(egui::Align2::RIGHT_BOTTOM, anchor).order(egui::Order::Foreground).show(ctx, |ui| {
            ui.set_max_width(380.0);
            let mut dismiss = None;
            for (i, toast) in self.toasts.iter().enumerate() {
                let accent = if toast.error { t.bad } else { t.good };
                let r = egui::Frame::new()
                    .fill(t.bg)
                    .stroke(egui::Stroke::new(1.0, t.line_strong))
                    .shadow(theme::shadow(&t))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, accent);
                            ui.add(egui::Label::new(egui::RichText::new(&toast.text).color(t.ink)).wrap());
                        });
                    });
                if r.response.interact(egui::Sense::click()).clicked() {
                    dismiss = Some(i);
                }
                ui.add_space(6.0);
            }
            if let Some(i) = dismiss {
                self.toasts.remove(i);
            }
        });
    }
}
