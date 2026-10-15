//! [`LauncherUi`]: state, event handling and the page layout.

use std::time::Duration;

use artcraft_launcher_core::protocol::{
    Backend, BackendOptions, CANCELLED, Capabilities, Command, Event, INSTALL_CANCELLED, JobPhase, LatestRelease, PAUSED, Source,
};
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::status::AppStatus;
use artcraft_launcher_core::{Platform, SUITE};

use crate::i18n::set_current;
use crate::model::AppEntry;
use crate::motion;
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
    /// The OS asks for fewer animations (Windows' "Animation effects" switched off, macOS'
    /// "Reduce motion"). Asked about once a second while the window draws, so it must be quick.
    fn reduce_motion(&self) -> bool {
        false
    }
    /// Asks the user for a folder with the system's own folder picker, starting in `start`.
    /// `None` when they cancel or there is no picker here.
    fn pick_folder(&mut self, _title: &str, _start: Option<&str>) -> Option<String> {
        None
    }
    /// Whether [`Host::pick_folder`] can show a picker.
    fn can_pick_folder(&self) -> bool {
        false
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
    LauncherUpdate(artcraft_launcher_core::Release),
}

impl Dialog {
    /// What sets this dialog apart from the others, for egui's memory: each one keeps its own
    /// size there, so a small prompt never opens at the height of the last dialog shown (#88).
    pub(crate) fn key(&self) -> (&'static str, &str) {
        match self {
            Dialog::Notes(id) => ("notes", id),
            Dialog::ConfirmUninstall(id) => ("uninstall", id),
            Dialog::Settings => ("settings", ""),
            Dialog::LauncherUpdate(r) => ("launcher-update", &r.tag),
        }
    }
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
    /// News from ArtCraft's release feed, shown inline until dismissed.
    pub(crate) announcements: Vec<artcraft_launcher_core::feed::Notice>,
    /// Alerts from the feed, shown as popups until acknowledged.
    pub(crate) alerts: Vec<artcraft_launcher_core::feed::Notice>,
    pub(crate) search: String,
    pub(crate) toasts: Vec<Toast>,
    pub(crate) dialog: Option<Dialog>,
    /// The app whose details drawer is open (or closing: see `detail_shown`).
    pub(crate) detail: Option<String>,
    /// The app the drawer is drawn for: it outlives `detail` while the drawer slides out.
    pub(crate) detail_shown: Option<String>,
    /// Which screenshot the drawer shows.
    pub(crate) detail_shot: usize,
    /// The screenshot is enlarged over the whole window.
    pub(crate) detail_zoom: bool,
    /// The details pictures the backend has sent (or been asked for), by app and picture key.
    pub(crate) pictures: std::collections::HashMap<(String, String), crate::details::Picture>,
    started: bool,
    applied_theme: Option<theme::ThemeChoice>,
    /// The OS asks for fewer animations: what the host last said, and the context's time to ask
    /// it again at.
    pub(crate) reduce_motion: bool,
    motion_recheck: f64,
    /// What the context's styles were last set for.
    applied_motion: Option<bool>,
    /// Fonts registered with `set_fonts` only exist from the next frame on; drawing with the named
    /// weights before then would abort.
    fonts_ready: bool,
    /// The CJK language whose system font is installed, if any.
    /// Set when such a language comes into use, then kept for the session.
    pub(crate) cjk_font: Option<crate::i18n::Lang>,
    sent_options: Option<BackendOptions>,
    /// Unix seconds of the last look at what's installed (startup counts).
    last_detect: i64,
    /// Whether the window had focus last frame, to re-check when the user comes back to it.
    was_focused: bool,
    /// Versions of apps or the launcher already queued for auto-update during this session,
    /// preventing repeated install loops if an update fails or is in progress.
    pub(crate) auto_update_attempted: std::collections::BTreeSet<(String, String)>,
}

/// The language to draw in: the preference, or English where it needs a CJK font the system
/// doesn't have (Settings says so), rather than boxes for every character. The only place the
/// CJK font gets loaded: when such a language is selected, or Auto resolves to it.
pub(crate) fn effective_lang(pref: &str) -> crate::i18n::Lang {
    let lang = crate::i18n::Lang::from_pref(pref);
    if lang.needs_cjk_font() && !theme::cjk_font_available(lang) { crate::i18n::Lang::EN } else { lang }
}

/// How often to look again at what's installed, to notice apps installed or removed outside
/// the launcher. A detection pass lists packages and reads a few files: cheap, but not free.
const RESCAN_EVERY: i64 = 5 * 60;
/// Coming back to the window re-checks, but not more often than this.
const RESCAN_ON_FOCUS_AFTER: i64 = 30;

impl LauncherUi {
    pub fn new(ctx: &egui::Context, backend: Box<dyn Backend>, settings: Settings) -> Self {
        let lang = effective_lang(&settings.language);
        let cjk_font = lang.needs_cjk_font().then_some(lang);
        theme::install_fonts(ctx, cjk_font);
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
            announcements: Vec::new(),
            alerts: Vec::new(),
            search: String::new(),
            toasts: Vec::new(),
            dialog: None,
            detail: None,
            detail_shown: None,
            detail_shot: 0,
            detail_zoom: false,
            pictures: std::collections::HashMap::new(),
            started: false,
            applied_theme: None,
            reduce_motion: false,
            motion_recheck: 0.0,
            applied_motion: None,
            fonts_ready: false,
            cjk_font,
            sent_options: None,
            last_detect: 0,
            was_focused: true,
            auto_update_attempted: std::collections::BTreeSet::new(),
        }
    }

    /// Moves the UI to another window's context: fonts, image loaders, the theme and the animation
    /// timings belong to a context, and toasts and the next look at the OS's animation setting are
    /// timed on its clock.
    pub fn attach(&mut self, ctx: &egui::Context) {
        theme::install_fonts(ctx, self.cjk_font);
        egui_extras::install_image_loaders(ctx);
        self.fonts_ready = false;
        self.applied_theme = None;
        self.applied_motion = None;
        self.motion_recheck = 0.0;
        self.toasts.clear();
    }

    /// Input on its way to egui (the shell's `raw_input_hook`): with fewer animations asked for,
    /// a mouse wheel scrolls at once instead of gliding.
    pub fn raw_input(&self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if self.reduce_motion {
            motion::unsmooth_wheel(ctx, raw);
        }
    }

    /// Installed apps with an update waiting (the tray shows the count). Hidden apps are left out,
    /// so the badge matches the Updates list.
    pub fn update_count(&self) -> usize {
        self.entries.iter().filter(|e| self.in_filter(e, Filter::Updates)).count()
    }

    /// Something is downloading or installing, the launcher's own update included: quitting now
    /// would cut it short.
    pub fn busy(&self) -> bool {
        self.launcher_job.is_some() || self.entries.iter().any(|e| e.job.is_some())
    }

    /// Looks for updates now (the tray's "Check for Updates").
    pub fn check_now(&mut self) {
        self.auto_update_attempted.clear();
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
        set_current(crate::i18n::Lang::from_pref(&self.settings.language));
        let options = BackendOptions {
            kinds: self.settings.preferred_kinds.clone(),
            prereleases: self.settings.prereleases,
            apps_folder: self.settings.apps_folder(),
        };
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
            if self.settings.auto_update {
                self.trigger_auto_updates();
            }
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
            Event::Latest { app, latest, from } => {
                if let Some(e) = self.entry_mut(&app) {
                    // A failed check keeps the release we already know about (and where it came from).
                    if !(matches!(latest, LatestRelease::Failed(_)) && matches!(e.latest, Some(LatestRelease::Found(_)))) {
                        e.latest = Some(latest);
                        e.origin = Some(from);
                    }
                }
                true
            }
            Event::Screenshot { app, key, result } => {
                self.pictures.insert((app, key), crate::details::Picture::from_result(result));
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
                    if matches!(self.dialog, Some(Dialog::LauncherUpdate(_))) {
                        self.dialog = None;
                    }
                } else if let Some(e) = self.entry_mut(&app) {
                    e.job = Some(phase);
                }
                false
            }
            Event::LauncherRestarting => {
                self.restart_requested = true;
                false
            }
            Event::Notices { announcements, alerts } => {
                self.announcements = announcements;
                self.alerts = alerts;
                false
            }
            Event::Paused { app, download } => {
                if let Some(e) = self.entry_mut(&app) {
                    e.paused = download;
                }
                false
            }
            Event::ReadyToInstall { app, ready } => {
                if let Some(e) = self.entry_mut(&app) {
                    e.ready = ready;
                }
                false
            }
            Event::JobFinished { app, result } => {
                let launcher = app == artcraft_launcher_core::catalog::LAUNCHER_ID;
                // What the job was doing when it ended says what a cancellation called off.
                let last = if launcher { self.launcher_job.take() } else { self.entry(&app).and_then(|e| e.job) };
                if let Some(e) = self.entry_mut(&app) {
                    e.job = None;
                }
                let kept = self.entry(&app).is_some_and(|e| e.ready);
                match result {
                    Ok(msg) => self.toast(ctx, msg, false),
                    // The paused card says it all.
                    Err(e) if e == PAUSED => {}
                    Err(e) if e == CANCELLED && launcher => self.toast(ctx, tl!("Update cancelled"), false),
                    // An install's own cancellations end as INSTALL_CANCELLED, so past the
                    // download this is a removal called off while it waited for the installer.
                    Err(e) if e == CANCELLED && matches!(last, Some(JobPhase::Waiting | JobPhase::Removing)) => {
                        self.toast(ctx, tl!("Uninstall cancelled"), false);
                    }
                    Err(e) if e == CANCELLED => self.toast(ctx, tl!("Download cancelled"), false),
                    Err(e) if e == INSTALL_CANCELLED && kept => {
                        self.toast(ctx, tl!("Install cancelled. The download is kept for the next try."), false);
                    }
                    Err(e) if e == INSTALL_CANCELLED => self.toast(ctx, tl!("Install cancelled"), false),
                    Err(e) => self.toast(ctx, e, true),
                }
                true
            }
            Event::LauncherUpdate(r) => {
                let fresh = self.launcher_update.as_ref().is_none_or(|cur| cur.version != r.version);
                self.launcher_update = Some(r.clone());
                // Ask once per new version, unless automatic updates will install it anyway.
                let automatic = self.settings.auto_update && self.caps.self_update;
                if fresh && !automatic && self.dialog.is_none() && self.launcher_job.is_none() {
                    self.dialog = Some(Dialog::LauncherUpdate(r));
                }
                fresh
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
            if !self.in_filter(e, Filter::Updates) {
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
                [(_, name, v)] => {
                    let body: String = tl!("Open ArtCraft Launcher to update.").to_string();
                    (crate::i18n::fmt(tl!("{name} {v} is available"), &[("name", name), ("v", v)]), body)
                }
                many => {
                    let body: String = many.iter().map(|(_, n, v)| format!("{n} {v}")).collect::<Vec<_>>().join(", ");
                    (crate::i18n::fmt(tl!("{count} updates are available"), &[("count", &many.len().to_string())]), body)
                }
            };
            host.notify(&title, &body);
        }
        for (id, _, version) in fresh {
            self.settings.notified.insert(id.to_string(), version);
        }
    }

    /// When auto-updates are enabled, automatically downloads and installs pending updates.
    pub(crate) fn trigger_auto_updates(&mut self) {
        if !self.caps.manage_apps || !self.settings.auto_update {
            return;
        }
        let to_update: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|e| self.in_filter(e, Filter::Updates) && e.job.is_none() && !e.ready)
            .filter_map(|e| {
                let v = e.latest_label()?;
                if self.auto_update_attempted.contains(&(e.app.id.to_string(), v.clone())) { None } else { Some((e.app.id.to_string(), v)) }
            })
            .collect();
        for (id, version) in to_update {
            log::info!("auto-update: queuing update for {id} to {version}");
            self.auto_update_attempted.insert((id.clone(), version));
            self.send(Command::Install { app: id });
        }
        if self.caps.self_update
            && let Some(r) = &self.launcher_update
            && self.launcher_job.is_none()
        {
            let id = artcraft_launcher_core::catalog::LAUNCHER_ID.to_string();
            let v = r.version_label();
            if !self.auto_update_attempted.contains(&(id.clone(), v.clone())) {
                log::info!("auto-update: queuing launcher self-update to {v}");
                self.auto_update_attempted.insert((id, v));
                self.send(Command::UpdateLauncher);
            }
        }
    }

    /// Draws the launcher.
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) {
        let ctx = ui.ctx().clone();
        // Set the current language for this frame based on settings.
        let lang = effective_lang(&self.settings.language);
        set_current(lang);
        // `effective_lang` only picks a CJK language once its font has loaded.
        if lang.needs_cjk_font() && self.cjk_font != Some(lang) {
            theme::install_fonts(&ctx, Some(lang));
            self.cjk_font = Some(lang);
            self.fonts_ready = false;
        }
        if !self.fonts_ready {
            self.fonts_ready = true;
            ctx.request_repaint();
            return;
        }
        if self.applied_theme != Some(self.settings.theme) {
            theme::apply(&ctx, self.settings.theme);
            self.applied_theme = Some(self.settings.theme);
        }
        let now = ctx.input(|i| i.time);
        if now >= self.motion_recheck {
            self.reduce_motion = host.reduce_motion();
            self.motion_recheck = now + motion::RECHECK_EVERY;
        }
        if self.applied_motion != Some(self.reduce_motion) {
            motion::apply(&ctx, self.reduce_motion);
            self.applied_motion = Some(self.reduce_motion);
        }
        let tokens = Tokens::get(&ctx);
        if self.entries.iter().any(|e| e.job.is_some()) || self.checking {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // The bars' separator lines are the hairlines under the header and over the footer.
        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(tokens.bg))
            .exact_size(crate::views::HEADER_H)
            .resizable(false)
            .show(ui, |ui| self.header(ui, host.title_bar()));
        egui::Panel::bottom("footer")
            .frame(egui::Frame::new().fill(tokens.bg).inner_margin(egui::Margin::symmetric(20, 0)))
            .exact_size(crate::views::FOOTER_H)
            .resizable(false)
            .show(ui, |ui| self.footer(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(tokens.bg).inner_margin(egui::Margin { left: 20, right: 20, top: 18, bottom: 0 }))
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
                            crate::widgets::hud(ui, tl!("No apps match"), tokens.muted);
                        });
                    } else {
                        match self.settings.view {
                            ViewMode::Grid => self.grid(ui, &ids),
                            ViewMode::Table => self.table(ui, &ids),
                        }
                    }
                    ui.add_space(16.0);
                });
            });
        self.details_drawer(&ctx);
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
                self.in_filter(e, self.settings.filter)
                    && (needle.is_empty() || e.app.name.to_lowercase().contains(&needle) || e.app.tagline.to_lowercase().contains(&needle))
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

    /// "ArtCraft API", "GitHub" or "ArtCraft API and GitHub": the services behind the releases on
    /// screen right now. `None` until something has loaded.
    pub(crate) fn sources_label(&self) -> Option<String> {
        use artcraft_launcher_core::protocol::Origin;
        let mut api = false;
        let mut github = false;
        for e in &self.entries {
            if matches!(e.latest, Some(LatestRelease::Found(_) | LatestRelease::None)) {
                match e.origin {
                    Some(Origin::ArtCraftApi) => api = true,
                    Some(Origin::GitHub) => github = true,
                    None => {}
                }
            }
        }
        match (api, github) {
            (true, true) => Some(crate::i18n::fmt(tl!("{a} and {b}"), &[("a", Origin::ArtCraftApi.label()), ("b", Origin::GitHub.label())])),
            (true, false) => Some(Origin::ArtCraftApi.label().to_string()),
            (false, true) => Some(Origin::GitHub.label().to_string()),
            (false, false) => None,
        }
    }

    pub(crate) fn count(&self, filter: Filter) -> usize {
        self.entries.iter().filter(|e| self.in_filter(e, filter)).count()
    }

    /// Whether `entry` belongs in `filter`. Hidden apps stay out of every list except Hidden.
    pub(crate) fn in_filter(&self, entry: &AppEntry, filter: Filter) -> bool {
        let hidden = self.settings.hidden.contains(entry.app.id);
        let status = entry.status(self.platform, self.caps.manage_apps);
        match filter {
            Filter::Hidden => hidden,
            Filter::All => !hidden,
            Filter::Installed => !hidden && status.is_installed(),
            Filter::Updates => !hidden && status == AppStatus::UpdateAvailable,
            Filter::NotInstalled => !hidden && !status.is_installed(),
        }
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
                            ui.add(egui::Label::new(egui::RichText::new(crate::i18n::msg(&toast.text)).color(t.ink)).wrap());
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
