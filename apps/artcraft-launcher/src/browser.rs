//! Opening the window's links with the browser brought in front of it (Linux and FreeBSD).
//!
//! A program started from another only comes in front when it is handed an activation token
//! from the program the user was using: an xdg-activation token under Wayland, a
//! startup-notification id under X11. eframe opens links itself, without one, so the browser
//! opens behind the launcher. Instead each link the window follows ([`take_links`]) waits for a
//! token: the window asks winit for one ([`Browser::pump`]), winit hands it back as a window event
//! (`WithTokens` in `main.rs`), and `xdg-open` is started with it in its environment. A
//! compositor that gives none, or doesn't answer within [`TOKEN_WAIT`], has the link opened
//! without one, as before. Ported from MailCraft's `browser.rs` (#698).
//!
//! Windows and macOS open pages in front already; nothing here runs there.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a link waits for its activation token before it is opened without one.
pub const TOKEN_WAIT: Duration = Duration::from_millis(1000);
/// At most this many links wait at once; more are opened at once, without a token.
const MAX_WAITING: usize = 32;
/// How long `xdg-open` is given to report a failure before it is taken to have opened the page.
const OPENER_WAIT: Duration = Duration::from_secs(2);

/// A link followed and not yet opened, with the token asked for it and when (`None` until asked).
struct Asked<S> {
    url: String,
    token: Option<(S, Instant)>,
}

/// The links waiting for their activation token. `S` is winit's `AsyncRequestSerial` (any
/// comparable value in the tests).
pub struct Browser<S> {
    asked: Mutex<Vec<Asked<S>>>,
    launch: fn(&str, Option<&str>),
}

impl<S: PartialEq> Browser<S> {
    pub fn new(launch: fn(&str, Option<&str>)) -> Self {
        Browser { asked: Mutex::new(Vec::new()), launch }
    }

    fn asked(&self) -> std::sync::MutexGuard<'_, Vec<Asked<S>>> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Links the window followed in this frame.
    pub fn follow(&self, urls: Vec<String>) {
        self.asked().extend(urls.into_iter().map(|url| Asked { url, token: None }));
    }

    /// Every frame, on the window's thread: ask a token for each new link (`request_token` is
    /// `None` when the window system gives none, and the link opens at once), and open those
    /// whose token is overdue without one. Returns how long until the next is overdue.
    pub fn pump(&self, request_token: &mut dyn FnMut() -> Option<S>, now: Instant) -> Option<Duration> {
        let mut now_open = Vec::new();
        let next = {
            let mut asked = self.asked();
            let mut waiting = asked.iter().filter(|a| a.token.is_some()).count();
            let mut kept = Vec::with_capacity(asked.len());
            for mut a in std::mem::take(&mut *asked) {
                match &a.token {
                    None => match (waiting < MAX_WAITING).then(&mut *request_token).flatten() {
                        Some(serial) => {
                            a.token = Some((serial, now));
                            waiting += 1;
                            kept.push(a);
                        }
                        None => now_open.push(a.url),
                    },
                    Some((_, since)) if now.saturating_duration_since(*since) >= TOKEN_WAIT => now_open.push(a.url),
                    Some(_) => kept.push(a),
                }
            }
            *asked = kept;
            asked.iter().filter_map(|a| a.token.as_ref()).map(|(_, since)| TOKEN_WAIT.saturating_sub(now.saturating_duration_since(*since))).min()
        };
        for url in now_open {
            (self.launch)(&url, None);
        }
        next
    }

    /// The window system's answer to a token request: the link that waited for it opens with it.
    pub fn token_arrived(&self, serial: &S, token: &str) {
        let found = {
            let mut asked = self.asked();
            let at = asked.iter().position(|a| a.token.as_ref().is_some_and(|(s, _)| s == serial));
            at.map(|at| asked.remove(at).url)
        };
        if let Some(url) = found {
            (self.launch)(&url, Some(token));
        }
    }
}

/// Take the links the window followed in this frame out of its output, before eframe opens them
/// itself (which would leave the browser behind the window).
pub fn take_links(ctx: &egui::Context) -> Vec<String> {
    ctx.output_mut(|o| {
        let mut urls = Vec::new();
        o.commands.retain(|c| match c {
            egui::OutputCommand::OpenUrl(open) => {
                urls.push(open.url.clone());
                false
            }
            _ => true,
        });
        urls
    })
}

/// Open `url` on a thread of its own (the opener can take a moment to report): `xdg-open` with
/// the token when there is one, the way eframe opened links otherwise or when that fails.
/// (`$BROWSER`, when set, is the user's choice, which the fallback honours.) The address isn't logged.
pub fn launch(url: &str, token: Option<&str>) {
    log::info!("browser: opening a link {} an activation token", if token.is_some() { "with" } else { "without" });
    let (url, token) = (url.to_string(), token.map(str::to_string));
    let work = move || {
        if let Some(token) = token
            && std::env::var_os("BROWSER").is_none()
            && let Some(mut command) = opener_command(&url, &token)
            && xdg_open(&mut command)
        {
            return;
        }
        if let Err(e) = webbrowser::open(&url) {
            log::warn!("couldn't start a browser ({:?})", e.kind());
        }
    };
    if let Err(e) = std::thread::Builder::new().name("browser".into()).spawn(work) {
        log::warn!("couldn't start a browser: no thread for it ({e})");
    }
}

/// Start `xdg-open` and see whether it opened the page: it exits at once on the desktops; where it
/// starts the browser itself it is still running, and is reaped in the background.
fn xdg_open(command: &mut std::process::Command) -> bool {
    let Ok(mut child) = command.spawn() else { return false };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < OPENER_WAIT => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = std::thread::Builder::new().name("browser".into()).spawn(move || child.wait());
                return true;
            }
            Err(_) => return true,
        }
    }
}

/// `xdg-open <url>` with the token in its environment under both the Wayland and the X11 name (on
/// the host through `flatpak-spawn` inside the Flatpak sandbox); `None` for anything but a plain
/// web address, so nothing in an address can be taken for an option.
pub fn opener_command(url: &str, token: &str) -> Option<std::process::Command> {
    use artcraft_launcher_native::process::{command, in_sandbox};
    use std::process::Stdio;
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let web = lower.starts_with("https://") || lower.starts_with("http://");
    let clean = |s: &str| !s.is_empty() && !s.chars().any(|c| c.is_control() || c.is_whitespace());
    if !web || !clean(url) || !clean(token) || token.len() > 4096 {
        return None;
    }
    let vars = [("XDG_ACTIVATION_TOKEN", token), ("DESKTOP_STARTUP_ID", token)];
    let mut cmd = if in_sandbox() {
        let mut c = command("flatpak-spawn");
        c.arg("--host");
        c.args(vars.iter().map(|(k, v)| format!("--env={k}={v}")));
        c.arg("xdg-open");
        c
    } else {
        let mut c = command("xdg-open");
        c.envs(vars);
        c
    };
    cmd.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    Some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    static OPENED: Mutex<Vec<(String, Option<String>)>> = Mutex::new(Vec::new());

    fn record(url: &str, token: Option<&str>) {
        OPENED.lock().unwrap().push((url.to_string(), token.map(str::to_string)));
    }

    fn opened() -> Vec<(String, Option<String>)> {
        std::mem::take(&mut *OPENED.lock().unwrap())
    }

    // One test, as the recorder is shared.
    #[test]
    fn links_wait_for_their_token_or_open_without_one() {
        let browser = Browser::new(record);
        let now = Instant::now();

        // With a token: waits, then opens with it; a late or unknown answer opens nothing.
        browser.follow(vec!["https://a.example/".into()]);
        assert_eq!(browser.pump(&mut || Some(7), now), Some(TOKEN_WAIT));
        assert!(opened().is_empty());
        browser.token_arrived(&8, "other");
        browser.token_arrived(&7, "tok-7");
        browser.token_arrived(&7, "tok-7");
        assert_eq!(opened(), [("https://a.example/".to_string(), Some("tok-7".to_string()))]);

        // No token given: at once. No answer: once the wait is over.
        browser.follow(vec!["https://b.example/".into()]);
        browser.pump(&mut || None, now);
        browser.follow(vec!["https://c.example/".into()]);
        browser.pump(&mut || Some(1), now);
        assert_eq!(browser.pump(&mut || Some(2), now + TOKEN_WAIT / 2), Some(TOKEN_WAIT / 2));
        assert_eq!(browser.pump(&mut || Some(3), now + TOKEN_WAIT), None);
        assert_eq!(opened(), [("https://b.example/".to_string(), None), ("https://c.example/".to_string(), None)]);

        // Past the cap, links don't wait.
        browser.follow((0..40).map(|i| format!("https://d.example/{i}")).collect());
        let mut serial = 10;
        browser.pump(
            &mut || {
                serial += 1;
                Some(serial)
            },
            now,
        );
        assert_eq!(opened().len(), 40 - MAX_WAITING);
        browser.token_arrived(&11, "t11");
        assert_eq!(opened(), [("https://d.example/0".to_string(), Some("t11".to_string()))]);
    }

    #[test]
    fn the_windows_links_are_taken_out_of_its_output() {
        let ctx = egui::Context::default();
        let mut taken = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.ctx().open_url(egui::OpenUrl::new_tab("https://a.example/"));
            ui.ctx().copy_text("kept".into());
            taken = take_links(ui.ctx());
        });
        assert_eq!(taken, ["https://a.example/"]);
        output.textures_delta.clear();
        assert!(matches!(output.platform_output.commands.as_slice(), [egui::OutputCommand::CopyText(t)] if t == "kept"));
    }

    #[test]
    fn the_opener_gets_the_token_and_nothing_an_address_could_misuse() {
        let command = opener_command("https://a.example/x?y=1", "abc_123").unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args.last().unwrap(), &"https://a.example/x?y=1");
        for url in ["-x https://a.example/", "mailto:a@example.org", "file:///etc/passwd", "https://a.example/\nb", "", "ht"] {
            assert!(opener_command(url, "abc").is_none(), "{url:?}");
        }
        assert!(opener_command("https://a.example/", "a b").is_none());
        assert!(opener_command("https://a.example/", "").is_none());
    }
}
