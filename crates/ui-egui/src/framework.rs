//! The craft-ui runtime, hosted from the launcher's own eframe `App` (craft-libs docs/craft-framework.md, section 2.2):
//! the Fonts page is a craft-ui component. Desktop only, as that page is. Each window's context gets [`install`] once,
//! [`raw_input`](crate::LauncherUi::raw_input) in `raw_input_hook`, [`close_path`] last in `logic`, and [`before_ui`] first in `ui`; test
//! windows install with [`install_headless`].
//!
//! The runtime takes Escape for the whole window unless one of the launcher's own widgets has the keyboard or an egui
//! popup is open. [`return_window_escape`] puts the window's Escape back into egui's input, so the launcher's dialogs
//! and its app page read it from egui as they read every other key.

use std::sync::Arc;

use craft_font_manager::CATALOGS;
use craft_host::target::{Backend, ShellInfo};
use craft_host::{AppInfo, Host, MemorySections};
use craft_i18n::{I18n, Lang};
use craft_theme::{ColorToken, ContrastIntent, Mode, ModePins, ModeSeeds, Pins, RadiusSet, Rgba, SeedOverrides, Seeds, ShapeSeed, Status};
use craft_theme::{ThemeDef, ThemeId, Variants};
use craft_ui::fonts::{I18nScripts, SystemFonts, UiFonts};
use craft_ui::runtime::env::{RootEnv, Scale};
use craft_ui::runtime::frame::FrameInput;
use craft_ui::runtime::keys::{Intent, with_window_intents};
use craft_ui::shell::embed::{self, CloseDecision};
use craft_window::system::{NoWindows, SystemWindows, WindowSystem};

use crate::theme::Tokens;

/// Each pass of the window, first in `App::ui`: hands the runtime the native window it answers the hit test of
/// (Windows) and fits the window to its monitor once.
pub use craft_ui::shell::embed::before_ui;

/// The scripts of the interface languages, for the font stack.
static SCRIPTS: I18nScripts = I18nScripts;

fn install_with(ctx: &egui::Context, app_id: &str, shell: ShellInfo, windows: Arc<dyn WindowSystem>) -> Result<(), String> {
    let spec = craft_ui::InstallSpec::new(shell, windows).app_id(app_id);
    // `theme::install_fonts` replaces this first stack before anything is drawn.
    let fonts = UiFonts::new(&SCRIPTS).system_fonts(SystemFonts::Off);
    embed::install(ctx, spec, &fonts).map(drop).map_err(|error| format!("the UI runtime could not be installed: {error}"))
}

/// Installs the runtime on a desktop window's context, once, before the launcher is attached to it. The launcher draws
/// its own title bar, so no frame decision is handed over.
pub fn install(ctx: &egui::Context, app_id: &str) -> Result<(), String> {
    install_with(ctx, app_id, ShellInfo::default().backend(Backend::from_env()), Arc::new(SystemWindows))
}

/// Installs the runtime on a context with no native window of its own: test windows.
pub fn install_headless(ctx: &egui::Context) {
    if let Err(error) = install_with(ctx, "", ShellInfo::default(), Arc::new(NoWindows)) {
        log::warn!("{error}");
    }
}

/// The window's close path, last in `App::logic`: the launcher has decided by then, and a close it refused carries
/// `CancelClose`.
pub fn close_path(ctx: &egui::Context) {
    embed::logic(ctx, || CloseDecision::Close);
}

/// What the runtime and its components read from the launcher.
pub(crate) struct Framework {
    /// The Fonts page's settings section lives in memory: the launcher keeps the same values in its own settings.
    pub(crate) host: Arc<Host>,
    /// The panel's catalogs; the Host's language follows the launcher's (see [`Framework::before_input`]).
    i18n: Arc<I18n>,
    theme: Arc<ThemeDef>,
    /// The interface zoom, which the runtime writes to the context each frame; the context's own (egui saves it with
    /// the window) until the first frame.
    pub(crate) zoom: Option<f32>,
}

impl Framework {
    pub(crate) fn new() -> Framework {
        let app = AppInfo { id: "artcraft-launcher", name: "ArtCraft Launcher", version: env!("CARGO_PKG_VERSION") };
        // Only the languages the panel has words for: a launcher language without a row set shows the panel in English.
        let (i18n, _) = I18n::new(CATALOGS, &["ja"]);
        let i18n = Arc::new(i18n);
        let host = Host::builder(app).settings(Arc::new(MemorySections::default())).i18n(Arc::clone(&i18n)).lang(Lang::EN).build();
        Framework { host: Arc::new(host), i18n, theme: Arc::new(theme()), zoom: None }
    }

    /// The root values of this frame: the launcher's appearance, zoom and language. `lang_code` is the code of the
    /// language the launcher draws in; the Host is installed again only when that language changes.
    pub(crate) fn before_input(&mut self, ctx: &egui::Context, reduce_motion: bool, lang_code: &str) {
        let lang = self.i18n.from_code(lang_code).unwrap_or(Lang::EN);
        if lang != self.host.lang() {
            self.host = Arc::new(self.host.with_lang(lang));
        }
        let zoom = *self.zoom.get_or_insert_with(|| ctx.zoom_factor());
        let mode = if ctx.theme() == egui::Theme::Dark { Mode::Dark } else { Mode::Light };
        let mut root = RootEnv::new().library_theme(Arc::clone(&self.theme)).mode(mode).scale(Scale::new(zoom));
        if reduce_motion {
            root.motion = craft_theme::MotionChoice::Reduced;
        }
        let host = Arc::clone(&self.host);
        embed::before_input(ctx, |_| FrameInput::new(root).host(host));
    }
}

/// Puts the window's Escape (an Escape that neither a craft-ui component nor one of the launcher's widgets has) back
/// into egui's input as a key press.
pub(crate) fn return_window_escape(ctx: &egui::Context) {
    let mut taken = false;
    with_window_intents(ctx, false, |intents| {
        taken = intents.take(Intent::Cancel);
        taken
    });
    if taken {
        ctx.input_mut(|i| {
            let modifiers = i.modifiers;
            let key = egui::Key::Escape;
            i.events.insert(0, egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers });
        });
    }
}

/// `color` as it shows over `on`: theme colours are opaque.
fn over(on: egui::Color32, color: egui::Color32) -> Rgba {
    let [r, g, b, _] = on.blend(color).to_array();
    Rgba::rgb(r, g, b)
}

fn pins(t: &Tokens) -> Pins {
    let on_bg = |color| over(t.bg, color);
    [
        (ColorToken::SurfaceBackground, on_bg(t.bg)),
        (ColorToken::SurfacePanel, on_bg(t.bg)),
        (ColorToken::SurfaceChrome, on_bg(t.bg)),
        (ColorToken::SurfaceRaised, on_bg(t.bg)),
        (ColorToken::SurfaceSunken, on_bg(t.sunken)),
        (ColorToken::TextNormal, on_bg(t.ink)),
        (ColorToken::TextMuted, on_bg(t.muted)),
        (ColorToken::TextFaint, on_bg(t.faint)),
        (ColorToken::LineDivider, on_bg(t.line)),
        (ColorToken::LineBorder, on_bg(t.line)),
        (ColorToken::LineControlEdge, on_bg(t.line_strong)),
        (ColorToken::AccentSolid, on_bg(t.accent)),
        (ColorToken::AccentText, on_bg(t.accent_ink)),
    ]
    .into_iter()
    .collect()
}

fn seeds(t: &Tokens) -> SeedOverrides {
    let solid = |color| Some(over(t.bg, color));
    SeedOverrides {
        neutral: solid(t.bg),
        accent: solid(t.accent),
        status: Status { good: solid(t.good), warn: solid(t.warn), bad: solid(t.bad), info: solid(t.accent) },
    }
}

/// The launcher's colours and hard corners as a craft theme.
fn theme() -> ThemeDef {
    let (dark, light) = (Tokens::DARK, Tokens::LIGHT);
    let solid = |color| over(dark.bg, color);
    ThemeDef {
        id: ThemeId::parse("artcraft-launcher").unwrap_or_else(|_| ThemeId::suite_default()),
        name: "ArtCraft Launcher".into(),
        version: 1,
        seeds: Seeds {
            neutral: solid(dark.bg),
            accent: solid(dark.accent),
            status: Status { good: solid(dark.good), warn: solid(dark.warn), bad: solid(dark.bad), info: solid(dark.accent) },
        },
        mode_seeds: ModeSeeds { light: seeds(&light), dark: seeds(&dark) },
        shape: Some(ShapeSeed {
            radius: Some(RadiusSet::default()),
            radius_offset: None,
            button_radius: None,
            edge: None,
            knob: None,
            wells: None,
            grip: None,
            locator: None,
        }),
        variants: Variants::Both,
        pins: ModePins { light: pins(&light), dark: pins(&dark) },
        high_contrast: ModePins::default(),
        contrast_intent: ContrastIntent::Standard,
        icon_pack: None,
        apps: Default::default(),
    }
}
