//! Pre-release discovery and consent. The same backend contract owns all side effects.

use artcraft_launcher_core::Platform;
use artcraft_launcher_core::protocol::{Backend, BackendOptions, Command, Event, JobPhase, LatestRelease, NightlyStatus};
use egui::{RichText, Ui};
use std::collections::BTreeMap;

use crate::app::{LauncherUi, UiAction};
use crate::model::AppEntry;
use crate::settings::Settings;
use crate::theme::Tokens;

// Nightly tags encode a build calendar day, not a publication instant. Do not pass this
// through local_time: changing time zones must never change a build's identity.
fn nightly_build_date(text: &str) -> Option<(i64, u32, u32)> {
    let version = artcraft_launcher_core::version::parse(text)?;
    let parts: Vec<_> = version.pre.as_str().split('.').collect();
    let [channel, date, run, attempt, revision] = parts.as_slice() else { return None };
    if *channel != "nightly"
        || date.len() != 8
        || !date.bytes().all(|c| c.is_ascii_digit())
        || !run.parse::<u64>().is_ok_and(|n| n > 0)
        || !attempt.parse::<u64>().is_ok_and(|n| n > 0)
        || revision.len() != 13
        || !revision.starts_with('g')
        || !revision[1..].bytes().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    let year: i64 = date.get(..4)?.parse().ok()?;
    let month: u32 = date.get(4..6)?.parse().ok()?;
    let day: u32 = date.get(6..)?.parse().ok()?;
    if year == 0 {
        return None;
    }
    let at = artcraft_launcher_core::time::parse_rfc3339(&format!("{year:04}-{month:02}-{day:02}T00:00:00Z"))?;
    // The shared parser tolerates days beyond a month's length; reject their rollover.
    (artcraft_launcher_core::time::civil_from_unix(at) == (year, month, day)).then_some((year, month, day))
}

fn nightly_build_label(version: &str, language: crate::i18n::Lang) -> String {
    let nightly = crate::i18n::tr(language, "Nightly");
    let Some((year, month, day)) = nightly_build_date(version) else { return nightly.to_owned() };
    let date = crate::i18n::fmt(
        crate::i18n::tr(language, "{month}/{day}/{year}"),
        &[("month", &month.to_string()), ("day", &day.to_string()), ("year", &year.to_string())],
    );
    format!("{nightly} {date}")
}

fn nightly_card_version(entry: &AppEntry, enabled: bool) -> Option<String> {
    if enabled && let Some(LatestRelease::Found(release)) = &entry.latest {
        // The immutable tag is authoritative; a cached parsed field is only a convenience.
        return Some(artcraft_launcher_core::version::parse(&release.tag).map_or_else(|| release.tag.clone(), |version| version.to_string()));
    }
    // OFF is management-only: a cached newer release must not label the installed copy.
    entry.installed.as_ref()?.version.as_ref().map(ToString::to_string)
}

// Reuse the UI's relative-time vocabulary; build identity still uses the UTC tag day.
/// The card's caption and progress for a running Nightly job, in the same words as other jobs.
fn nightly_job_progress(phase: JobPhase) -> (Option<f32>, String) {
    match phase {
        JobPhase::Downloading { done, total } if total > 0 => (
            Some(done as f32 / total as f32),
            crate::fmt(
                tl!("Downloading {done} of {total}"),
                &[("done", &crate::widgets::human_bytes(done)), ("total", &crate::widgets::human_bytes(total))],
            ),
        ),
        JobPhase::Downloading { .. } | JobPhase::Reconnecting { .. } => (None, tl!("Downloading…").to_string()),
        JobPhase::Verifying => (None, tl!("Verifying…").to_string()),
        JobPhase::Queued => (None, tl!("Waiting for another download…").to_string()),
        JobPhase::Waiting => (None, tl!("Waiting for other installs…").to_string()),
        JobPhase::Installing => (None, tl!("Installing…").to_string()),
        JobPhase::Removing => (None, tl!("Removing…").to_string()),
    }
}

fn nightly_ago(at: i64, now: i64) -> String {
    use crate::i18n::tn;
    use artcraft_launcher_core::time::{TimeAgo, ago_kind};
    match ago_kind(at, now) {
        TimeAgo::JustNow => tl!("just now").into(),
        TimeAgo::Minutes(n) => tn(n, "{n} min ago", "{n} min ago"),
        TimeAgo::Hours(n) => tn(n, "{n} h ago", "{n} h ago"),
        TimeAgo::Yesterday => tl!("yesterday").into(),
        TimeAgo::Days(n) => tn(n, "{n} day ago", "{n} days ago"),
        TimeAgo::Date(at) => crate::views::date_text(at),
    }
}

fn nightly_status_labels(ui: &mut Ui, status: &NightlyStatus, now: i64) {
    use crate::i18n::fmt;
    if status.stale && status.verified_at.is_some() {
        ui.add(egui::Label::new(RichText::new(tl!("Previous verified result is stale. Check again before installing.")).strong()).wrap());
    }
    if let Some(at) = status.verified_at {
        ui.label(fmt(tl!("Last verified: {ago}"), &[("ago", &nightly_ago(at, now))]));
    }
    if let Some(at) = status.attempted_at {
        ui.label(fmt(tl!("Last attempt: {ago}"), &[("ago", &nightly_ago(at, now))]));
    }
    if let Some(message) = &status.message {
        ui.add(egui::Label::new(crate::i18n::message(crate::i18n::current(), message)).wrap());
    }
    if let Some(at) = status.retry_at.filter(|at| *at > now) {
        let minutes = at.saturating_sub(now).saturating_add(59) / 60;
        ui.add(egui::Label::new(fmt(tl!("Retry available in {minutes} min"), &[("minutes", &minutes.to_string())])).wrap());
    }
}

// Only a known installed version can make installation redundant. A blocked or
// failed lookup is not proof that the installed copy is current.
fn nightly_install_is_redundant(entry: &AppEntry) -> bool {
    let Some(installed) = entry.installed.as_ref().and_then(|copy| copy.version.as_ref()) else { return false };
    let Some(LatestRelease::Found(release)) = &entry.latest else { return false };
    // As on the card, compare the immutable tag, not a cached parsed version.
    let Some(latest) = artcraft_launcher_core::version::parse(&release.tag) else { return false };
    if nightly_build_date(&release.tag).is_none() || nightly_build_date(&installed.to_string()).is_none() {
        return false;
    }
    !artcraft_launcher_core::version::is_update(&latest, installed)
}

pub(crate) struct NightlyState {
    backend: Option<Box<dyn Backend>>,
    /// New builds can be discovered and installed here: the platform's signing trust is
    /// configured. Copies already installed are managed either way.
    available: bool,
    pub active: bool,
    pub consent_open: bool,
    consent_focus: bool,
    return_focus: Option<egui::Id>,
    return_settings: bool,
    focus_tab: bool,
    pub entries: Vec<AppEntry>,
    options: Option<BackendOptions>,
    checked_at: Option<i64>,
    attempted_at: Option<i64>,
    // Request throttling is not evidence that Native actually attempted a lookup.
    requested_at: Option<i64>,
    statuses: BTreeMap<String, NightlyStatus>,
    checking: bool,
    notices: Vec<String>,
    uninstall: Option<String>,
    uninstall_focus: bool,
}

impl NightlyState {
    pub fn new(backend: Option<Box<dyn Backend>>, platform: Option<Platform>) -> Self {
        // Without a nightly context the page already says installation is unavailable.
        let available = backend.as_ref().is_none_or(|backend| backend.nightly_available());
        Self {
            backend,
            available,
            active: false,
            consent_open: false,
            consent_focus: false,
            return_focus: None,
            return_settings: false,
            focus_tab: false,
            entries: artcraft_launcher_core::hosted_catalog::apps()
                .iter()
                .filter(|app| app.has_separate_nightly_channel() && platform.is_none_or(|p| app.runs_on(p.os)))
                .map(AppEntry::new)
                .collect(),
            options: None,
            checked_at: None,
            attempted_at: None,
            requested_at: None,
            statuses: BTreeMap::new(),
            checking: false,
            notices: Vec::new(),
            uninstall: None,
            uninstall_focus: false,
        }
    }

    // Initial discovery lives only in Settings. Retained installations keep a management
    // destination after switching off; hiding it would strand Open/Reveal/Uninstall.
    fn destination_visible(&self, enabled: bool) -> bool {
        // Errors, including unusable Pre-Release storage, never stand in for consent; once
        // consent is given the page shows them as scoped errors.
        self.discovering(enabled) || self.entries.iter().any(|entry| entry.installed.is_some())
    }

    /// Whether new builds are looked for and offered: consent, on a platform that can verify them.
    fn discovering(&self, enabled: bool) -> bool {
        enabled && self.available
    }

    pub(crate) fn available(&self) -> bool {
        self.available
    }

    pub fn busy(&self) -> bool {
        self.backend.as_ref().is_some_and(|backend| backend.has_active_jobs()) || self.entries.iter().any(|entry| entry.job.is_some())
    }

    pub fn configure(&mut self, settings: &Settings) {
        let options = BackendOptions {
            nightly_enabled: self.discovering(settings.nightly_enabled),
            simultaneous_downloads: settings.simultaneous_downloads,
            ..Default::default()
        };
        if self.options.as_ref() != Some(&options) {
            if let Some(backend) = &mut self.backend {
                backend.send(Command::Configure(options.clone()));
            }
            self.options = Some(options);
        }
    }

    pub fn send(&mut self, command: Command, enabled: bool) {
        if !self.discovering(enabled)
            && matches!(&command, Command::Install { .. } | Command::InstallAll { .. } | Command::Replace { .. } | Command::CheckForUpdates { .. })
        {
            return; // Consent is not inferred from installed copies or from an old pending click.
        }
        if let Some(backend) = &mut self.backend {
            if matches!(&command, Command::CheckForUpdates { .. }) {
                self.requested_at = Some(backend.now());
            }
            backend.send(command);
        }
    }

    pub fn logic(&mut self, settings: &Settings) {
        self.configure(settings);
        let events = self.backend.as_mut().map_or_else(Vec::new, |backend| backend.poll());
        for event in events {
            match event {
                Event::CheckStarted => self.checking = true,
                Event::CheckFinished { .. } => self.checking = false,
                Event::NightlyStatus { app, mut status } => {
                    if self.entries.iter().any(|entry| entry.app.id == app) {
                        status.message = status.message.map(|message| message.chars().take(512).collect());
                        self.statuses.insert(app, status);
                        self.attempted_at = self.statuses.values().filter_map(|status| status.attempted_at).max();
                        self.checked_at = self.statuses.values().filter_map(|status| status.verified_at).max();
                    }
                }
                Event::Latest { app, mut latest, from } => {
                    // An in-flight check can finish after consent is withdrawn. It must not
                    // create discoveries or replace the retained local-management presentation.
                    if self.available
                        && settings.nightly_enabled
                        && let Some(entry) = self.entries.iter_mut().find(|e| e.app.id == app)
                    {
                        if let LatestRelease::Failed(why) = &mut latest {
                            *why = why.chars().take(512).collect();
                            let status = self.statuses.entry(app).or_default();
                            status.stale = true;
                            status.message = Some(why.clone());
                            entry.installable = Some(false);
                        }
                        if !(matches!(latest, LatestRelease::Failed(_)) && matches!(entry.latest, Some(LatestRelease::Found(_)))) {
                            entry.latest = Some(latest);
                            entry.origin = Some(from);
                        }
                    }
                }
                Event::Detected { app, installed } => {
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.app.id == app) {
                        entry.installed = installed;
                        entry.detected = true;
                    }
                }
                Event::Installable { app, installable } => {
                    if let Some(entry) = self.entries.iter_mut().find(|e| e.app.id == app) {
                        entry.installable = Some(self.available && settings.nightly_enabled && installable);
                    }
                }
                Event::Progress { app, phase } => {
                    if let Some(entry) = self.entries.iter_mut().find(|entry| entry.app.id == app) {
                        entry.job = Some(phase);
                    }
                }
                Event::JobFinished { app, result } => {
                    if let Some(entry) = self.entries.iter_mut().find(|entry| entry.app.id == app) {
                        entry.job = None;
                    }
                    if let Err(why) = result {
                        self.notices.push(why);
                        if self.notices.len() > 4 {
                            self.notices.remove(0);
                        }
                    }
                }
                Event::Notice { result: Err(why), .. } => {
                    self.notices.push(why);
                    if self.notices.len() > 4 {
                        self.notices.remove(0);
                    }
                }
                _ => {}
            }
        }
        let Some(backend) = &mut self.backend else { return };
        // Nightly has its own six-hour policy, independent of Stable's user preference.
        // Poll persisted metadata before requesting work. Native remains the final gate,
        // including server backoff and manual request coalescing.
        let now = backend.now();
        // Like Native, treat an attempt in the future after clock rollback as stale.
        let due = self.attempted_at.is_none_or(|at| now < at || now.saturating_sub(at) >= 6 * 60 * 60);
        // Unacknowledged requests are coalesced briefly, not treated as actual attempts.
        // A future request permits one replacement, which resets this baseline to now.
        let request_allowed = self.requested_at.is_none_or(|at| now < at || now.saturating_sub(at) >= 60);
        let retry_allowed = self.statuses.values().all(|status| status.retry_at.is_none_or(|at| now >= at));
        if self.available && settings.nightly_enabled && !self.checking && due && request_allowed && retry_allowed {
            // A request is not a successful verification, including when Native coalesces it.
            self.requested_at = Some(now);
            backend.send(Command::CheckForUpdates { force: false });
        }
    }
}

impl LauncherUi {
    pub(crate) fn open_nightly_consent(&mut self, return_focus: egui::Id) {
        self.nightly.consent_open = true;
        self.nightly.consent_focus = true;
        self.nightly.return_focus = Some(return_focus);
        self.nightly.return_settings = matches!(self.dialog, Some(crate::app::Dialog::Settings));
    }

    /// Width reserved in the top bar, before decorative caption text gets space.
    /// Clearing stale active state here also handles removal of the final retained copy.
    pub(crate) fn channel_navigation_width(&mut self, ui: &Ui) -> Option<f32> {
        if !self.nightly.destination_visible(self.settings.nightly_enabled) {
            self.nightly.active = false;
            self.nightly.focus_tab = false;
            return None;
        }
        let font = egui::TextStyle::Button.resolve(ui.style());
        let width: f32 = [tl!("Stable"), tl!("Pre-Release")]
            .iter()
            .map(|label| ui.painter().layout_no_wrap((*label).into(), font.clone(), Tokens::get(ui.ctx()).ink).size().x + 24.0)
            .sum();
        Some(width)
    }

    pub(crate) fn channel_navigation(&mut self, ui: &mut Ui) {
        let tokens = Tokens::get(ui.ctx());
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.spacing_mut().button_padding.x = 12.0;
            ui.visuals_mut().selection.bg_fill = tokens.invert_bg;
            ui.visuals_mut().selection.stroke.color = tokens.invert_fg;
            ui.horizontal(|ui| {
                let stable = RichText::new(tl!("Stable")).color(if self.nightly.active { tokens.ink } else { tokens.invert_fg });
                ui.selectable_value(&mut self.nightly.active, false, stable);
                let prerelease = RichText::new(tl!("Pre-Release")).color(if self.nightly.active { tokens.invert_fg } else { tokens.ink });
                let tab = ui.selectable_value(&mut self.nightly.active, true, prerelease);
                if self.nightly.focus_tab {
                    tab.request_focus();
                    self.nightly.focus_tab = false;
                }
            });
        });
    }

    /// Align the actions to the title's right when their translated labels fit.
    /// Allocate left to right so visual and keyboard order remain the same.
    fn nightly_page_heading(&mut self, ui: &mut Ui) -> bool {
        let heading = tl!("Pre-Release");
        let labels = [tl!("Check for updates"), tl!("Turn off pre-release builds")];
        let color = Tokens::get(ui.ctx()).ink;
        // Query the same stock style as Button: this SDK's default uses Body,
        // not TextStyle::Button. A narrower estimate would wrap the second action.
        let button_style: egui::widget_style::ButtonStyle = ui.widget_style(ui.scope_id().with("nightly-heading-actions"), &Default::default());
        let button_font = button_style.atom_layout.text_style.font_id;
        let button_margin = button_style.atom_layout.frame.total_margin().sum().x;
        let actions_width: f32 =
            labels.iter().map(|label| ui.painter().layout_no_wrap((*label).into(), button_font.clone(), color).size().x + button_margin).sum::<f32>()
                + ui.spacing().item_spacing.x;
        let heading_width = ui.painter().layout_no_wrap(heading.into(), egui::TextStyle::Heading.resolve(ui.style()), color).size().x;
        let inline = self.nightly.discovering(self.settings.nightly_enabled)
            && heading_width + actions_width + ui.spacing().item_spacing.x <= ui.available_width();
        if inline {
            ui.horizontal(|ui| {
                ui.heading(heading);
                ui.add_space((ui.available_width() - actions_width).max(0.0));
                self.nightly_page_actions(ui);
            });
        } else {
            ui.heading(heading);
        }
        inline
    }

    fn nightly_page_actions(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            let now = self.nightly.backend.as_ref().map_or(0, |backend| backend.now());
            let retry_allowed = self.nightly.statuses.values().all(|status| status.retry_at.is_none_or(|at| now >= at));
            if ui.add_enabled(!self.nightly.checking && retry_allowed, egui::Button::new(tl!("Check for updates"))).clicked() {
                self.act(UiAction::NightlyCommand(Command::CheckForUpdates { force: true }));
            }
            if ui.button(tl!("Turn off pre-release builds")).clicked() {
                self.act(UiAction::NightlyConsent(false));
            }
        });
    }

    pub(crate) fn nightly_page(&mut self, ui: &mut Ui) {
        let tokens = Tokens::get(ui.ctx());
        let inline_actions = self.nightly_page_heading(ui);
        ui.label(tl!("The pre-release channel is updated automatically every night with the latest code. It may have bugs, performance and stability issues, and unfinished work. Please use the stable channel for important work."));
        ui.add_space(8.0);
        if self.settings.nightly_enabled && !self.nightly.available() {
            egui::Frame::new().fill(tokens.sunken).inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(tl!("Pre-release builds aren't available on this platform yet.")).strong()).wrap());
                ui.add(egui::Label::new(tl!("Separate installs, settings and data. Turning this off keeps installed copies and their data.")).wrap());
                if ui.button(tl!("Turn off pre-release builds")).clicked() {
                    self.act(UiAction::NightlyConsent(false));
                }
            });
        } else if !self.settings.nightly_enabled {
            egui::Frame::new().fill(tokens.sunken).inner_margin(12).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tl!("Pre-release builds are off.")).strong());
                ui.add(
                    egui::Label::new(tl!("Installed copies and their data are kept. To discover new builds, enable pre-release builds in Settings."))
                        .wrap(),
                );
                // Only Settings may enable the channel; this page only manages retained copies.
                if ui.button(tl!("Settings")).clicked() {
                    self.dialog = Some(crate::app::Dialog::Settings);
                }
            });
        } else if !inline_actions {
            self.nightly_page_actions(ui);
        }
        if self.nightly.backend.is_none() {
            ui.label(tl!("Pre-release installation is unavailable on this platform."));
        }
        if self.nightly.checking {
            ui.spinner();
        }
        ui.add_space(12.0);
        let entries = self.nightly.entries.clone();
        let enabled = self.nightly.discovering(self.settings.nightly_enabled);
        let visible: Vec<_> =
            entries.iter().filter(|e| e.installed.is_some() || (enabled && matches!(&e.latest, Some(LatestRelease::Found(_))))).collect();
        if visible.is_empty() {
            egui::Frame::new().fill(tokens.sunken).inner_margin(24).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if self.nightly.checking {
                    ui.label(tl!("Checking…"));
                } else if enabled && let Some(status) = self.nightly.statuses.values().find(|status| status.message.is_some()) {
                    ui.label(tl!("Could not check for pre-release builds."));
                    let now = self.nightly.backend.as_ref().map_or(0, |backend| backend.now());
                    nightly_status_labels(ui, status, now);
                } else if enabled
                    && self.nightly.checked_at.is_some()
                    && entries.iter().any(|entry| matches!(entry.latest, Some(LatestRelease::None)))
                {
                    ui.label(tl!("No pre-release builds are available yet."));
                    if let Some(status) = self.nightly.statuses.values().find(|status| !status.stale && status.verified_at.is_some()) {
                        let now = self.nightly.backend.as_ref().map_or(0, |backend| backend.now());
                        nightly_status_labels(ui, status, now);
                    }
                } else {
                    ui.label(tl!("Not checked yet"));
                    if enabled && let Some(status) = self.nightly.statuses.values().find(|status| status.attempted_at.is_some()) {
                        let now = self.nightly.backend.as_ref().map_or(0, |backend| backend.now());
                        nightly_status_labels(ui, status, now);
                    }
                }
            });
        } else if enabled {
            // A failed app with no previous selection has no card. Do not hide its error
            // merely because some other app has a verified build.
            for entry in entries.iter().filter(|entry| !visible.iter().any(|shown| shown.app.id == entry.app.id)) {
                if let Some(status) = self.nightly.statuses.get(entry.app.id).filter(|status| status.message.is_some()) {
                    ui.label(RichText::new(entry.app.name).strong());
                    let now = self.nightly.backend.as_ref().map_or(0, |backend| backend.now());
                    nightly_status_labels(ui, status, now);
                }
            }
        }
        egui::ScrollArea::vertical().id_salt("pre-release-apps").show(ui, |ui| {
            let width = ui.available_width();
            let gap = crate::theme::PAD / 2.0;
            let columns = (((width + gap) / (crate::views::CARD_MIN_W + gap)).floor() as usize).max(1);
            let card_width = (width - gap * columns.saturating_sub(1) as f32) / columns as f32;
            for row in visible.chunks(columns) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for entry in row {
                        ui.allocate_ui_with_layout(egui::vec2(card_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            self.nightly_card(ui, entry, enabled, &tokens);
                        });
                    }
                });
                ui.add_space(gap);
            }
        });
        for notice in &self.nightly.notices {
            ui.add(egui::Label::new(notice).wrap());
        }
    }

    fn nightly_card(&mut self, ui: &mut Ui, entry: &AppEntry, enabled: bool, tokens: &Tokens) {
        egui::Frame::new().fill(tokens.bg).stroke(egui::Stroke::new(1.0, tokens.line_strong)).inner_margin(16).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(crate::theme::CONTROL_H * 3.0);
            ui.horizontal_wrapped(|ui| {
                crate::icons::nightly_icon(ui, entry.app.id, entry.app.name, 48.0);
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(crate::widgets::heading(entry.app.name, crate::theme::DIALOG_TITLE, tokens.ink_strong));
                        crate::widgets::badge(ui, tl!("Pre-Release"), tokens.accent_ink);
                    });
                    if entry.installed.is_some() {
                        ui.label(RichText::new(tl!("Installed pre-release copy")).color(tokens.ink));
                    }
                    if let Some(version) = nightly_card_version(entry, enabled) {
                        ui.add(egui::Label::new(RichText::new(nightly_build_label(&version, crate::i18n::current())).strong()).wrap());
                        // A technical identifier is not prose: fill each line instead of
                        // leaving the base version alone before the first hyphen break.
                        let mut text =
                            egui::text::LayoutJob::simple(version, egui::TextStyle::Small.resolve(ui.style()), tokens.muted, ui.available_width());
                        text.wrap.break_anywhere = true;
                        ui.add(egui::Label::new(text).wrap());
                    }
                    if enabled {
                        if let Some(status) = self.nightly.statuses.get(entry.app.id) {
                            let now = self.nightly.backend.as_ref().map_or(0, |backend| backend.now());
                            nightly_status_labels(ui, status, now);
                        }
                        match &entry.latest {
                            Some(LatestRelease::Failed(_)) => {}
                            Some(LatestRelease::None) => {
                                ui.add(egui::Label::new(tl!("No pre-release build is available yet.")).wrap());
                            }
                            Some(LatestRelease::Found(_)) => {}
                            None => {
                                ui.label(tl!("Not checked yet"));
                            }
                        }
                    }
                });
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                // Existing stable actions always go to the stable backend, never this context.
                if self.is_installed(entry.app.id) && ui.button(tl!("Open stable")).clicked() {
                    self.launch_app(entry.app.id);
                }
                if entry.installed.is_some() {
                    if ui.add(egui::Button::new(RichText::new(tl!("Open")).color(tokens.invert_fg)).fill(tokens.invert_bg)).clicked() {
                        self.act(UiAction::NightlyCommand(Command::Launch { app: entry.app.id.into(), args: Vec::new() }));
                    }
                    if ui.button(tl!("Show installed copy")).clicked() {
                        self.act(UiAction::NightlyCommand(Command::Reveal { app: entry.app.id.into() }));
                    }
                    // Removal needs its own confirmation, independent of opt-in or stable records.
                    let uninstall = ui.button(tl!("Uninstall pre-release…"));
                    if uninstall.clicked() {
                        self.nightly_uninstall_prompt(entry.app.id, uninstall.id);
                    }
                }
                // A running job replaces the install action with its progress, as on stable cards.
                if let Some(phase) = entry.job {
                    let (fraction, caption) = nightly_job_progress(phase);
                    crate::widgets::progress(ui, fraction, &caption, tokens.accent, ui.available_width());
                } else if enabled
                    && !nightly_install_is_redundant(entry)
                    && ui
                        .add_enabled(
                            entry.installable == Some(true)
                                && self.nightly.statuses.get(entry.app.id).is_some_and(|status| !status.stale && status.verified_at.is_some()),
                            egui::Button::new(RichText::new(tl!("Install pre-release")).color(tokens.invert_fg))
                                .fill(tokens.invert_bg)
                                .min_size(egui::vec2(ui.available_width(), crate::theme::CONTROL_H)),
                        )
                        .clicked()
                {
                    self.act(UiAction::NightlyCommand(Command::Install { app: entry.app.id.into() }));
                }
            });
        });
    }

    pub(crate) fn nightly_consent_modal(&mut self, ctx: &egui::Context) {
        if self.nightly.uninstall.is_none() && !self.nightly.consent_open {
            if let Some(id) = self.nightly.return_focus.take() {
                ctx.memory_mut(|m| m.request_focus(id));
            }
            return;
        }
        let tokens = Tokens::get(ctx);
        let frame = egui::Frame::new()
            .fill(tokens.bg)
            .stroke(egui::Stroke::new(1.0, tokens.line_strong))
            .shadow(crate::theme::shadow(&tokens))
            .inner_margin(24);
        if let Some(id) = self.nightly.uninstall.clone() {
            let mut remove = false;
            let mut cancel = false;
            let response = egui::Modal::new(egui::Id::unique("nightly-uninstall-v1")).frame(frame).show(ctx, |ui| {
                ui.set_width(440.0_f32.min(ctx.content_rect().width() - 80.0).max(100.0));
                ui.label(crate::widgets::heading(tl!("Uninstall pre-release?"), crate::theme::DIALOG_TITLE, tokens.ink_strong));
                ui.add_space(8.0);
                ui.add(egui::Label::new(tl!("Only this pre-release copy will be removed. Stable copies and user data are kept.")).wrap());
                ui.add_space(16.0);
                ui.horizontal_wrapped(|ui| {
                    let no = ui.button(tl!("Cancel"));
                    if self.nightly.uninstall_focus {
                        no.request_focus();
                        self.nightly.uninstall_focus = false;
                    }
                    cancel = no.clicked();
                    remove = ui.button(tl!("Uninstall pre-release")).clicked();
                });
            });
            if remove {
                self.act(UiAction::NightlyCommand(Command::Uninstall { app: id }));
                self.nightly.return_focus = None;
            }
            if remove || cancel || response.should_close() {
                self.nightly.uninstall = None;
            }
            return;
        }
        let mut enable = false;
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::unique("nightly-consent-v1")).frame(frame).show(ctx, |ui| {
            ui.set_width(440.0_f32.min(ctx.content_rect().width() - 80.0).max(100.0));
            ui.label(crate::widgets::heading(tl!("Enable pre-release builds?"), crate::theme::DIALOG_TITLE, tokens.ink_strong));
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(tl!("Pre-release builds let you try new work early, but they will have bugs and may lose unsaved work.")).strong()).wrap());
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(tl!(
                    "They install alongside your stable apps with their own settings and data, leaving your stable setup untouched. Always back up important files before opening them in a pre-release build, as newer saves may not open in stable versions."
                ))
                .wrap(),
            );
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(tl!(
                    "You can turn this off at any time without deleting your installed pre-release apps or their data."
                ))
                .wrap(),
            );
            ui.add_space(16.0);
            ui.horizontal_wrapped(|ui| {
                let no = ui.button(tl!("Cancel"));
                if self.nightly.consent_focus {
                    no.request_focus();
                    self.nightly.consent_focus = false;
                }
                cancel = no.clicked();
                enable = ui.button(tl!("Enable pre-release builds")).clicked();
            });
        });
        if enable {
            self.act(UiAction::NightlyConsent(true));
            self.nightly.return_focus = None;
            self.nightly.focus_tab = true;
        } else if cancel || response.should_close() {
            self.nightly.consent_open = false; // Cancel/Escape/outside never grants consent.
            if self.nightly.return_settings {
                self.dialog = Some(crate::app::Dialog::Settings);
            }
            // Restore focus next frame, after the returned control is present again.
        }
    }

    fn nightly_uninstall_prompt(&mut self, id: &str, return_focus: egui::Id) {
        self.nightly.uninstall = Some(id.into());
        self.nightly.uninstall_focus = true;
        self.nightly.return_focus = Some(return_focus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artcraft_launcher_core::protocol::Capabilities;
    use std::cell::RefCell;
    use std::rc::Rc;

    pub(super) struct Recorder(pub(super) Rc<RefCell<Vec<Command>>>);
    impl Backend for Recorder {
        fn send(&mut self, command: Command) {
            self.0.borrow_mut().push(command);
        }
        fn poll(&mut self) -> Vec<Event> {
            Vec::new()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { manage_apps: true, reveal_label: "Show", self_update: false, desktop_shortcuts: false }
        }
        fn platform(&self) -> Option<Platform> {
            None
        }
        fn now(&self) -> i64 {
            1000
        }
    }

    #[test]
    fn off_keeps_installed_copies_but_rejects_new_install_and_update_commands() {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut state = NightlyState::new(Some(Box::new(Recorder(sent.clone()))), None);
        let mut settings = Settings { nightly_enabled: true, ..Default::default() };
        state.entries[0].installed = Some(artcraft_launcher_core::Installed {
            version: None,
            source: artcraft_launcher_core::InstallSource::Launcher,
            location: "/nightly/photo".into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        });
        let before = state.entries[0].installed.clone();
        state.configure(&settings);
        settings.nightly_enabled = false;
        state.configure(&settings);
        sent.borrow_mut().clear();
        state.send(Command::Install { app: "photocraft".into() }, false);
        state.send(Command::InstallAll { apps: vec!["photocraft".into()] }, false);
        state.send(Command::CheckForUpdates { force: true }, false);
        state.send(Command::Launch { app: "photocraft".into(), args: Vec::new() }, false);
        state.send(Command::Reveal { app: "photocraft".into() }, false);
        assert_eq!(state.entries[0].installed, before);
        assert!(!sent.borrow().iter().any(|c| matches!(
            c,
            Command::Install { .. } | Command::InstallAll { .. } | Command::CheckForUpdates { .. } | Command::Uninstall { .. }
        )));
        assert!(sent.borrow().iter().any(|c| matches!(c, Command::Launch { .. })));
        assert!(sent.borrow().iter().any(|c| matches!(c, Command::Reveal { .. })));
    }

    #[test]
    fn stable_and_nightly_commands_do_not_share_the_same_backend_instance() {
        let stable = Rc::new(RefCell::new(Vec::new()));
        let nightly = Rc::new(RefCell::new(Vec::new()));
        let mut stable_backend = Recorder(stable.clone());
        let mut state = NightlyState::new(Some(Box::new(Recorder(nightly.clone()))), None);
        state.send(Command::Install { app: "photocraft".into() }, true);
        stable_backend.send(Command::Launch { app: "photocraft".into(), args: Vec::new() });
        assert!(!stable.borrow().iter().any(|c| matches!(c, Command::Install { .. })));
        assert!(!nightly.borrow().iter().any(|c| matches!(c, Command::Launch { .. })));
    }

    #[test]
    fn old_settings_do_not_grant_pre_release_consent() {
        let settings: Settings = serde_json::from_str(r#"{"prereleases":true,"versions":{"photocraft":"Nightly"}}"#).unwrap();
        assert!(!settings.nightly_enabled);
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(!serde_json::from_str::<Settings>(&saved).unwrap().nightly_enabled);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut nightly = NightlyState::new(Some(Box::new(Recorder(sent.clone()))), None);
        nightly.configure(&settings);
        assert!(
            matches!(sent.borrow().as_slice(), [Command::Configure(options)] if !options.nightly_enabled && !options.prereleases),
            "legacy beta/RC opt-in sends only disabled nightly configuration, never discovery or install work"
        );
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Host;
    impl crate::Host for Host {}

    #[test]
    fn consent_has_real_keyboard_focus_escape_cancels_and_confirm_is_explicit() {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut backend = Some(super::tests::Recorder(sent));
        let mut harness = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
            move |ui, state: &mut Option<LauncherUi>| {
                if state.is_none()
                    && let Some(backend) = backend.take()
                {
                    *state =
                        Some(LauncherUi::new(ui.ctx(), Box::new(backend), Settings { language: "en".into(), notify: false, ..Default::default() }));
                }
                if let Some(launcher) = state {
                    launcher.logic(&ui.ctx().clone(), &mut Host);
                    launcher.ui(ui, &mut Host);
                }
            },
            None,
        );
        harness.run_steps(5);
        assert!(harness.query_by_label("Try nightly builds").is_none());
        assert!(harness.query_by_label("Pre-Release").is_none(), "no first-run OFF destination");
        harness.get_by_label("Settings").focus();
        harness.key_press(egui::Key::Enter);
        harness.run_steps(3);
        harness.get_by_label("Include pre-releases (betas and release candidates)").focus();
        harness.key_press(egui::Key::Space);
        harness.run_steps(3);
        assert!(harness.state().as_ref().unwrap().settings.prereleases, "the existing beta/RC filter still changes independently");
        assert!(!harness.state().as_ref().unwrap().settings.nightly_enabled);
        assert!(!harness.state().as_ref().unwrap().nightly.consent_open);
        assert!(harness.query_by_label("Pre-Release").is_none(), "the beta/RC filter never opts into nightly discovery");
        harness.get_by_label("Discover nightly builds (Pre-Release page)").focus();
        harness.key_press(egui::Key::Space);
        harness.run_steps(3);
        assert!(!harness.state().as_ref().unwrap().settings.nightly_enabled);
        assert!(harness.get_by_label("Cancel").accesskit_node().is_focused());
        harness.key_press(egui::Key::Escape);
        harness.run_steps(3);
        assert!(!harness.state().as_ref().unwrap().settings.nightly_enabled);
        assert!(!harness.state().as_ref().unwrap().nightly.consent_open);
        assert!(matches!(harness.state().as_ref().unwrap().dialog, Some(crate::app::Dialog::Settings)));
        assert!(harness.state().as_ref().unwrap().settings.prereleases, "cancelling nightly consent keeps the beta/RC choice");
        assert!(
            harness.get_by_label("Discover nightly builds (Pre-Release page)").accesskit_node().is_focused(),
            "Cancel restores the real Settings checkbox focus"
        );
        harness.get_by_label("Discover nightly builds (Pre-Release page)").focus();
        harness.key_press(egui::Key::Space);
        harness.run_steps(3);
        harness.key_press(egui::Key::Tab);
        harness.run_steps(2);
        assert!(harness.get_by_label("Enable pre-release builds").accesskit_node().is_focused());
        harness.key_press(egui::Key::Enter);
        harness.run_steps(3);
        assert!(harness.state().as_ref().unwrap().settings.nightly_enabled);
        assert!(harness.state().as_ref().unwrap().settings.prereleases, "nightly confirmation does not alter the beta/RC filter");
        assert!(harness.state().as_ref().unwrap().nightly.active, "dedicated destination appears only after consent");
        assert!(!harness.state().as_ref().unwrap().nightly.consent_open);
        assert!(harness.get_by_role_and_label(egui::accesskit::Role::Button, "Pre-Release").accesskit_node().is_focused());
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn unavailable_backend_does_not_retry_a_failed_check_every_frame() {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut state = NightlyState::new(Some(Box::new(super::tests::Recorder(sent.clone()))), None);
        let settings = Settings { nightly_enabled: true, ..Default::default() };
        for _ in 0..20 {
            state.logic(&settings);
        }
        assert_eq!(sent.borrow().iter().filter(|c| matches!(c, Command::CheckForUpdates { .. })).count(), 1);
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn first_run_off_hides_destination_until_settings_consent() {
        let state = NightlyState::new(None, None);
        assert!(!state.destination_visible(false));
        assert!(state.destination_visible(true));
        assert!(state.entries.iter().all(|entry| entry.installed.is_none()));
    }

    #[test]
    fn switching_off_keeps_retained_copy_management_without_granting_consent() {
        let mut state = NightlyState::new(None, None);
        let copy = artcraft_launcher_core::Installed {
            version: None,
            source: artcraft_launcher_core::InstallSource::Launcher,
            location: "/nightly/photo".into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        };
        state.entries[0].installed = Some(copy.clone());
        assert!(state.destination_visible(false));
        assert!(!Settings::default().nightly_enabled);
        assert_eq!(state.entries[0].installed, Some(copy));
        state.entries[0].installed = None;
        assert!(!state.destination_visible(false));
    }
}

#[cfg(test)]
mod build_date_tests {
    use super::*;
    const VERSION: &str = "0.23.1-nightly.20261120.20261120211929.1.gace4bfc68195";
    fn entry() -> AppEntry {
        AppEntry::new(artcraft_launcher_core::hosted_catalog::apps().iter().find(|app| app.id == "photocraft").unwrap())
    }
    #[test]
    fn build_date_is_a_validated_calendar_day_not_a_local_timestamp() {
        assert_eq!(nightly_build_date(VERSION), Some((2026, 11, 20)));
        assert_eq!(nightly_build_date(&format!("v{VERSION}")), Some((2026, 11, 20)));
        assert_eq!(nightly_build_date("0.23.1-nightly.20240229.1.1.gace4bfc68195"), Some((2024, 2, 29)));
    }
    #[test]
    fn malformed_and_non_calendar_tags_never_invent_a_build_day() {
        for bad in [
            "",
            "nightly",
            "v0.23.1",
            "0.23.1-beta.20261120.1.1.gace4bfc68195",
            "0.23.1-nightly.20260229.1.1.gace4bfc68195",
            "0.23.1-nightly.20260230.1.1.gace4bfc68195",
            "0.23.1-nightly.20261301.1.1.gace4bfc68195",
            "0.23.1-nightly.20261100.1.1.gace4bfc68195",
            "0.23.1-nightly.20261120.0.1.gace4bfc68195",
            "0.23.1-nightly.20261120.1.0.gace4bfc68195",
            "0.23.1-nightly.20261120.1.1.gbad",
            "0.23.1-nightly.20261120.1.1.gxxxxxxxxxxxx",
        ] {
            assert_eq!(nightly_build_date(bad), None, "{bad}");
            assert_eq!(nightly_build_label(bad, crate::i18n::Lang::EN), "Nightly");
        }
    }
    #[test]
    fn build_day_uses_the_ui_languages_date_order() {
        assert_eq!(nightly_build_label(VERSION, crate::i18n::Lang::EN), "Nightly 11/20/2026");
        assert_eq!(nightly_build_label(VERSION, crate::i18n::Lang::DE), "Nightly 20.11.2026");
    }
    #[test]
    fn build_day_comes_from_the_tag_not_publication_or_a_stale_parsed_version() {
        let mut entry = entry();
        entry.latest = Some(LatestRelease::Found(artcraft_launcher_core::Release {
            tag: format!("v{VERSION}"),
            version: artcraft_launcher_core::version::parse("0.23.1"),
            published_at: Some(1_795_294_800),
            ..Default::default()
        }));
        let full = nightly_card_version(&entry, true).unwrap();
        assert_eq!(full, VERSION);
        assert_eq!(nightly_build_label(&full, crate::i18n::Lang::EN), "Nightly 11/20/2026");
    }
    #[test]
    fn off_retained_copy_uses_its_version_not_a_newer_cached_release() {
        let mut entry = entry();
        let installed = "0.23.1-nightly.20261119.1.1.gace4bfc68195";
        entry.installed = Some(artcraft_launcher_core::Installed {
            version: artcraft_launcher_core::version::parse(installed),
            source: artcraft_launcher_core::InstallSource::Launcher,
            location: "/apps/PhotoCraft Nightly".into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        });
        entry.latest = Some(LatestRelease::Found(artcraft_launcher_core::Release {
            tag: format!("v{VERSION}"),
            version: artcraft_launcher_core::version::parse(VERSION),
            ..Default::default()
        }));
        assert_eq!(nightly_card_version(&entry, false).as_deref(), Some(installed));
        assert_eq!(nightly_card_version(&entry, true).as_deref(), Some(VERSION));
        entry.latest = Some(LatestRelease::Failed("offline".into()));
        assert_eq!(nightly_card_version(&entry, true).as_deref(), Some(installed));
        entry.installed.as_mut().unwrap().version = None;
        assert!(nightly_card_version(&entry, false).is_none());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use std::cell::RefCell;
    use std::rc::Rc;

    const VERSION: &str = "0.23.1-nightly.20261120.20261120211929.1.gace4bfc68195";
    struct Host;
    impl crate::Host for Host {}

    fn page(width: f32) -> Harness<'static, Option<LauncherUi>> {
        let mut harness = Harness::builder().with_size(egui::vec2(width, 900.0)).build_ui_state(
            |ui, state: &mut Option<LauncherUi>| {
                if state.is_none() {
                    let backend = super::tests::Recorder(Rc::new(RefCell::new(Vec::new())));
                    let mut launcher = LauncherUi::new(
                        ui.ctx(),
                        Box::new(backend),
                        Settings { language: "en".into(), notify: false, nightly_enabled: true, ..Default::default() },
                    );
                    launcher.nightly.active = true;
                    let photo = launcher.nightly.entries.iter_mut().find(|entry| entry.app.id == "photocraft").unwrap();
                    photo.latest = Some(LatestRelease::Found(artcraft_launcher_core::Release { tag: format!("v{VERSION}"), ..Default::default() }));
                    *state = Some(launcher);
                }
                if let Some(launcher) = state {
                    launcher.logic(&ui.ctx().clone(), &mut Host);
                    launcher.ui(ui, &mut Host);
                }
            },
            None,
        );
        harness.run_steps(5);
        harness
    }

    #[test]
    fn page_actions_share_the_heading_row_when_they_fit_and_stack_when_narrow() {
        for width in [1180.0, 560.0] {
            let harness = page(width);
            let heading = harness
                .get_all_by_label("Pre-Release")
                .filter(|node| node.accesskit_node().role() == egui::accesskit::Role::Label)
                .map(|node| node.rect())
                .min_by(|a, b| a.top().total_cmp(&b.top()))
                .unwrap();
            let check = harness.get_by_label("Check for updates").rect();
            let off = harness.get_by_label("Turn off pre-release builds").rect();
            if width > 1000.0 {
                assert!((heading.center().y - check.center().y).abs() <= 0.5, "actions align with the title");
                assert!(check.left() > heading.right(), "actions stay to the title's right");
                assert!(
                    off.right() > width - 48.0 && off.right() <= width,
                    "actions align to the content's right edge: heading={heading:?}, check={check:?}, off={off:?}, viewport={width}"
                );
            } else {
                assert!(check.top() > heading.bottom(), "actions stack instead of squeezing the title");
                assert!(off.right() <= width, "translated controls must remain within the viewport");
            }
        }
    }

    #[test]
    fn full_version_fills_the_first_row_and_retains_its_exact_accessible_value() {
        let harness = page(1180.0);
        let full = harness.get_by_label(VERSION);
        assert_eq!(full.accesskit_node().value().as_deref(), Some(VERSION));
        let first_row = full.children_recursive().find(|node| node.accesskit_node().role() == egui::accesskit::Role::TextRun).unwrap();
        let text = first_row.accesskit_node().value().unwrap();
        assert!(text.contains("nightly."), "do not leave the base version alone before a hyphen break: {text}");
    }

    #[test]
    fn inline_actions_keep_visual_keyboard_order_and_can_turn_the_channel_off() {
        let mut harness = page(1180.0);
        harness.get_by_label("Check for updates").focus();
        harness.run_steps(2);
        harness.key_press(egui::Key::Tab);
        harness.run_steps(2);
        assert!(harness.get_by_label("Turn off pre-release builds").accesskit_node().is_focused());
        harness.key_press(egui::Key::Enter);
        harness.run_steps(3);
        assert!(!harness.state().as_ref().unwrap().settings.nightly_enabled);
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use artcraft_launcher_core::protocol::{Capabilities, Origin, Source};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct FakeBackend {
        events: Rc<RefCell<Vec<Event>>>,
        sent: Rc<RefCell<Vec<Command>>>,
        now: Rc<Cell<i64>>,
        active_jobs: Rc<Cell<bool>>,
    }
    impl Backend for FakeBackend {
        fn has_active_jobs(&self) -> bool {
            self.active_jobs.get()
        }
        fn send(&mut self, command: Command) {
            self.sent.borrow_mut().push(command);
        }
        fn poll(&mut self) -> Vec<Event> {
            std::mem::take(&mut *self.events.borrow_mut())
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { manage_apps: true, reveal_label: "Show", self_update: false, desktop_shortcuts: false }
        }
        fn platform(&self) -> Option<Platform> {
            None
        }
        fn now(&self) -> i64 {
            self.now.get()
        }
    }
    #[test]
    fn review_probe_nightly_jobs_block_quit_before_progress_through_off_until_terminal() {
        let fake = FakeBackend::default();
        let stable = FakeBackend::default();
        let ctx = egui::Context::default();
        let mut ui = LauncherUi::new(&ctx, Box::new(stable), Settings { language: "en".into(), ..Default::default() });
        ui.nightly = NightlyState::new(Some(Box::new(fake.clone())), None);
        let mut settings = Settings { nightly_enabled: true, ..Default::default() };
        assert!(!ui.busy());
        fake.active_jobs.set(true); // Native admission has happened; no progress event exists yet.
        assert!(ui.busy());
        fake.events.borrow_mut().push(Event::Progress { app: "photocraft".into(), phase: artcraft_launcher_core::protocol::JobPhase::Removing });
        ui.nightly.logic(&settings);
        assert!(ui.nightly.entries.iter().find(|entry| entry.app.id == "photocraft").unwrap().job.is_some());
        settings.nightly_enabled = false;
        ui.nightly.logic(&settings);
        assert!(ui.busy());
        fake.active_jobs.set(false);
        fake.events.borrow_mut().push(Event::JobFinished { app: "photocraft".into(), result: Ok("finished".into()) });
        ui.nightly.logic(&settings);
        assert!(!ui.busy());
        fake.events.borrow_mut().push(Event::CheckStarted);
        ui.nightly.logic(&settings);
        assert!(ui.nightly.checking);
        assert!(!ui.busy());
        fake.events.borrow_mut().push(Event::Progress { app: "photocraft".into(), phase: artcraft_launcher_core::protocol::JobPhase::Installing });
        ui.nightly.logic(&settings);
        assert!(ui.busy());
        fake.events.borrow_mut().push(Event::JobFinished { app: "photocraft".into(), result: Err("cancelled".into()) });
        ui.nightly.logic(&settings);
        assert!(!ui.busy());
    }

    fn status(attempted_at: i64, verified_at: i64, stale: bool, retry_at: Option<i64>) -> Event {
        Event::NightlyStatus {
            app: "photocraft".into(),
            status: NightlyStatus {
                attempted_at: Some(attempted_at),
                verified_at: Some(verified_at),
                stale,
                retry_at,
                message: stale.then(|| "offline".into()),
            },
        }
    }
    fn found() -> Event {
        Event::Latest {
            app: "photocraft".into(),
            latest: LatestRelease::Found(artcraft_launcher_core::Release {
                tag: "v0.23.1-nightly.20261120.1.1.gace4bfc68195".into(),
                ..Default::default()
            }),
            from: Origin::GitHub,
        }
    }
    fn finished(at: i64) -> Event {
        Event::CheckFinished { at, source: Source::GitHubApi, rate_limited_until: None }
    }
    fn checks(fake: &FakeBackend) -> usize {
        fake.sent.borrow().iter().filter(|command| matches!(command, Command::CheckForUpdates { .. })).count()
    }

    #[test]
    fn failed_after_success_stops_spinner_without_advancing_verified_time_or_losing_selection() {
        let fake = FakeBackend::default();
        fake.now.set(100);
        let mut state = NightlyState::new(Some(Box::new(fake.clone())), None);
        let settings = Settings { nightly_enabled: true, ..Default::default() };
        fake.events.borrow_mut().extend([Event::CheckStarted, found(), status(100, 100, false, None), finished(100)]);
        state.logic(&settings);
        let previous = state.entries.iter().find(|entry| entry.app.id == "photocraft").unwrap().latest.clone();
        fake.now.set(200);
        fake.events.borrow_mut().extend([
            Event::CheckStarted,
            Event::Latest { app: "photocraft".into(), latest: LatestRelease::Failed("offline".into()), from: Origin::ArtCraftApi },
            status(200, 100, true, Some(600)),
            finished(201),
        ]);
        state.logic(&settings);
        let entry = state.entries.iter().find(|entry| entry.app.id == "photocraft").unwrap();
        assert_eq!(entry.latest, previous);
        assert_eq!(entry.origin, Some(Origin::GitHub));
        assert_eq!(entry.installable, Some(false));
        assert!(!state.checking);
        assert_eq!(state.checked_at, Some(100));
        assert_eq!(state.attempted_at, Some(200));
        assert!(state.statuses["photocraft"].stale);
        assert_eq!(state.statuses["photocraft"].message.as_deref(), Some("offline"));
    }

    #[test]
    fn completion_without_verified_status_never_claims_success() {
        let fake = FakeBackend::default();
        fake.now.set(100);
        fake.events.borrow_mut().extend([Event::CheckStarted, finished(100)]);
        let mut state = NightlyState::new(Some(Box::new(fake.clone())), None);
        state.logic(&Settings::default());
        assert!(!state.checking);
        assert_eq!(state.checked_at, None);
        assert_eq!(state.attempted_at, None);
        assert_eq!(checks(&fake), 0);
    }

    #[test]
    fn persisted_attempt_controls_six_hour_boundary_independently_of_stable_and_clock_rollback() {
        for check_every in [crate::settings::CheckEvery::Hour, crate::settings::CheckEvery::Day, crate::settings::CheckEvery::Never] {
            let fake = FakeBackend::default();
            fake.events.borrow_mut().push(status(1000, 500, true, None));
            fake.now.set(1000 + 6 * 60 * 60 - 1);
            let mut state = NightlyState::new(Some(Box::new(fake.clone())), None);
            let settings = Settings { nightly_enabled: true, check_every, ..Default::default() };
            state.logic(&settings);
            assert_eq!(checks(&fake), 0, "restart polls persisted scheduling metadata before any request");
            assert!(state.entries.iter().all(|entry| entry.latest.is_none()), "metadata never recreates a release body");
            fake.now.set(1000 + 6 * 60 * 60);
            state.logic(&settings);
            assert_eq!(checks(&fake), 1);
            state.logic(&settings);
            assert_eq!(checks(&fake), 1, "an unacknowledged request is throttled, not recorded as an actual attempt");
            // Roll back far enough that the saved actual attempt remains in the future
            // throughout the short request-coalescing boundary below.
            fake.now.set(900);
            for _ in 0..3 {
                state.logic(&settings);
            }
            assert_eq!(checks(&fake), 2, "future attempt is due, but a future request resets only once after rollback");
            fake.now.set(900 + 59);
            state.logic(&settings);
            assert_eq!(checks(&fake), 2, "unacknowledged requests coalesce for sixty seconds");
            fake.now.set(900 + 60);
            state.logic(&settings);
            assert_eq!(checks(&fake), 3, "an unacknowledged request can be retried after the short coalescing window");
            assert_eq!(state.attempted_at, Some(1000));
            assert_eq!(state.checked_at, Some(500));
            fake.events.borrow_mut().push(status(960, 500, true, None));
            state.logic(&settings);
            assert_eq!(state.attempted_at, Some(960));
            assert_eq!(state.checked_at, Some(500));
            fake.now.set(960 + 6 * 60 * 60 - 1);
            state.logic(&settings);
            assert_eq!(checks(&fake), 3, "a confirmed actual attempt restores the six-hour policy");
            fake.now.set(960 + 6 * 60 * 60);
            state.logic(&settings);
            assert_eq!(checks(&fake), 4);
        }
    }

    #[test]
    fn server_backoff_defers_due_automatic_requests() {
        let fake = FakeBackend::default();
        let retry = 1000 + 8 * 60 * 60;
        fake.now.set(retry - 1);
        fake.events.borrow_mut().push(status(1000, 500, true, Some(retry)));
        let mut state = NightlyState::new(Some(Box::new(fake.clone())), None);
        let settings = Settings { nightly_enabled: true, ..Default::default() };
        state.logic(&settings);
        assert_eq!(checks(&fake), 0);
        fake.now.set(retry);
        state.logic(&settings);
        assert_eq!(checks(&fake), 1);
    }

    #[test]
    fn unusable_storage_errors_never_open_the_destination_without_consent() {
        let fake = FakeBackend::default();
        let mut state = NightlyState::new(Some(Box::new(fake.clone())), None);
        let refusal = "Pre-Release storage is unavailable: the nightly folder contains an alias.";
        fake.events.borrow_mut().push(Event::Notice { app: "photocraft".into(), result: Err(refusal.into()) });
        state.logic(&Settings::default());
        assert!(!state.destination_visible(false), "an error is not consent");
        assert!(state.destination_visible(true));
        assert!(state.notices.iter().any(|notice| notice == refusal), "after consent the page can show the scoped refusal");
    }

    #[test]
    fn off_ignores_in_flight_discoveries_and_blocks_all_remote_actions() {
        let fake = FakeBackend::default();
        fake.now.set(1000);
        let mut state = NightlyState::new(Some(Box::new(fake.clone())), None);
        fake.events.borrow_mut().extend([found(), status(100, 100, false, None), finished(100)]);
        state.logic(&Settings::default());
        assert!(!state.destination_visible(false));
        assert!(state.entries.iter().all(|entry| entry.latest.is_none()));
        assert_eq!(checks(&fake), 0);
        let enabled = Settings { nightly_enabled: true, ..Default::default() };
        fake.events.borrow_mut().push(found());
        state.logic(&enabled);
        let previous = state.entries.iter().find(|entry| entry.app.id == "photocraft").unwrap().latest.clone();
        fake.events.borrow_mut().push(Event::Latest { app: "photocraft".into(), latest: LatestRelease::None, from: Origin::GitHub });
        state.logic(&Settings::default());
        assert_eq!(state.entries.iter().find(|entry| entry.app.id == "photocraft").unwrap().latest, previous);
        fake.sent.borrow_mut().clear();
        state.send(Command::CheckForUpdates { force: true }, false);
        state.send(Command::Install { app: "photocraft".into() }, false);
        state.send(Command::InstallAll { apps: vec!["photocraft".into()] }, false);
        for _ in 0..3 {
            state.logic(&Settings::default());
        }
        assert!(fake.sent.borrow().is_empty());
    }

    #[test]
    fn stale_status_is_visible_in_accessibility_tree_with_distinct_times_and_bounded_failure() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;
        let fake = FakeBackend::default();
        fake.now.set(500);
        let mut value =
            NightlyStatus { attempted_at: Some(440), verified_at: Some(100), retry_at: Some(800), stale: true, message: Some("x".repeat(2000)) };
        fake.events.borrow_mut().push(Event::NightlyStatus { app: "photocraft".into(), status: value.clone() });
        let mut state = NightlyState::new(Some(Box::new(fake)), None);
        state.logic(&Settings::default());
        value = state.statuses["photocraft"].clone();
        assert_eq!(value.message.as_ref().unwrap().len(), 512);
        value.message = Some("offline".into());
        let mut harness = Harness::new_ui(move |ui| {
            crate::i18n::set_current(crate::i18n::Lang::EN);
            nightly_status_labels(ui, &value, 500);
        });
        harness.run_steps(3);
        assert!(harness.query_by_label("Previous verified result is stale. Check again before installing.").is_some());
        assert!(harness.query_by_label("Last verified: 6 min ago").is_some());
        assert!(harness.query_by_label("Last attempt: 1 min ago").is_some());
        assert!(harness.query_by_label("offline").is_some());
        assert!(harness.query_by_label("Retry available in 5 min").is_some());
    }
}

#[cfg(test)]
mod polish_tests {
    use super::*;
    use crate::theme::ThemeChoice;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use std::cell::RefCell;
    use std::rc::Rc;

    const CURRENT: &str = "0.23.1-nightly.20261121.20261121033640.1.gf0e5170d529c";
    const OLDER: &str = "0.23.1-nightly.20261120.20261120211929.1.gace4bfc68195";
    const EARLIER_SAME_DAY: &str = "0.23.1-nightly.20261121.20261121033639.1.gf0e5170d529c";
    type Commands = Rc<RefCell<Vec<Command>>>;
    struct Host;
    impl crate::Host for Host {}

    #[derive(Clone)]
    struct Card {
        // None is no copy; Some(None) is a copy whose version could not be read.
        installed: Option<Option<&'static str>>,
        latest: Option<LatestRelease>,
        installable: Option<bool>,
        enabled: bool,
        status: Option<NightlyStatus>,
    }
    fn release(tag: &str) -> LatestRelease {
        LatestRelease::Found(artcraft_launcher_core::Release {
            tag: format!("v{tag}"),
            version: artcraft_launcher_core::version::parse(tag),
            prerelease: true,
            ..Default::default()
        })
    }
    fn current() -> Card {
        Card {
            installed: Some(Some(CURRENT)),
            latest: Some(release(CURRENT)),
            installable: Some(false),
            enabled: true,
            status: Some(NightlyStatus { attempted_at: Some(1000), verified_at: Some(1000), ..Default::default() }),
        }
    }
    fn page(width: f32, theme: ThemeChoice, card: Card, sent: Commands) -> Harness<'static, Option<LauncherUi>> {
        let mut harness = Harness::builder().with_size(egui::vec2(width, 900.0)).build_ui_state(
            move |ui, state: &mut Option<LauncherUi>| {
                let launcher = state.get_or_insert_with(|| {
                    let stable = super::tests::Recorder(Rc::new(RefCell::new(Vec::new())));
                    let mut launcher = LauncherUi::new(
                        ui.ctx(),
                        Box::new(stable),
                        Settings { language: "en".into(), notify: false, theme, nightly_enabled: card.enabled, ..Default::default() },
                    );
                    launcher.nightly.backend = Some(Box::new(super::tests::Recorder(sent.clone())));
                    launcher.nightly.active = true;
                    if let Some(status) = &card.status {
                        launcher.nightly.statuses.insert("photocraft".into(), status.clone());
                    }
                    let photo = launcher.nightly.entries.iter_mut().find(|entry| entry.app.id == "photocraft").unwrap();
                    photo.detected = true;
                    photo.latest = card.latest.clone();
                    photo.installable = card.installable;
                    photo.installed = card.installed.map(|version| artcraft_launcher_core::Installed {
                        version: version.and_then(artcraft_launcher_core::version::parse),
                        source: artcraft_launcher_core::InstallSource::Launcher,
                        location: "/apps/PhotoCraft Nightly".into(),
                        package: None,
                        externally_managed: false,
                        origin: None,
                        installation: None,
                    });
                    launcher
                });
                launcher.logic(&ui.ctx().clone(), &mut Host);
                launcher.ui(ui, &mut Host);
            },
            None,
        );
        harness.run_steps(5);
        harness
    }

    #[test]
    fn install_action_distinguishes_current_new_older_blocked_and_unknown_copies() {
        let mut stale_cache = current();
        if let Some(LatestRelease::Found(release)) = &mut stale_cache.latest {
            release.version = artcraft_launcher_core::version::parse(OLDER);
        }
        let cases = [
            ("current", current(), None),
            ("immutable tag overrides stale cache", stale_cache, None),
            ("missing", Card { installed: None, installable: Some(true), ..current() }, Some(true)),
            ("older", Card { installed: Some(Some(OLDER)), installable: Some(true), ..current() }, Some(true)),
            ("earlier same day", Card { installed: Some(Some(EARLIER_SAME_DAY)), installable: Some(true), ..current() }, Some(true)),
            ("blocked newer build", Card { installed: Some(Some(OLDER)), ..current() }, Some(false)),
            ("no verified status", Card { installed: Some(Some(OLDER)), installable: Some(true), status: None, ..current() }, Some(false)),
            (
                "stale verified status",
                Card {
                    installed: Some(Some(OLDER)),
                    installable: Some(true),
                    status: Some(NightlyStatus { verified_at: Some(1000), stale: true, ..Default::default() }),
                    ..current()
                },
                Some(false),
            ),
            ("non-nightly installed version", Card { installed: Some(Some("0.23.1")), ..current() }, Some(false)),
            ("unknown installed version", Card { installed: Some(None), ..current() }, Some(false)),
            ("unknown latest version", Card { latest: Some(release("invalid")), ..current() }, Some(false)),
            ("failed lookup", Card { latest: Some(LatestRelease::Failed("offline".into())), ..current() }, Some(false)),
            ("no available release", Card { latest: Some(LatestRelease::None), ..current() }, Some(false)),
            ("pending lookup", Card { latest: None, ..current() }, Some(false)),
            ("OFF with cached newer build", Card { installed: Some(Some(OLDER)), installable: Some(true), enabled: false, ..current() }, None),
        ];
        for theme in [ThemeChoice::Light, ThemeChoice::Dark] {
            for width in [560.0, 1180.0] {
                for (name, card, install) in &cases {
                    let sent = Rc::new(RefCell::new(Vec::new()));
                    let mut harness = page(width, theme, card.clone(), sent.clone());
                    let button = harness.query_by_label("Install pre-release");
                    if let Some(enabled) = install {
                        let button = button.unwrap_or_else(|| panic!("missing install action: {name}"));
                        assert_eq!(!button.accesskit_node().is_disabled(), *enabled, "{name}");
                        if *enabled {
                            button.focus();
                            harness.key_press(egui::Key::Enter);
                            harness.run_steps(3);
                            assert!(sent.borrow().iter().any(|cmd| matches!(cmd, Command::Install { app } if app == "photocraft")), "{name}");
                        }
                    } else {
                        assert!(button.is_none(), "redundant or OFF install action: {name}");
                    }
                    if card.installed.is_some() {
                        harness.get_by_label("Open");
                        harness.get_by_label("Show installed copy");
                        harness.get_by_label("Uninstall pre-release…");
                    }
                }
            }
        }
    }

    #[test]
    fn uninstall_modal_keeps_safe_keyboard_confirmation_and_responsive_layout() {
        for theme in [ThemeChoice::Light, ThemeChoice::Dark] {
            for width in [560.0, 1180.0] {
                let mut consent = page(width, theme, current(), Rc::new(RefCell::new(Vec::new())));
                consent.state_mut().as_mut().unwrap().open_nightly_consent(egui::Id::unique("polish-consent-reference"));
                consent.run_steps(3);
                let consent_title = consent.get_by_label("Enable pre-release builds?").rect();
                let consent_frame = consent.ctx.memory(|memory| memory.area_rect(egui::Id::unique("nightly-consent-v1"))).unwrap();
                let consent_padding = consent_title.min - consent_frame.min;
                for enabled in [false, true] {
                    let sent = Rc::new(RefCell::new(Vec::new()));
                    let mut harness = page(width, theme, Card { enabled, ..current() }, sent.clone());
                    for cancel_key in [egui::Key::Escape, egui::Key::Enter] {
                        harness.get_by_label("Uninstall pre-release…").focus();
                        harness.key_press(egui::Key::Enter);
                        harness.run_steps(3);
                        let title = harness.get_by_label("Uninstall pre-release?").rect();
                        let body = harness.get_by_label("Only this pre-release copy will be removed. Stable copies and user data are kept.").rect();
                        let cancel = harness.get_by_label("Cancel");
                        let frame = harness.ctx.memory(|memory| memory.area_rect(egui::Id::unique("nightly-uninstall-v1"))).unwrap();
                        let padding = title.min - frame.min;
                        assert!((padding - consent_padding).length() < 1.0, "uninstall uses the consent modal's padded frame");
                        assert_eq!(title.height(), consent_title.height(), "both confirmations use the same title typography");
                        assert!(cancel.accesskit_node().is_focused());
                        assert!(title.left() > 0.0 && title.right() < width, "heading stays inside the viewport");
                        assert!(title.bottom() < body.top() && body.bottom() < cancel.rect().top(), "dialog sections have breathing room");
                        assert!(body.left() > 0.0 && body.right() < width, "explanation wraps inside the viewport");
                        assert!(harness.query_by_role(egui::accesskit::Role::Dialog).is_some());
                        harness.key_press(cancel_key);
                        harness.run_steps(3);
                        assert!(harness.state().as_ref().unwrap().nightly.uninstall.is_none());
                        assert!(harness.get_by_label("Uninstall pre-release…").accesskit_node().is_focused());
                        assert!(!sent.borrow().iter().any(|cmd| matches!(cmd, Command::Uninstall { .. })));
                        assert!(harness.state().as_ref().unwrap().nightly.entries.iter().any(|entry| entry.installed.is_some()));
                    }
                    harness.get_by_label("Uninstall pre-release…").focus();
                    harness.key_press(egui::Key::Enter);
                    harness.run_steps(3);
                    harness.key_press(egui::Key::Tab);
                    harness.run_steps(2);
                    assert!(harness.get_by_label("Uninstall pre-release").accesskit_node().is_focused());
                    harness.key_press(egui::Key::Enter);
                    harness.run_steps(3);
                    assert_eq!(sent.borrow().iter().filter(|cmd| matches!(cmd, Command::Uninstall { app } if app == "photocraft")).count(), 1);
                    assert_eq!(harness.state().as_ref().unwrap().settings.nightly_enabled, enabled);
                }
            }
        }
    }
    #[test]
    fn shared_selection_pre_release_destination_lists_only_explicit_channel_apps_on_every_host() {
        for os in [artcraft_launcher_core::Os::Windows, artcraft_launcher_core::Os::Linux, artcraft_launcher_core::Os::MacOs] {
            let state = NightlyState::new(None, Some(Platform { os, arch: artcraft_launcher_core::Arch::X64 }));
            assert_eq!(state.entries.iter().map(|entry| entry.app.id).collect::<Vec<_>>(), vec!["photocraft"]);
        }
    }
    #[test]
    fn a_running_install_shows_its_progress_instead_of_the_install_action() {
        let sent: Commands = Rc::default();
        let card = Card { installed: None, installable: Some(true), ..current() };
        let mut harness = page(1180.0, ThemeChoice::Light, card, sent);
        assert!(harness.query_by_label("Install pre-release").is_some(), "precondition: an installable build offers the install action");
        let set_job = |harness: &mut Harness<'static, Option<LauncherUi>>, job: Option<JobPhase>| {
            let launcher = harness.state_mut().as_mut().unwrap();
            launcher.nightly.entries.iter_mut().find(|entry| entry.app.id == "photocraft").unwrap().job = job;
        };
        for (phase, caption) in [
            (JobPhase::Downloading { done: 0, total: 0 }, "Downloading…"),
            (JobPhase::Verifying, "Verifying…"),
            (JobPhase::Installing, "Installing…"),
        ] {
            set_job(&mut harness, Some(phase));
            harness.run_steps(3);
            assert!(
                harness.query_by_label("Install pre-release").is_none(),
                "{caption}: the install action must not stay clickable while installing"
            );
            let progress = harness.get_by_label(caption);
            assert_eq!(progress.accesskit_node().role(), egui::accesskit::Role::ProgressIndicator, "{caption}");
        }
        set_job(&mut harness, None);
        harness.run_steps(3);
        assert!(harness.query_by_label("Install pre-release").is_some(), "the action returns when the job ends");
    }
}

#[cfg(test)]
mod trust_gate_tests {
    use super::*;
    use artcraft_launcher_core::protocol::Capabilities;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A nightly context whose platform trust is (or isn't) configured, as Native reports it.
    struct Context {
        available: bool,
        sent: Rc<RefCell<Vec<Command>>>,
        events: Rc<RefCell<Vec<Event>>>,
    }
    impl Backend for Context {
        fn nightly_available(&self) -> bool {
            self.available
        }
        fn send(&mut self, command: Command) {
            self.sent.borrow_mut().push(command);
        }
        fn poll(&mut self) -> Vec<Event> {
            std::mem::take(&mut *self.events.borrow_mut())
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { manage_apps: true, reveal_label: "Show", self_update: false, desktop_shortcuts: false }
        }
        fn platform(&self) -> Option<Platform> {
            None
        }
        fn now(&self) -> i64 {
            1000
        }
    }

    type Sent = Rc<RefCell<Vec<Command>>>;
    type Pending = Rc<RefCell<Vec<Event>>>;

    fn context(available: bool) -> (NightlyState, Sent, Pending) {
        let (sent, events) = (Rc::default(), Rc::default());
        let backend = Context { available, sent: Rc::clone(&sent), events: Rc::clone(&events) };
        (NightlyState::new(Some(Box::new(backend)), None), sent, events)
    }

    fn installed_copy() -> artcraft_launcher_core::Installed {
        artcraft_launcher_core::Installed {
            version: None,
            source: artcraft_launcher_core::InstallSource::Launcher,
            location: "/nightly/photo".into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        }
    }

    fn discovery(sent: &[Command]) -> bool {
        sent.iter()
            .any(|c| matches!(c, Command::Install { .. } | Command::InstallAll { .. } | Command::Replace { .. } | Command::CheckForUpdates { .. }))
    }

    /// With consent but no configured trust nothing is looked for or offered, so nobody
    /// downloads a build only to have the signature check refuse it. Copies already installed
    /// stay manageable.
    #[test]
    fn unconfigured_trust_offers_no_discovery_or_install_but_keeps_management() {
        let (mut state, sent, events) = context(false);
        let settings = Settings { nightly_enabled: true, ..Default::default() };
        assert!(!state.available());
        state.logic(&settings);
        assert!(!discovery(&sent.borrow()), "{:?}", sent.borrow());
        assert!(
            sent.borrow().iter().any(|c| matches!(c, Command::Configure(options) if !options.nightly_enabled)),
            "the context is configured without discovery: {:?}",
            sent.borrow()
        );
        state.send(Command::Install { app: "photocraft".into() }, true);
        state.send(Command::CheckForUpdates { force: true }, true);
        assert!(!discovery(&sent.borrow()));
        events.borrow_mut().extend([
            Event::Latest {
                app: "photocraft".into(),
                latest: LatestRelease::Found(artcraft_launcher_core::Release::default()),
                from: artcraft_launcher_core::protocol::Origin::GitHub,
            },
            Event::Installable { app: "photocraft".into(), installable: true },
        ]);
        state.logic(&settings);
        assert!(state.entries[0].latest.is_none(), "no discovery is shown");
        assert_eq!(state.entries[0].installable, Some(false));
        assert!(!state.destination_visible(true), "no Pre-Release page without a copy to manage");
        state.entries[0].installed = Some(installed_copy());
        assert!(state.destination_visible(true) && state.destination_visible(false), "a retained copy keeps its page");
        sent.borrow_mut().clear();
        for command in [
            Command::Launch { app: "photocraft".into(), args: Vec::new() },
            Command::Reveal { app: "photocraft".into() },
            Command::Uninstall { app: "photocraft".into() },
        ] {
            state.send(command, true);
        }
        assert_eq!(sent.borrow().len(), 3, "Open, Show and Uninstall still reach the backend: {:?}", sent.borrow());
    }

    #[test]
    fn configured_trust_discovers_and_installs_with_consent() {
        let (mut state, sent, events) = context(true);
        let settings = Settings { nightly_enabled: true, ..Default::default() };
        assert!(state.available());
        state.logic(&settings);
        assert!(sent.borrow().iter().any(|c| matches!(c, Command::CheckForUpdates { .. })), "{:?}", sent.borrow());
        assert!(state.destination_visible(true));
        events.borrow_mut().push(Event::Installable { app: "photocraft".into(), installable: true });
        state.logic(&settings);
        assert_eq!(state.entries[0].installable, Some(true));
        state.send(Command::Install { app: "photocraft".into() }, true);
        assert!(sent.borrow().iter().any(|c| matches!(c, Command::Install { .. })));
        // Consent is still needed where trust is configured.
        let (mut off, sent, _) = context(true);
        off.logic(&Settings::default());
        off.send(Command::Install { app: "photocraft".into() }, false);
        assert!(!discovery(&sent.borrow()));
    }

    /// The stable backend, handing out a nightly context with or without configured trust.
    struct Stable(bool);
    impl Backend for Stable {
        fn nightly_context(&self) -> Option<Box<dyn Backend>> {
            Some(Box::new(Context { available: self.0, sent: Rc::default(), events: Rc::default() }))
        }
        fn send(&mut self, _: Command) {}
        fn poll(&mut self) -> Vec<Event> {
            Vec::new()
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { manage_apps: true, reveal_label: "Show", self_update: false, desktop_shortcuts: false }
        }
        fn platform(&self) -> Option<Platform> {
            None
        }
        fn now(&self) -> i64 {
            1000
        }
    }

    struct Host;
    impl crate::Host for Host {}

    fn settings_open(available: bool, nightly_enabled: bool) -> Harness<'static, Option<LauncherUi>> {
        let mut backend = Some(Stable(available));
        let mut harness = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
            move |ui, state: &mut Option<LauncherUi>| {
                if state.is_none()
                    && let Some(backend) = backend.take()
                {
                    let settings = Settings { language: "en".into(), notify: false, nightly_enabled, ..Default::default() };
                    *state = Some(LauncherUi::new(ui.ctx(), Box::new(backend), settings));
                }
                if let Some(launcher) = state {
                    launcher.logic(&ui.ctx().clone(), &mut Host);
                    launcher.ui(ui, &mut Host);
                }
            },
            None,
        );
        harness.run_steps(5);
        harness.get_by_label("Settings").focus();
        harness.key_press(egui::Key::Enter);
        harness.run_steps(3);
        assert!(matches!(harness.state().as_ref().unwrap().dialog, Some(crate::app::Dialog::Settings)));
        harness
    }

    const CHECKBOX: &str = "Discover nightly builds (Pre-Release page)";
    const NOT_YET: &str = "Pre-release builds aren't available on this platform yet.";

    #[test]
    fn settings_says_pre_release_is_not_available_yet_without_configured_trust() {
        let mut harness = settings_open(false, false);
        assert!(harness.get_by_label(CHECKBOX).accesskit_node().is_disabled(), "consent can't be given");
        harness.get_by_label(NOT_YET);
        // By keyboard, as a user reaches it: the Settings sheet scrolls, so a pointer click
        // depends on where the panel happens to be.
        harness.get_by_label(CHECKBOX).focus();
        harness.key_press(egui::Key::Space);
        harness.run_steps(3);
        let state = harness.state().as_ref().unwrap();
        assert!(!state.settings.nightly_enabled && !state.nightly.consent_open);
        assert!(harness.query_by_label("Pre-Release").is_none(), "no Pre-Release destination");
    }

    #[test]
    fn settings_offers_pre_release_with_configured_trust() {
        let harness = settings_open(true, false);
        assert!(!harness.get_by_label(CHECKBOX).accesskit_node().is_disabled());
        assert!(harness.query_by_label(NOT_YET).is_none());
    }

    /// Consent given before the build lost (or never had) trust can still be withdrawn.
    #[test]
    fn an_earlier_choice_can_still_be_turned_off_without_configured_trust() {
        let mut harness = settings_open(false, true);
        assert!(!harness.get_by_label(CHECKBOX).accesskit_node().is_disabled());
        harness.get_by_label(NOT_YET);
        harness.get_by_label(CHECKBOX).focus();
        harness.key_press(egui::Key::Space);
        harness.run_steps(3);
        assert!(!harness.state().as_ref().unwrap().settings.nightly_enabled);
    }
}
