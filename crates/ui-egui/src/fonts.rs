//! The Fonts page: Craft Font Manager's panel in the launcher's look, and the download consent and
//! preview settings the Settings dialog shows. Desktop only: the web build has no font store.

use std::sync::Arc;
use std::time::Duration;

use crate::i18n::fmt;
use crate::settings::FontSettings;
use crate::theme::{self, Tokens};
use craft_font_manager::craft_font_catalog::Watch;
use craft_font_manager::store::{Installer, NoStore, PREVIEW_SERVICE_ENV, PreviewServiceSource, SuiteStore};
use craft_font_manager::{Catalog, FontsPanel, Look, Options, Palette, Preset, Runner};

fn to_panel(settings: &FontSettings) -> craft_font_manager::Settings {
    let preset = match settings.preset.as_str() {
        "alphabet" => Preset::Alphabet,
        "numerals" => Preset::Numerals,
        _ => Preset::Sentence,
    };
    craft_font_manager::Settings {
        sample: settings.sample.clone(),
        preset,
        size: f32::from(settings.size.clamp(8, 200)),
        hide_download_note: settings.hide_download_note,
    }
}

fn from_panel(panel: &craft_font_manager::Settings) -> FontSettings {
    let preset = match panel.preset {
        Preset::Sentence => "sentence",
        Preset::Alphabet => "alphabet",
        Preset::Numerals => "numerals",
    };
    FontSettings {
        sample: panel.sample.clone(),
        preset: preset.into(),
        size: panel.size.clamp(8.0, 200.0).round() as u16,
        hide_download_note: panel.hide_download_note,
    }
}

/// How often the consent is read again from the font store's settings file, which every Craft
/// app shares.
const CONSENT_RECHECK_SECS: f64 = 3.0;

/// How often the catalog looks for fonts another Craft app installed or removed, so the page
/// shows them without input.
const CATALOG_WATCH: Duration = Duration::from_secs(2);

/// The suite's font preview server, used while neither `CRAFT_FONT_PREVIEW_URL` nor the user sets
/// one. The store keeps it as the host's default and never saves it as the user's choice.
const DEFAULT_PREVIEW_SERVICE: &str = "https://fonts.artcraft.build";

/// The preview settings in Settings, as shown and typed.
struct PreviewField {
    text: String,
    /// Where the server in effect comes from; the environment's can't be edited.
    source: PreviewServiceSource,
    enabled: bool,
    /// Why the last change wasn't saved, from the font store.
    error: Option<String>,
    read_at: f64,
}

pub(crate) struct FontsTab {
    catalog: Arc<Catalog>,
    installer: Arc<dyn Installer>,
    /// Built when the page first shows: reading the index and scanning the installed fonts wait
    /// until then.
    panel: Option<FontsPanel>,
    /// What the panel is built from, when a test supplies it.
    options: Option<Options>,
    consent: Option<(bool, f64)>,
    preview: Option<PreviewField>,
    /// Repaints the page when the catalog changes while the window is idle.
    watch: Option<Watch>,
    /// The sans-serif and system-language filters are set once, when the page first shows.
    default_filters: bool,
}

/// The launcher's defaults for the shared store: previews from the default server, and downloads
/// allowed. Neither is saved, so a choice the user made, in this app or another, still wins.
fn with_launcher_defaults(store: SuiteStore) -> SuiteStore {
    store.with_default_preview_service(DEFAULT_PREVIEW_SERVICE).with_default_network(craft_font_store::Network::On)
}

impl FontsTab {
    /// The process's font catalog and the user's font store.
    pub(crate) fn new() -> FontsTab {
        let catalog = Arc::clone(Catalog::global());
        let installer: Arc<dyn Installer> = match SuiteStore::for_user(Arc::clone(&catalog)) {
            Some(store) => Arc::new(with_launcher_defaults(store)),
            None => Arc::new(NoStore),
        };
        FontsTab { catalog, installer, panel: None, options: None, consent: None, preview: None, watch: None, default_filters: false }
    }

    /// A page built from `options` (tests: a font folder, a fake store and a small index).
    pub(crate) fn with_options(options: Options) -> FontsTab {
        FontsTab {
            catalog: Arc::clone(&options.catalog),
            installer: Arc::clone(&options.installer),
            panel: None,
            options: Some(options),
            consent: None,
            preview: None,
            watch: None,
            default_filters: false,
        }
    }

    /// The download consent, read at most every few seconds.
    pub(crate) fn downloads_allowed(&mut self, now: f64) -> bool {
        match self.consent {
            Some((allowed, at)) if (0.0..CONSENT_RECHECK_SECS).contains(&(now - at)) => allowed,
            _ => {
                let allowed = self.installer.downloads_allowed();
                self.consent = Some((allowed, now));
                allowed
            }
        }
    }

    /// Saves the consent; once the page is built it goes through the panel, which reads the
    /// consent again at once instead of after its own recheck.
    pub(crate) fn set_downloads_allowed(&mut self, allowed: bool) -> Result<(), String> {
        self.consent = None;
        let result = match self.panel.as_mut() {
            Some(panel) => panel.set_downloads_allowed(allowed),
            None => self.installer.allow_downloads(allowed),
        };
        result.map_err(|e| e.to_string())
    }

    /// The server the user saved, or the environment's; empty while the default (shown as the
    /// field's placeholder) or none is in effect.
    fn typed_preview_service(&self, source: PreviewServiceSource) -> Option<String> {
        match source {
            PreviewServiceSource::Env | PreviewServiceSource::Saved => self.installer.preview_service(),
            PreviewServiceSource::Default | PreviewServiceSource::None => None,
        }
    }

    fn read_preview_field(&self, now: f64) -> PreviewField {
        let source = self.installer.preview_service_source();
        PreviewField {
            text: self.typed_preview_service(source).unwrap_or_default(),
            source,
            enabled: self.installer.previews_enabled(),
            error: None,
            read_at: now,
        }
    }

    /// The panel reads the preview settings again on its next frame.
    fn preview_settings_changed(&mut self, now: f64) {
        if let Some(panel) = self.panel.as_mut() {
            panel.reload_preview_settings();
        }
        self.preview = Some(self.read_preview_field(now));
    }

    fn preview_error(&mut self, error: String) {
        if let Some(field) = self.preview.as_mut() {
            field.error = Some(error);
        }
    }

    /// Saves the typed preview server in the font store, which every Craft app shares; empty
    /// clears it. The store checks the address.
    fn apply_preview_server(&mut self, now: f64) {
        let Some(field) = self.preview.as_ref() else { return };
        let text = field.text.trim();
        let url = (!text.is_empty()).then_some(text);
        let saved = self.typed_preview_service(self.installer.preview_service_source());
        if url.map(|u| u.trim_end_matches('/')) == saved.as_deref() {
            return;
        }
        match self.installer.set_preview_service(url) {
            Ok(()) => self.preview_settings_changed(now),
            Err(e) => self.preview_error(fmt(tl!("Couldn't use this server: {e}"), &[("e", &e.to_string())])),
        }
    }

    /// The previews switch and the preview server field, for the Settings dialog. The address is
    /// saved when the field loses focus or on Enter; both are read again from the store every
    /// few seconds while the field isn't being edited.
    pub(crate) fn preview_settings_ui(&mut self, ui: &mut egui::Ui) {
        let t = Tokens::get(ui.ctx());
        let now = ui.input(|i| i.time);
        let id = ui.make_persistent_id("font-preview-server");
        let focused = ui.memory(|m| m.has_focus(id));
        let stale = self.preview.as_ref().is_none_or(|f| f.error.is_none() && !focused && !(0.0..CONSENT_RECHECK_SECS).contains(&(now - f.read_at)));
        if stale {
            self.preview = Some(self.read_preview_field(now));
        }
        let Some(field) = self.preview.as_mut() else { return };
        let mut enabled = field.enabled;
        let toggled = ui.checkbox(&mut enabled, tl!("Show font previews")).changed();
        let from_env = field.source == PreviewServiceSource::Env;
        let small = |ui: &mut egui::Ui, text: &str, color: egui::Color32| {
            ui.add(egui::Label::new(egui::RichText::new(text).size(12.0).color(color)).wrap());
        };
        // The server address is for people who host their own: collapsed, and held open while the
        // address is refused or fixed by the environment so the reason stays visible.
        let force_open = (field.error.is_some() || from_env).then_some(true);
        let response = egui::CollapsingHeader::new(tl!("Advanced"))
            .id_salt("font-preview-advanced")
            .open(force_open)
            .show(ui, |ui| {
                let label = ui.label(tl!("Font preview server"));
                let edit = egui::TextEdit::singleline(&mut field.text).id(id).hint_text(DEFAULT_PREVIEW_SERVICE).desired_width(f32::INFINITY);
                let response = ui.add_enabled(!from_env, edit).labelled_by(label.id);
                if response.changed() {
                    field.error = None;
                }
                if let Some(error) = &field.error {
                    small(ui, error, t.bad);
                }
                if from_env {
                    small(ui, &fmt(tl!("Set by {name} in the environment."), &[("name", PREVIEW_SERVICE_ENV)]), t.muted);
                }
                small(
                    ui,
                    tl!(
                        "Fonts you haven't installed show their previews from this server, and installs try it first. Empty uses the default server."
                    ),
                    t.muted,
                );
                response
            })
            .body_returned;
        if toggled {
            match self.installer.set_previews_enabled(enabled) {
                // Only the switch changed: a refused address typed meanwhile stays, with its error.
                Ok(()) => {
                    field.enabled = enabled;
                    if let Some(panel) = self.panel.as_mut() {
                        panel.reload_preview_settings();
                    }
                }
                Err(e) => self.preview_error(fmt(tl!("Couldn't change the font previews setting: {e}"), &[("e", &e.to_string())])),
            }
        }
        if response.is_some_and(|r| r.lost_focus()) {
            self.apply_preview_server(now);
        }
    }

    /// Repaints `ctx` whenever the catalog rescans for fonts installed or removed meanwhile.
    fn watch_catalog(&mut self, ctx: &egui::Context) {
        if self.watch.is_none() {
            let ctx = ctx.clone();
            self.watch = Some(self.catalog.watch(CATALOG_WATCH, move |_| ctx.request_repaint()));
        }
    }

    /// Stops the catalog watch while another page is drawn; the next [`FontsTab::ui`] restarts it.
    pub(crate) fn hidden(&mut self) {
        self.watch = None;
    }

    /// The panel, built on the first call: reading the indexes and scanning the installed fonts
    /// wait until then.
    fn panel_mut(&mut self, settings: &FontSettings) -> &mut FontsPanel {
        self.panel.get_or_insert_with(|| {
            let options = self.options.take().unwrap_or_else(|| {
                self.catalog.scan_in_background();
                Options {
                    catalog: Arc::clone(&self.catalog),
                    installer: Arc::clone(&self.installer),
                    index: self.installer.indexes().map_err(|e| e.to_string()),
                    runner: Runner::Thread,
                }
            });
            let mut panel = FontsPanel::new(options, to_panel(settings));
            panel.set_look(look());
            // The launcher's Settings has the download switch.
            panel.show_downloads_switch(false);
            panel
        })
    }

    /// Draws the page into `ui`.
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, settings: &mut FontSettings, language: &str) {
        self.watch_catalog(ui.ctx());
        let first = !std::mem::replace(&mut self.default_filters, true);
        let panel = self.panel_mut(settings);
        panel.set_language(language);
        if first {
            panel.use_default_filters(language);
        }
        panel.ui(ui);
        let saved = from_panel(panel.settings());
        if saved != *settings {
            *settings = saved;
        }
    }

    /// The font sources, for the Settings dialog.
    pub(crate) fn sources_ui(&mut self, ui: &mut egui::Ui, settings: &FontSettings, language: &str) {
        let panel = self.panel_mut(settings);
        panel.set_language(language);
        panel.sources_ui(ui);
    }

    /// The panel, once the page has shown.
    pub(crate) fn panel(&self) -> Option<&FontsPanel> {
        self.panel.as_ref()
    }
}

fn palette(t: &Tokens) -> Palette {
    Palette {
        bg: t.bg,
        sunken: t.sunken,
        ink: t.ink,
        ink_strong: t.ink_strong,
        muted: t.muted,
        faint: t.faint,
        line: t.line,
        line_strong: t.line_strong,
        accent: t.accent,
        accent_ink: t.accent_ink,
        invert_bg: t.invert_bg,
        invert_fg: t.invert_fg,
        good: t.good,
        warn: t.warn,
        bad: t.bad,
    }
}

/// The launcher's colours and faces: Archivo titles, Inter names, upper-case Geist Mono labels
/// and buttons, hard corners.
fn look() -> Look {
    Look {
        dark: Some(palette(&Tokens::DARK)),
        light: Some(palette(&Tokens::LIGHT)),
        display: theme::display(),
        strong: theme::semibold(),
        label: theme::mono(),
        button: theme::mono_bold(),
        caps: true,
        corner: egui::CornerRadius::ZERO,
        control_height: theme::CONTROL_H,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_font_manager::FontEnv;

    fn store_in(dir: &std::path::Path) -> Option<SuiteStore> {
        let env = FontEnv { font_store: Some(dir.to_path_buf()), ..FontEnv::default() };
        SuiteStore::for_user(Arc::new(Catalog::new(env))).map(with_launcher_defaults)
    }

    #[test]
    fn downloads_are_allowed_on_a_fresh_store_and_an_explicit_off_persists() {
        let dir = std::env::temp_dir().join(format!("launcher-fonts-default-network-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let Some(store) = store_in(&dir) else { return };
        assert!(store.downloads_allowed(), "on by default");
        assert!(store.allow_downloads(false).is_ok());
        let Some(again) = store_in(&dir) else { return };
        assert!(!again.downloads_allowed(), "an explicit off is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
