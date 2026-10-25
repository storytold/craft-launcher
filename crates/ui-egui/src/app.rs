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
    fn pick_folder(&mut self, _initial: Option<&str>) -> Option<String> {
        None
    }
    /// The URL to open when Discord is clicked. Defaults to the web link.
    fn discord_url(&self) -> &'static str {
        artcraft_launcher_core::catalog::DISCORD_URL
    }
    /// Quit the launcher for real (not just hide to the tray).
    fn quit(&mut self) {}
}

pub(crate) struct Toast {
    pub text: String,
    pub error: bool,
    pub until: f64,
}

/// One user action collected while a UI pass draws, sent once when the pass finishes.
///
/// egui 0.37 may run `App::ui` (and therefore `LauncherUi::ui`) several times for one visible
/// frame after `request_discard`. Keeping the side effect out of the widget handler and comparing
/// it with what this frame already sent makes the same click idempotent across those passes, while
/// a new click in the next frame is still delivered.
///
/// The payload is not a widget identity: two different controls in one pass may legitimately ask
/// for the same command (for example the update button on a card and the launcher-update banner).
/// The queue is drained in draw order and equal actions are counted, so a repeated pass can only
/// repeat the actions it already produced, never collapse two distinct intents into one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UiAction {
    Command(Command),
    /// [`Command::Launch`] plus one launch-count increment.
    Launch(String),
    /// Re-run the manual update check, clearing the auto-update back-off.
    CheckNow,
    /// A message a UI interaction asked to show.
    Toast {
        text: String,
        error: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dialog {
    Notes(String),
    ConfirmUninstall(String),
    Install { app: String, folder: String },
    Replace { app: String, path: String },
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
            Dialog::Install { app, .. } => ("install", app),
            Dialog::Replace { app, .. } => ("replace", app),
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
    /// Release-note text scale for this session, independent of the main UI zoom.
    pub(crate) notes_zoom: f32,
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
    /// The URL or deep link to open when Discord is clicked.
    pub(crate) discord_url: &'static str,
    /// UI actions drawn in this pass, not sent yet.
    pending: Vec<UiAction>,
    /// The `egui::Context::cumulative_frame_nr` those actions belong to.
    frame: Option<u64>,
    /// UI actions this visible frame has already sent, so a discarded pass cannot repeat them.
    flushed: Vec<UiAction>,
    /// Toasts created from logic (which may run with an old input time) or the UI. Their expiry is
    /// fixed when the next `ui` pass actually shows them.
    pending_toasts: Vec<(String, bool)>,
}

/// The language to draw in: the preference, or English where it needs a CJK font the system
/// doesn't have (Settings says so), rather than boxes for every character. The only place the
/// CJK font gets loaded: when such a language is selected, or Auto resolves to it.
pub(crate) fn effective_lang(pref: &str) -> crate::i18n::Lang {
    let lang = crate::i18n::Lang::from_pref(pref);
    if lang.needs_cjk_font() && !theme::cjk_font_available(lang) { crate::i18n::Lang::EN } else { lang }
}

/// The language this frame may draw: a CJK preference only once its own face is installed in the
/// context, never merely because the system has one. Callers draw with the result, so a language
/// whose font becomes active only next pass cannot ask this pass for a missing glyph.
pub(crate) fn drawable_lang(pref: &str, installed: Option<crate::i18n::Lang>) -> crate::i18n::Lang {
    let lang = crate::i18n::Lang::from_pref(pref);
    if lang.needs_cjk_font() && installed != Some(lang) { crate::i18n::Lang::EN } else { lang }
}

/// How often to look again at what's installed, to notice apps installed or removed outside
/// the launcher. A detection pass lists packages and reads a few files: cheap, but not free.
const RESCAN_EVERY: i64 = 5 * 60;
/// Coming back to the window re-checks, but not more often than this.
const RESCAN_ON_FOCUS_AFTER: i64 = 30;

impl LauncherUi {
    /// Builds the UI shell around `backend`: installs the bundled fonts and an available system
    /// CJK face when the effective language needs it, plus image loaders; snapshots capabilities and
    /// platform, and migrates the settings in memory. It does not save the migrated settings or
    /// send `Configure`; [`Self::logic`] sends the configuration and the shell owns persistence.
    /// `fonts_ready` starts false, so the first `ui` pass is a gate that draws nothing and flushes
    /// any action queued before it.
    pub fn new(ctx: &egui::Context, backend: Box<dyn Backend>, settings: Settings) -> Self {
        let lang = effective_lang(&settings.language);
        let cjk_font = lang.needs_cjk_font().then_some(lang);
        theme::install_fonts(ctx, cjk_font);
        egui_extras::install_image_loaders(ctx);
        let caps = backend.capabilities();
        let platform = backend.platform();
        let mut settings = settings;
        settings.migrate();
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
            notes_zoom: 1.0,
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
            discord_url: artcraft_launcher_core::catalog::DISCORD_URL,
            pending: Vec::new(),
            frame: None,
            flushed: Vec::new(),
            pending_toasts: Vec::new(),
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
        // Actions belong to the old context's frame; messages waiting to be shown do not.
        self.pending.clear();
        self.frame = None;
        self.flushed.clear();
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

    /// The toolbar's check button. The side effects run once, when the pass flushes.
    pub(crate) fn check_now_from_ui(&mut self) {
        self.act(UiAction::CheckNow);
    }

    /// Queues one user action. Distinct controls may queue equal payloads in the same pass; the
    /// per-frame duplicate check happens in [`Self::flush_actions`] when the draw order is known.
    fn act(&mut self, action: UiAction) {
        self.pending.push(action);
    }

    /// Queues a backend command at this pass's action boundary. [`Self::flush_actions`] delivers it
    /// once per visible frame; equal commands queued by distinct controls in one pass are separate
    /// intents and are both kept.
    pub(crate) fn act_command(&mut self, command: Command) {
        self.act(UiAction::Command(command));
    }

    /// Starts a visible frame: only a completed `run_ui` advances the frame number, so all discarded
    /// passes of one frame share a `flushed` list.
    fn begin_ui_frame(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_frame_nr();
        if self.frame != Some(frame) {
            self.frame = Some(frame);
            self.flushed.clear();
        }
    }

    /// Sends the actions collected by this pass, exactly once per visible frame.
    ///
    /// Actions are compared by their `rank` among equal actions in this pass: the first Install in
    /// a pass is the first Install that frame, the second is the second. A re-run pass produces the
    /// same ranks in the same draw order, so anything at or below the number already sent is a
    /// repeat; a genuinely new second intent of the same kind still gets its own rank and is sent.
    fn flush_actions(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for (index, action) in pending.iter().enumerate() {
            let rank = pending[..index].iter().filter(|earlier| *earlier == action).count();
            let already_sent = self.flushed.iter().filter(|sent| *sent == action).count();
            if rank < already_sent {
                // This action belongs to an earlier pass of the same visible frame.
                continue;
            }
            match action {
                UiAction::Command(command) => self.backend.send(command.clone()),
                UiAction::Launch(id) => {
                    let opened = self.settings.launches.entry(id.clone()).or_insert(0);
                    *opened = opened.saturating_add(1);
                    self.backend.send(Command::Launch { app: id.clone() });
                }
                UiAction::CheckNow => {
                    self.auto_update_attempted.clear();
                    self.backend.send(Command::CheckForUpdates { force: true });
                    self.backend.send(Command::Detect);
                    self.last_detect = self.backend.now();
                }
                UiAction::Toast { text, error } => self.queue_toast(text.clone(), *error),
            }
            self.flushed.push(action.clone());
        }
    }

    /// Queues a message for the next pass that can actually draw. Its age is deliberately not
    /// part of the queue: a job that finished while the window was hidden is still news when the
    /// window first opens, so the countdown starts on display (see [`Self::draw_toasts`]).
    fn queue_toast(&mut self, text: String, error: bool) {
        self.pending_toasts.push((text, error));
        if self.pending_toasts.len() > 4 {
            self.pending_toasts.remove(0);
        }
    }

    /// Queues a programmatic message made outside the UI action boundary (logic, tray work). It is
    /// shown by the next pass that can draw, and its expiry starts then; a UI interaction must use
    /// [`Self::toast_from_ui`] so a discarded pass cannot queue it twice.
    pub(crate) fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.queue_toast(text.into(), error);
    }

    /// A toast caused by a UI interaction: guarded by the same per-frame action boundary as
    /// installs and launches, so a discarded pass cannot show it twice.
    pub(crate) fn toast_from_ui(&mut self, text: impl Into<String>, error: bool) {
        self.act(UiAction::Toast { text: text.into(), error });
    }

    pub(crate) fn entry(&self, id: &str) -> Option<&AppEntry> {
        self.entries.iter().find(|e| e.app.id == id)
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut AppEntry> {
        self.entries.iter_mut().find(|e| e.app.id == id)
    }

    pub(crate) fn configure(&mut self) {
        let options = BackendOptions {
            kinds: self.settings.preferred_kinds.clone(),
            prereleases: self.settings.prereleases,
            install_locations: self.settings.install_locations.clone(),
        };
        if self.sent_options.as_ref() != Some(&options) {
            self.backend.send(Command::Configure(options.clone()));
            self.sent_options = Some(options);
        }
    }

    pub(crate) fn custom_install_folders(&self) -> bool {
        self.caps.manage_apps
            && self.platform.is_some_and(|p| {
                matches!(p.os, artcraft_launcher_core::Os::Windows | artcraft_launcher_core::Os::MacOs | artcraft_launcher_core::Os::Linux)
            })
            && !crate::settings::is_flatpak_sandbox()
    }

    /// Asks to install `id`. On a host that supports custom folders and for an app that is not
    /// installed, it opens the folder dialog and sends nothing until that is confirmed; otherwise
    /// it queues `Install` immediately, including for an already installed app (update/reinstall).
    pub(crate) fn install_app(&mut self, id: &str) {
        if !self.custom_install_folders() || self.entry(id).is_some_and(|e| e.installed.is_some()) {
            self.act_command(Command::Install { app: id.to_string() });
            return;
        }
        self.dialog =
            Some(Dialog::Install { app: id.to_string(), folder: self.settings.install_locations.per_app.get(id).cloned().unwrap_or_default() });
    }

    /// Non-drawing work: runs every frame and also while the window is hidden in the tray.
    pub fn logic(&mut self, ctx: &egui::Context, host: &mut dyn Host) {
        // Messages made here (toasts, alerts) are drawn later, so use the language the UI can
        // actually show: English where the system has no CJK face for the preference.
        set_current(effective_lang(&self.settings.language));
        self.configure();
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
            changed |= self.handle(event);
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
    fn handle(&mut self, event: Event) -> bool {
        match event {
            Event::Latest { app, latest, from } => {
                self.on_latest(app, latest, from);
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
                self.on_progress(app, phase);
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
            Event::Occupied { app, path } => {
                self.dialog = Some(Dialog::Replace { app, path });
                false
            }
            Event::JobFinished { app, result } => {
                self.on_job_finished(app, result);
                true
            }
            Event::LauncherUpdate(r) => self.on_launcher_update(r),
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

    // A failed check keeps the release we already know about (and where it came from).
    fn on_latest(&mut self, app: String, latest: LatestRelease, from: artcraft_launcher_core::protocol::Origin) {
        if let Some(e) = self.entry_mut(&app)
            && !(matches!(latest, LatestRelease::Failed(_)) && matches!(e.latest, Some(LatestRelease::Found(_))))
        {
            e.latest = Some(latest);
            e.origin = Some(from);
        }
    }

    // Routes a job phase to the launcher's own slot or to the app card it belongs to.
    fn on_progress(&mut self, app: String, phase: JobPhase) {
        if app == artcraft_launcher_core::catalog::LAUNCHER_ID {
            self.launcher_job = Some(phase);
            if matches!(self.dialog, Some(Dialog::LauncherUpdate(_))) {
                self.dialog = None;
            }
        } else if let Some(e) = self.entry_mut(&app) {
            e.job = Some(phase);
        }
    }

    /// Summarizes a job's end as a toast; cancellations have their own messages.
    fn on_job_finished(&mut self, app: String, result: Result<String, String>) {
        let launcher = app == artcraft_launcher_core::catalog::LAUNCHER_ID;
        // What the job was doing when it ended says what a cancellation called off.
        let last = if launcher { self.launcher_job.take() } else { self.entry(&app).and_then(|e| e.job) };
        if let Some(e) = self.entry_mut(&app) {
            e.job = None;
        }
        let kept = self.entry(&app).is_some_and(|e| e.ready);
        match result {
            Ok(msg) => self.toast(msg, false),
            // The paused card says it all.
            Err(e) if e == PAUSED => {}
            Err(e) if e == CANCELLED && launcher => self.toast(tl!("Update cancelled"), false),
            // An install's own cancellations end as INSTALL_CANCELLED, so past the
            // download this is a removal called off while it waited for the installer.
            Err(e) if e == CANCELLED && matches!(last, Some(JobPhase::Waiting | JobPhase::Removing)) => {
                self.toast(tl!("Uninstall cancelled"), false);
            }
            Err(e) if e == CANCELLED => self.toast(tl!("Download cancelled"), false),
            Err(e) if e == INSTALL_CANCELLED && kept => {
                self.toast(tl!("Install cancelled. The download is kept for the next try."), false);
            }
            Err(e) if e == INSTALL_CANCELLED => self.toast(tl!("Install cancelled"), false),
            // The Replace dialog says it all.
            Err(_) if matches!(&self.dialog, Some(Dialog::Replace { app: a, .. }) if *a == app) => {}
            Err(e) => self.toast(e, true),
        }
    }

    /// Offers the launcher's own update once per new version, unless automatic updates install it.
    fn on_launcher_update(&mut self, r: Release) -> bool {
        // Whether the launcher can replace itself is only known once an update is found.
        self.caps = self.backend.capabilities();
        let fresh = self.launcher_update.as_ref().is_none_or(|cur| cur.version != r.version);
        self.launcher_update = Some(r.clone());
        let automatic = self.settings.auto_update && self.caps.self_update;
        if fresh && !automatic && self.dialog.is_none() && self.launcher_job.is_none() {
            self.dialog = Some(Dialog::LauncherUpdate(r));
        }
        fresh
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

    /// The user closed the window and it went away in the tray. The first time, a notification
    /// says so, with where to find it and how to quit: a window that vanishes with nothing to show
    /// for it looks hung (#45, #215).
    pub fn hidden_to_tray(&mut self, host: &mut dyn Host) {
        if self.settings.tray_hint_shown {
            return;
        }
        self.settings.tray_hint_shown = true;
        host.notify(
            tl!("ArtCraft Launcher is still running"),
            tl!("It's in the tray: open it or quit it from the tray icon, or turn this off in the settings."),
        );
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

    fn zoom_input(&mut self, ctx: &egui::Context) {
        use egui::gui_zoom::kb_shortcuts;
        let notes_open = matches!(self.dialog, Some(Dialog::Notes(_)));
        let (delta, zoom_in, zoom_out, reset) = ctx.input_mut(|i| {
            let delta = i.zoom_delta();
            let reset = notes_open && i.consume_shortcut(&kb_shortcuts::ZOOM_RESET);
            let zoom_in = notes_open && (i.consume_shortcut(&kb_shortcuts::ZOOM_IN) || i.consume_shortcut(&kb_shortcuts::ZOOM_IN_SECONDARY));
            let zoom_out = notes_open && i.consume_shortcut(&kb_shortcuts::ZOOM_OUT);
            (delta, zoom_in, zoom_out, reset)
        });
        let delta = if delta.is_finite() && delta > 0.0 { delta } else { 1.0 };
        if notes_open {
            let step = match (zoom_in, zoom_out) {
                (true, false) => 0.1,
                (false, true) => -0.1,
                _ => 0.0,
            };
            self.notes_zoom = if reset { 1.0 } else { ((self.notes_zoom + step) * delta).clamp(0.5, 3.0) };
        } else if delta != 1.0 {
            ctx.set_zoom_factor((ctx.zoom_factor() * delta).clamp(0.2, 5.0));
        }
    }

    /// Draws the launcher.
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &mut dyn Host) {
        let ctx = ui.ctx().clone();
        self.begin_ui_frame(&ctx);
        if host.title_bar() == TitleBar::MacOs && ctx.input_mut(|i| i.consume_key(egui::Modifiers::MAC_CMD, egui::Key::M)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        // `effective_lang` only picks a CJK language once the system's font has loaded; install
        // that face now, and draw nothing until the context has switched to it.
        let lang = effective_lang(&self.settings.language);
        if lang.needs_cjk_font() && self.cjk_font != Some(lang) {
            theme::install_fonts(&ctx, Some(lang));
            self.cjk_font = Some(lang);
            self.fonts_ready = false;
        }
        if !self.fonts_ready {
            self.fonts_ready = true;
            ctx.request_repaint();
            // A click cannot have produced an action on this pass (nothing was drawn), but a queued
            // action from an earlier pass must never be lost to the early return.
            self.flush_actions();
            return;
        }
        // Set the current language only after the font gate: a face installed by `set_fonts`
        // becomes active next pass, so drawing with it in the pass that installed it would panic.
        set_current(drawable_lang(&self.settings.language, self.cjk_font));
        self.zoom_input(&ctx);
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
        self.discord_url = host.discord_url();
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
        self.flush_actions();
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
        // The grid offers its own few orders (`views::grid_sort_now`); the table sorts by its columns.
        let grid = self.settings.view == ViewMode::Grid;
        let (sort, desc) = if grid { crate::views::grid_sort_now(&self.settings) } else { (self.settings.sort, self.settings.sort_descending) };
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
            let ord = match sort {
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
                SortKey::Frequent => {
                    let opened = |e: &AppEntry| self.settings.launches.get(e.app.id).copied().unwrap_or(0);
                    dir(opened(a).cmp(&opened(b)))
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
        // The grid shows installed apps first, then the rest under "Not installed" (the sort holds
        // within each group; the sort is stable). The table keeps the order its column gives.
        if grid {
            list.sort_by_key(|(_, e)| !e.status(platform, manage).is_installed());
        }
        list.into_iter().map(|(_, e)| e.app.id).collect()
    }

    /// Opens an app, and counts it for "Frequently used".
    pub(crate) fn launch_app(&mut self, id: &str) {
        self.act(UiAction::Launch(id.to_string()));
    }

    /// Whether the app on screen under `id` is installed on this computer.
    pub(crate) fn is_installed(&self, id: &str) -> bool {
        self.entry(id).is_some_and(|e| e.status(self.platform, self.caps.manage_apps).is_installed())
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

    /// Draws the toasts and dismisses a clicked one. Drains messages queued since the last drawing
    /// pass, starting each expiry on this pass, keeps at most four (dropping the oldest), removes
    /// expired ones, and asks for another frame while any remain; call it once per visible frame.
    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        // Logic may have queued these while no UI ran, with an old input time. They start their
        // countdown only now, on the first pass that can actually show them.
        for (text, error) in std::mem::take(&mut self.pending_toasts) {
            self.toasts.push(Toast { text, error, until: now + if error { 8.0 } else { 4.5 } });
            if self.toasts.len() > 4 {
                self.toasts.remove(0);
            }
        }
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let t = Tokens::get(ctx);
        let anchor = egui::vec2(-20.0, -(crate::views::FOOTER_H + 12.0));
        egui::Area::new(egui::Id::unique("toasts"))
            .role(egui::Role::Alert)
            .anchor(egui::Align2::RIGHT_BOTTOM, anchor)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui_kittest::Harness;

    use super::*;

    struct NoHost;
    impl Host for NoHost {}

    /// A backend that records commands and has no jobs, pictures or events.
    struct Recorder {
        sent: Rc<RefCell<Vec<Command>>>,
    }

    impl Backend for Recorder {
        /// Records the command so the test can assert what crossed the UI boundary.
        fn send(&mut self, command: Command) {
            self.sent.borrow_mut().push(command);
        }
        /// Always empty: the recorder has no events, jobs or pictures.
        fn poll(&mut self) -> Vec<Event> {
            Vec::new()
        }
        /// No install management, a neutral reveal label and no self-update, so UI paths needing them
        /// stay off.
        fn capabilities(&self) -> Capabilities {
            Capabilities { manage_apps: false, reveal_label: "Show", self_update: false }
        }
        /// Reports no platform, so the UI offers no OS-specific package format.
        fn platform(&self) -> Option<Platform> {
            None
        }
        /// A fixed zero clock; tests that need time drive their own backend.
        fn now(&self) -> i64 {
            0
        }
    }

    /// A UI and the context its fonts, theme and image loaders were installed into.
    fn launcher() -> (LauncherUi, Rc<RefCell<Vec<Command>>>, egui::Context) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let backend = Recorder { sent: sent.clone() };
        // A fixed language keeps `effective_lang` away from the machine's locale.
        let settings = Settings { language: "en".into(), notify: false, ..Settings::default() };
        let ctx = egui::Context::default();
        (LauncherUi::new(&ctx, Box::new(backend), settings), sent, ctx)
    }

    /// One `run_ui` pass at a chosen input time, exactly like the native shell's callback order.
    fn frame(launcher: &mut LauncherUi, ctx: &egui::Context, time: f64) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1180.0, 900.0))),
            time: Some(time),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| launcher.ui(ui, &mut NoHost));
        // This logic-only harness has no renderer to receive texture uploads.
        output.textures_delta.clear();
    }

    /// Counts the `Install` commands in `sent`, ignoring every other command.
    fn installs(sent: &RefCell<Vec<Command>>) -> usize {
        sent.borrow().iter().filter(|command| matches!(command, Command::Install { .. })).count()
    }

    /// A CJK preference is drawable only while that language's own face is installed: having the
    /// system font cached, or another script's face installed, is not enough.
    #[test]
    fn a_cjk_language_is_drawn_only_with_its_own_installed_face() {
        use crate::i18n::Lang;
        assert_eq!(drawable_lang("zh-CN", None), Lang::EN);
        assert_eq!(drawable_lang("zh-CN", Some(Lang::ZH_CN)), Lang::ZH_CN);
        assert_eq!(drawable_lang("zh-CN", Some(Lang::JA)), Lang::EN);
        assert_eq!(drawable_lang("ja", Some(Lang::ZH_CN)), Lang::EN);
        assert_eq!(drawable_lang("ja", Some(Lang::JA)), Lang::JA);
        assert_eq!(drawable_lang("fr", Some(Lang::ZH_CN)), Lang::FR);
    }

    #[test]
    /// Two controls queueing the same command in one pass are two gestures and must both reach the
    /// backend.
    fn two_distinct_intents_with_the_same_command_are_both_delivered() {
        let (mut launcher, sent, _ctx) = launcher();
        launcher.frame = Some(1);
        // Two different controls in one pass can ask for the same command (the card's Update and
        // the banner's Update, say). Their payload is equal, but they are not one gesture.
        launcher.act_command(Command::Install { app: "photocraft".into() });
        launcher.act_command(Command::Install { app: "photocraft".into() });
        launcher.flush_actions();
        assert_eq!(installs(&sent), 2, "both intents must reach the backend: {:?}", sent.borrow());
    }

    #[test]
    /// A later pass within one visible frame may add genuinely different actions without losing the
    /// earlier one.
    fn distinct_actions_in_a_later_pass_are_both_delivered() {
        let (mut launcher, sent, _ctx) = launcher();
        launcher.frame = Some(1);
        launcher.act_command(Command::Install { app: "photocraft".into() });
        launcher.flush_actions();
        // A second pass that finds two genuinely different actions must not lose either.
        launcher.act_command(Command::Install { app: "filmcraft".into() });
        launcher.act_command(Command::Cancel { app: "lightcraft".into() });
        launcher.flush_actions();
        assert_eq!(
            *sent.borrow(),
            [
                Command::Install { app: "photocraft".into() },
                Command::Install { app: "filmcraft".into() },
                Command::Cancel { app: "lightcraft".into() },
            ]
        );
    }

    #[test]
    /// A discarded second pass over the same gesture sends nothing, while the next visible frame
    /// accepts it again.
    fn a_repeated_pass_sends_nothing_but_a_new_frame_sends_again() {
        let (mut launcher, sent, _ctx) = launcher();
        launcher.frame = Some(1);
        launcher.act_command(Command::Install { app: "photocraft".into() });
        launcher.flush_actions();
        // The discarded second pass redraws the same clicked widget.
        launcher.act_command(Command::Install { app: "photocraft".into() });
        launcher.flush_actions();
        assert_eq!(installs(&sent), 1, "a second pass over the same gesture must not repeat it: {:?}", sent.borrow());
        // The next visible frame is a new gesture.
        launcher.frame = Some(2);
        launcher.flushed.clear();
        launcher.act_command(Command::Install { app: "photocraft".into() });
        launcher.flush_actions();
        assert_eq!(installs(&sent), 2, "the next frame must accept the action again: {:?}", sent.borrow());
    }

    #[test]
    /// A launch increments the persisted counter and sends one `Launch` per visible frame, not per
    /// pass.
    fn a_launch_count_is_incremented_once_per_visible_frame() {
        let (mut launcher, sent, _ctx) = launcher();
        launcher.frame = Some(1);
        launcher.act(UiAction::Launch("photocraft".into()));
        launcher.flush_actions();
        launcher.act(UiAction::Launch("photocraft".into()));
        launcher.flush_actions();
        assert_eq!(launcher.settings.launches.get("photocraft"), Some(&1));
        let launches = sent.borrow().iter().filter(|command| matches!(command, Command::Launch { .. })).count();
        assert_eq!(launches, 1, "one gesture, one launch command: {:?}", sent.borrow());
    }

    #[test]
    /// The font-gate pass flushes an action queued before it exactly once, and the following pass
    /// does not repeat it.
    fn a_pending_action_survives_the_fonts_ready_early_return_exactly_once() {
        let (mut launcher, sent, ctx) = launcher();
        // `new` installed the fonts but the first pass cannot draw with them yet.
        assert!(!launcher.fonts_ready);
        launcher.act_command(Command::Install { app: "photocraft".into() });
        frame(&mut launcher, &ctx, 0.0);
        assert_eq!(installs(&sent), 1, "the early return must flush what was already pending: {:?}", sent.borrow());
        frame(&mut launcher, &ctx, 0.25);
        assert_eq!(installs(&sent), 1, "the following pass must not repeat it: {:?}", sent.borrow());
    }

    #[test]
    /// A toast raised through the UI is queued once even when the same visible frame is drawn twice.
    fn a_ui_toast_is_shown_once_across_passes() {
        let (mut launcher, _sent, _ctx) = launcher();
        launcher.frame = Some(1);
        launcher.toast_from_ui("Finished", false);
        launcher.flush_actions();
        launcher.toast_from_ui("Finished", false);
        launcher.flush_actions();
        assert_eq!(launcher.pending_toasts.len(), 1, "a repeated pass must not queue the toast twice");
    }

    #[test]
    /// A toast made while no UI ran starts its expiry on the first pass that draws it, not at the
    /// older logic time.
    fn a_hidden_result_gets_its_expiry_from_the_pass_that_shows_it() {
        let (mut launcher, _sent, ctx) = launcher();
        // The fonts gate pass draws nothing and shows no toast.
        frame(&mut launcher, &ctx, 0.0);
        // A job finished while the window was hidden, when `logic`'s input time was old (0).
        launcher.toast("PhotoCraft updated", false);
        frame(&mut launcher, &ctx, 1000.0);
        let toast = launcher.toasts.first().expect("the first drawing pass shows the pending toast");
        assert_eq!(toast.until, 1004.5, "expiry must be relative to the drawing pass, not the old logic time");
        // Past the stale (0 + 4.5) deadline this would already be gone; it is still fresh at 1000.
        frame(&mut launcher, &ctx, 1004.6);
        assert!(launcher.toasts.is_empty(), "the toast goes at its real expiry");
    }

    #[test]
    /// Moving to a new window preserves a queued toast that has not yet been shown.
    fn attaching_a_window_keeps_messages_still_waiting_to_be_shown() {
        let (mut launcher, _sent, ctx) = launcher();
        launcher.toast("PhotoCraft updated", false);
        launcher.attach(&ctx);
        assert_eq!(launcher.pending_toasts.len(), 1, "a message from logic is not tied to the old window");
    }

    /// A UI driven through the real toolbar → Settings → language picker path, with the test-only
    /// font seam, so the selected frame and the single font-gate pass can be inspected exactly.
    fn cjk_picker_harness() -> Harness<'static, Option<LauncherUi>> {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut backend = Some(Recorder { sent });
        let settings = Settings { language: "en".into(), notify: false, ..Settings::default() };
        Harness::builder().with_size(egui::vec2(1180.0, 900.0)).with_theme(egui::Theme::Dark).build_ui_state(
            move |ui, state: &mut Option<LauncherUi>| {
                if state.is_none()
                    && let Some(backend) = backend.take()
                {
                    *state = Some(LauncherUi::new(ui.ctx(), Box::new(backend), settings.clone()));
                }
                if let Some(launcher) = state.as_mut() {
                    launcher.logic(&ui.ctx().clone(), &mut NoHost);
                    // The strict policy covers drawing passes only: translation is what asks the
                    // fonts for glyphs. Logic may set the current language ahead of the gate, but
                    // it draws nothing.
                    let _strict = crate::i18n::StrictCjkDrawing::on(launcher.cjk_font);
                    launcher.ui(ui, &mut NoHost);
                }
            },
            None,
        )
    }

    /// Opens Settings and its language combo through visible labels, leaving the picker open for a
    /// selection.
    fn open_language_picker(h: &mut Harness<'static, Option<LauncherUi>>) {
        use egui_kittest::kittest::Queryable;
        h.get_by_label("Settings").click();
        h.run_steps(3);
        h.get_by_value("English").click();
        h.run_steps(2);
    }

    /// Clicks Simplified Chinese and checks the very frame that records it: the frame must still
    /// draw English and must not have installed a CJK face yet.
    fn select_chinese_in_its_settings_frame(h: &mut Harness<'static, Option<LauncherUi>>) {
        use crate::i18n::Lang;
        use egui_kittest::kittest::Queryable;
        h.get_by_label("Simplified Chinese").click_accesskit();
        for _ in 0..4 {
            h.step();
            let launcher = h.state().as_ref().expect("harness state");
            if launcher.settings.language == "zh-CN" {
                assert_eq!(crate::i18n::current(), Lang::EN, "the selection frame must still draw English");
                assert_eq!(launcher.cjk_font, None, "the selection frame must not install a face yet");
                assert!(!crate::i18n::drew_cjk(), "the selection frame must not draw CJK text");
                return;
            }
        }
        panic!("the picker click must select Simplified Chinese");
    }

    /// Selecting Simplified Chinese must keep the selection frame itself in English: its face is
    /// installed at the next pass's font gate, and drawing the chosen language before then asks
    /// the bundled (CJK-free) fonts for Chinese glyphs. The old `effective_lang` selector sets the
    /// current language to Chinese inside this frame and fails the strict policy.
    #[test]
    fn selecting_chinese_keeps_the_selection_frame_english_until_the_font_gate() {
        use crate::i18n::Lang;
        use egui_kittest::kittest::Queryable;

        // Available: the system "has" a Chinese face. The seam's face is the real bundled Inter,
        // which has no CJK glyphs, so the gate can install a face without faking coverage.
        {
            let _fonts = crate::theme::CjkFontOverride::available(Lang::ZH_CN);
            let mut h = cjk_picker_harness();
            h.run_steps(8);
            open_language_picker(&mut h);
            select_chinese_in_its_settings_frame(&mut h);
            let marker = crate::theme::cjk_font(Lang::ZH_CN).expect("the seam answers with the bundled face");
            let inter: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");
            assert_eq!(marker.bytes(), inter, "the available marker must be the real Inter face, not a CJK face");

            // Exactly one gate pass: it installs the face and draws nothing. We stop here and do
            // not run the following pass, which would panic on missing Chinese glyphs in the Inter stand-in.
            h.step();
            {
                let launcher = h.state().as_ref().expect("harness state");
                assert_eq!(launcher.cjk_font, Some(Lang::ZH_CN), "the gate must install the face");
            }
            assert!(!crate::i18n::drew_cjk(), "the gate pass must not draw CJK text");
        }

        // Unavailable: no face at all, so every frame stays English and the gate installs nothing.
        {
            let _fonts = crate::theme::CjkFontOverride::unavailable(Lang::ZH_CN);
            let mut h = cjk_picker_harness();
            h.run_steps(8);
            open_language_picker(&mut h);
            select_chinese_in_its_settings_frame(&mut h);
            h.run_steps(3);
            {
                let launcher = h.state().as_ref().expect("harness state");
                assert_eq!(crate::i18n::current(), Lang::EN, "without a face, frames stay English");
                assert_eq!(launcher.cjk_font, None, "without a face, the gate installs nothing");
            }
            h.get_by_label_contains("No font for this language was found");
        }
    }

    /// The strict policy the regression relies on really rejects CJK text without its face.
    #[test]
    #[should_panic(expected = "strict CJK drawing policy")]
    fn strict_drawing_policy_rejects_cjk_text_without_its_installed_face() {
        use crate::i18n::Lang;
        let _guard = crate::i18n::StrictCjkDrawing::on(None);
        let _ = crate::i18n::tr(Lang::ZH_CN, "Settings");
    }
}
