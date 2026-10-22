//! The browser backend and entry point.

use std::cell::RefCell;
use std::rc::Rc;

use artcraft_launcher_core::details::{AppDetails, MAX_SCREENSHOT_BYTES, looks_like_jpeg};
use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event, LatestRelease, Origin, Source};
use artcraft_launcher_core::release::{self, Release};
use artcraft_launcher_core::{Arch, Os, Platform, SUITE, catalog, version};
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings, i18n};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

const SETTINGS_KEY: &str = "settings";

/// Tell the UI the browser's languages (`?lang=ru` in the address first, as `ARTCRAFT_LAUNCHER_LOCALE`
/// does on the desktop) before the first frame, so the Automatic interface language follows the
/// browser; and show the loading text in that language.
fn detect_language() {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return,
    };
    let document = match window.document() {
        Some(d) => d,
        None => return,
    };
    let mut tags = Vec::new();

    // ?lang=xx in the URL takes priority.
    if let Ok(href) = window.location().href() {
        tags.extend(crate::lang_params(&href));
    }

    // Then the browser's preferred languages.
    let navigator = window.navigator();
    let langs: js_sys::Array = navigator.languages();
    for i in 0..langs.length() {
        if let Some(v) = langs.get(i).as_string() {
            tags.push(v);
        }
    }
    if let Some(lang) = navigator.language() {
        tags.push(lang);
    }
    i18n::set_system_locales(&tags);

    // Screen readers and spell-check follow the page language: match <html lang>
    // to what was detected (`en` when the browser asks for nothing supported).
    if let Some(root) = document.document_element() {
        let _ = root.set_attribute("lang", i18n::system_lang().code());
    }

    // Update loading text in the detected language.
    if let Some(el) = document.get_element_by_id("launcher_loading") {
        el.set_text_content(Some(i18n::tr(i18n::system_lang(), "Loading ArtCraft Launcher…")));
    }
}
pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Warn).ok();
    // Detect language before starting the UI.
    detect_language();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
        let Some(canvas) = document.get_element_by_id("launcher_canvas").and_then(|c| c.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("no canvas");
            return;
        };
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| {
                    let ctx = cc.egui_ctx.clone();
                    let settings: Settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
                    let backend = WebBackend::new(ctx.clone());
                    Ok(Box::new(WebApp { ui: LauncherUi::new(&ctx, Box::new(backend), settings) }))
                }),
            )
            .await;
        if let Some(loading) = document.get_element_by_id("launcher_loading") {
            loading.remove();
        }
        if let Err(e) = result {
            log::error!("couldn't start: {e:?}");
        }
    });
}

/// The browser: nothing of the desktop's, but it knows whether the visitor asked for fewer animations.
struct WebHost;

impl Host for WebHost {
    fn reduce_motion(&self) -> bool {
        web_sys::window().and_then(|w| w.match_media("(prefers-reduced-motion: reduce)").ok().flatten()).is_some_and(|query| query.matches())
    }
}

struct WebApp {
    ui: LauncherUi,
}

impl eframe::App for WebApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui.logic(ctx, &mut WebHost);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui.ui(ui, &mut WebHost);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.ui.raw_input(ctx, raw_input);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.ui.settings);
    }
}

struct WebBackend {
    events: Rc<RefCell<Vec<Event>>>,
    ctx: egui::Context,
    platform: Option<Platform>,
    checking: Rc<RefCell<bool>>,
    prereleases: bool,
    /// Apps whose details pictures are on their way (a second request meanwhile does nothing).
    pictures_busy: Rc<RefCell<std::collections::HashSet<&'static str>>>,
}

impl WebBackend {
    fn new(ctx: egui::Context) -> Self {
        Self { events: Rc::default(), ctx, platform: visitor_platform(), checking: Rc::default(), prereleases: false, pictures_busy: Rc::default() }
    }

    /// Fetches the pictures of an app's details page, lead first. The browser keeps them in its own
    /// cache, so there is nothing to save here.
    fn fetch_screenshots(&self, app: &str) {
        let Some(details) = AppDetails::by_id(app) else { return };
        if !self.pictures_busy.borrow_mut().insert(details.id) {
            return;
        }
        let (events, ctx, busy) = (self.events.clone(), self.ctx.clone(), self.pictures_busy.clone());
        wasm_bindgen_futures::spawn_local(async move {
            // Once the network has failed the rest would fail the same way: say so at once.
            let mut offline: Option<String> = None;
            for shot in details.shots {
                let result = match (&offline, details.screenshot_url(shot.key)) {
                    (Some(why), _) => Err(why.clone()),
                    (None, Some(url)) => picture(&url).await.map_err(|failure| match failure {
                        PictureError::Network(why) => {
                            offline = Some(why.clone());
                            why
                        }
                        PictureError::Picture(why) => why,
                    }),
                    (None, None) => Err("this page has no such picture".to_string()),
                };
                events.borrow_mut().push(Event::Screenshot { app: details.id.to_string(), key: shot.key.to_string(), result });
                ctx.request_repaint();
            }
            busy.borrow_mut().remove(details.id);
        });
    }

    fn check(&self) {
        if self.checking.replace(true) {
            return;
        }
        let events = self.events.clone();
        let ctx = self.ctx.clone();
        let checking = self.checking.clone();
        let pre = self.prereleases;
        wasm_bindgen_futures::spawn_local(async move {
            events.borrow_mut().push(Event::CheckStarted);
            let mut limited = None;
            for app in &SUITE {
                let latest = latest(app.repo, pre).await;
                if let Err(Some(reset)) = &latest {
                    limited = Some(*reset);
                }
                let latest = latest.unwrap_or_else(|e| {
                    LatestRelease::Failed(if e.is_some() { "GitHub's hourly limit was reached".into() } else { "couldn't reach GitHub".into() })
                });
                events.borrow_mut().push(Event::Latest { app: app.id.to_string(), latest, from: Origin::GitHub });
                ctx.request_repaint();
            }
            if let Ok(LatestRelease::Found(r)) = latest(catalog::LAUNCHER_REPO, false).await
                && let (Some(l), Some(c)) = (r.version.as_ref(), version::parse(env!("CARGO_PKG_VERSION")))
                && version::is_newer(l, &c)
            {
                events.borrow_mut().push(Event::LauncherUpdate(r));
            }
            let at = (js_sys::Date::now() / 1000.0) as i64;
            events.borrow_mut().push(Event::CheckFinished { at, source: Source::GitHubApi, rate_limited_until: limited });
            *checking.borrow_mut() = false;
            ctx.request_repaint();
        });
    }
}

/// Why a picture couldn't be had.
enum PictureError {
    /// This picture only: it isn't published, or what came back isn't a picture.
    Picture(String),
    /// The network or the server: the other pictures would fail the same way.
    Network(String),
}

/// One picture of a details page. The size cap is checked on the announced length first and again
/// on what arrived; the bytes must start like a JPEG.
async fn picture(url: &str) -> Result<Vec<u8>, PictureError> {
    let offline = |what: &str| PictureError::Network(format!("couldn't load a picture ({what})"));
    let window = web_sys::window().ok_or_else(|| offline("no window"))?;
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_mode(web_sys::RequestMode::Cors);
    let request = web_sys::Request::new_with_str_and_init(url, &init).map_err(|_| offline("bad request"))?;
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| offline("no connection"))?
        .dyn_into()
        .map_err(|_| offline("no answer"))?;
    match resp.status() {
        200 => {}
        404 => return Err(PictureError::Picture("this picture isn't published".into())),
        status => return Err(PictureError::Network(format!("the server answered HTTP {status}"))),
    }
    let too_big = || PictureError::Picture(format!("a picture of more than {MAX_SCREENSHOT_BYTES} bytes"));
    let announced = resp.headers().get("content-length").ok().flatten().and_then(|n| n.parse::<usize>().ok());
    if announced.is_some_and(|n| n > MAX_SCREENSHOT_BYTES) {
        return Err(too_big());
    }
    let buffer = JsFuture::from(resp.array_buffer().map_err(|_| offline("unreadable"))?).await.map_err(|_| offline("unreadable"))?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    if bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(too_big());
    }
    if !looks_like_jpeg(&bytes) {
        return Err(PictureError::Picture("the answer isn't a JPEG picture".into()));
    }
    Ok(bytes)
}

/// The latest release; `Err(Some(reset))` when rate-limited, `Err(None)` on other failures.
async fn latest(repo: &str, prereleases: bool) -> Result<LatestRelease, Option<i64>> {
    let url = if prereleases {
        format!("https://api.github.com/repos/{repo}/releases?per_page=30")
    } else {
        format!("https://api.github.com/repos/{repo}/releases/latest")
    };
    let window = web_sys::window().ok_or(None)?;
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_mode(web_sys::RequestMode::Cors);
    let request = web_sys::Request::new_with_str_and_init(&url, &init).map_err(|_| None)?;
    let _ = request.headers().set("Accept", "application/vnd.github+json");
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_request(&request)).await.map_err(|_| None)?.dyn_into().map_err(|_| None)?;
    match resp.status() {
        200 => {}
        404 => return Ok(LatestRelease::None),
        403 | 429 => {
            let reset = resp.headers().get("x-ratelimit-reset").ok().flatten().and_then(|r| r.parse().ok());
            return Err(Some(reset.unwrap_or(0)));
        }
        _ => return Err(None),
    }
    let text = JsFuture::from(resp.text().map_err(|_| None)?).await.map_err(|_| None)?.as_string().ok_or(None)?;
    let found: Option<Release> = if prereleases {
        release::parse_github_release_list(&text, repo, true).map_err(|_| None)?
    } else {
        Some(release::parse_github_release(&text, repo).map_err(|_| None)?)
    };
    Ok(found.map_or(LatestRelease::None, LatestRelease::Found))
}

/// The visitor's OS and CPU, as far as the browser says.
fn visitor_platform() -> Option<Platform> {
    let nav = web_sys::window()?.navigator();
    let ua = nav.user_agent().ok()?.to_lowercase();
    let os = if ua.contains("windows") {
        Os::Windows
    } else if ua.contains("mac os") || ua.contains("macintosh") {
        Os::MacOs
    } else if ua.contains("freebsd") {
        Os::FreeBsd
    } else if ua.contains("linux") && !ua.contains("android") {
        Os::Linux
    } else {
        return None;
    };
    let arch = if ua.contains("aarch64") || ua.contains("arm64") || ua.contains("armv8") {
        Arch::Arm64
    } else if ua.contains("riscv") {
        Arch::Riscv64
    } else {
        Arch::X64
    };
    Some(Platform { os, arch })
}

impl Backend for WebBackend {
    fn send(&mut self, command: Command) {
        match command {
            Command::CheckForUpdates { .. } => self.check(),
            Command::FetchScreenshots { app } => self.fetch_screenshots(&app),
            Command::Configure(o) => {
                let changed = o.prereleases != self.prereleases;
                self.prereleases = o.prereleases;
                if changed {
                    self.check();
                }
            }
            _ => {}
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: false, reveal_label: "", self_update: false }
    }

    fn platform(&self) -> Option<Platform> {
        self.platform
    }

    fn now(&self) -> i64 {
        (js_sys::Date::now() / 1000.0) as i64
    }
}
