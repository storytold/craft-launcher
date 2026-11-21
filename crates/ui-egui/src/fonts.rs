//! The Fonts page: Craft Font Manager's panel in the launcher's look, and the download consent the
//! Settings dialog shows. Desktop only: the web build has no font store.

use std::sync::Arc;
use std::time::Duration;

use crate::settings::FontSettings;
use crate::theme::{self, Tokens};
use craft_font_manager::craft_font_catalog::Watch;
use craft_font_manager::store::{Installer, NoStore, SuiteStore};
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

pub(crate) struct FontsTab {
    catalog: Arc<Catalog>,
    installer: Arc<dyn Installer>,
    /// Built when the page first shows: reading the index and scanning the installed fonts wait
    /// until then.
    panel: Option<FontsPanel>,
    /// What the panel is built from, when a test supplies it.
    options: Option<Options>,
    consent: Option<(bool, f64)>,
    /// Repaints the page when the catalog changes while the window is idle.
    watch: Option<Watch>,
}

impl FontsTab {
    /// The process's font catalog and the user's font store.
    pub(crate) fn new() -> FontsTab {
        let catalog = Arc::clone(Catalog::global());
        let installer: Arc<dyn Installer> = match SuiteStore::for_user(Arc::clone(&catalog)) {
            Some(store) => Arc::new(store),
            None => Arc::new(NoStore),
        };
        FontsTab { catalog, installer, panel: None, options: None, consent: None, watch: None }
    }

    /// A page built from `options` (tests: a font folder, a fake store and a small index).
    pub(crate) fn with_options(options: Options) -> FontsTab {
        FontsTab {
            catalog: Arc::clone(&options.catalog),
            installer: Arc::clone(&options.installer),
            panel: None,
            options: Some(options),
            consent: None,
            watch: None,
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

    /// Repaints `ctx` whenever the catalog rescans for fonts installed or removed meanwhile.
    fn watch_catalog(&mut self, ctx: &egui::Context) {
        if self.watch.is_none() {
            let ctx = ctx.clone();
            self.watch = Some(self.catalog.watch(CATALOG_WATCH, move |_| ctx.request_repaint()));
        }
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
        let panel = self.panel_mut(settings);
        panel.set_language(language);
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
