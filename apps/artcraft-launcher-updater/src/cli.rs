//! The command line: `check`, `update` and `status`, for people and for scripts.
//!
//! With the ArtCraft Launcher Update Service running, `check` and `update` ask it, so installing
//! an update needs no administrator permission. Without it (or with `--local`) they do the work in
//! this process, the launcher's way: Windows then asks for permission for each app installed for
//! all users.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use artcraft_launcher_core::protocol::{BackendOptions, JobPhase};
use artcraft_launcher_core::{CraftApp, SUITE};
use artcraft_launcher_native::http::Stop;
use artcraft_launcher_native::paths::Paths;
use artcraft_launcher_native::service::{self, AppState, AppSummary, Connection, DISPLAY_NAME, Op, Outcome, Phase, Reply, Status};
use artcraft_launcher_native::updater::{Engine, Found, Scope};
use serde::Serialize;

pub const USAGE: &str = "\
ArtCraft Launcher Updater: checks for and installs updates to the Craft App Suite.

usage: artcraft-launcher-updater <command> [options]

commands:
  check             look for updates to the installed Craft Apps
  update [APP...]   install the available updates: every app's, or only the named apps'
                    (photocraft, wordcraft, ...)
  status            show the ArtCraft Launcher Update Service's settings, last check and results
  set auto-update on|off
                    turn the service's background installs on or off for everyone on this
                    computer (in an administrator terminal; ArtCraft Launcher's \"Automatically
                    download and install updates\" does the same and asks for permission)
  run [--once]      do the update service's work in this console (as an administrator; for
                    testing). --once: one check and its installs, then exit
  service           (how Windows starts the update service)
  help, version

options:
  --local           do the work in this process, not through the update service (Windows then asks
                    for administrator permission for each app installed for all users)
  --json            print the result as JSON (check, update, status)
  --verbose         also print the log as it's written

With the ArtCraft Launcher Update Service installed, check and update go through it, and installing
an update needs no administrator permission. Stopping this command (Ctrl+C) doesn't stop an update
the service has begun.

exit codes: 0 done, or nothing to do; 100 updates are available (check); 1 something failed;
2 bad usage
";

/// `check`'s exit code when updates are available (as `dnf check-update` has it).
const UPDATES_AVAILABLE: u8 = 100;

/// What the command line was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    /// Windows starts the service with this.
    Service,
    /// The service's work in this console.
    Run {
        once: bool,
    },
    Check(Options),
    /// The apps to update (by id); empty: every one with an update.
    Update(Options, Vec<String>),
    Status(Options),
    /// Turn the service's background installs on or off (an administrator's setting).
    SetAutoUpdate(bool),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub json: bool,
    pub local: bool,
    pub verbose: bool,
}

impl Command {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Options::default();
        let mut once = false;
        let mut words = Vec::new();
        for arg in args {
            match arg.as_str() {
                "--json" => options.json = true,
                "--local" => options.local = true,
                "--verbose" | "-v" => options.verbose = true,
                "--once" => once = true,
                "-h" | "--help" => return Ok(Self::Help),
                "-V" | "--version" => return Ok(Self::Version),
                flag if flag.starts_with('-') => return Err(format!("unknown option `{flag}`")),
                word => words.push(word),
            }
        }
        let Some((command, rest)) = words.split_first() else { return Ok(Self::Help) };
        let only = |allowed: bool, what: &str| if allowed { Ok(()) } else { Err(format!("{what} doesn't go with `{command}`")) };
        let no_more = |rest: &[&str]| match rest.first() {
            Some(extra) => Err(format!("`{command}` takes no `{extra}`")),
            None => Ok(()),
        };
        match *command {
            "check" => {
                no_more(rest)?;
                only(!once, "--once")?;
                Ok(Self::Check(options))
            }
            "update" => {
                only(!once, "--once")?;
                Ok(Self::Update(options, apps(rest)?))
            }
            "status" => {
                no_more(rest)?;
                only(!once, "--once")?;
                only(!options.local, "--local")?;
                Ok(Self::Status(options))
            }
            "run" => {
                no_more(rest)?;
                only(!options.local && !options.json, "--local or --json")?;
                Ok(Self::Run { once })
            }
            "service" => {
                no_more(rest)?;
                Ok(Self::Service)
            }
            "set" => {
                only(!once && options == Options::default(), "an option")?;
                match rest {
                    ["auto-update", "on"] => Ok(Self::SetAutoUpdate(true)),
                    ["auto-update", "off"] => Ok(Self::SetAutoUpdate(false)),
                    _ => Err("use `set auto-update on` or `set auto-update off`".into()),
                }
            }
            "help" => Ok(Self::Help),
            "version" => Ok(Self::Version),
            other => Err(format!("unknown command `{other}`")),
        }
    }

    pub fn verbose(&self) -> bool {
        matches!(self, Self::Check(o) | Self::Update(o, _) | Self::Status(o) if o.verbose)
    }
}

/// The ids of the apps named on the command line: by id, name or former name, in any case.
fn apps(words: &[&str]) -> Result<Vec<String>, String> {
    words
        .iter()
        .map(|word| {
            find_app(word).map(|a| a.id.to_string()).ok_or_else(|| {
                let ids: Vec<&str> = SUITE.iter().map(|a| a.id).collect();
                format!("`{word}` isn't a Craft App (they are: {})", ids.join(", "))
            })
        })
        .collect()
}

fn find_app(word: &str) -> Option<&'static CraftApp> {
    SUITE.iter().find(|a| a.stems().chain(a.names()).any(|n| n.eq_ignore_ascii_case(word)))
}

/// An app's display name, from its id.
fn name(id: &str) -> &str {
    CraftApp::by_id(id).map_or(id, |a| a.name)
}

/// Runs `check`, `update`, `status` or `set`.
pub fn run(command: Command) -> ExitCode {
    match command {
        Command::Check(options) => check(options),
        Command::Update(options, apps) => update(options, &apps),
        Command::Status(options) => status(options),
        Command::SetAutoUpdate(on) => set_auto_update(on),
        Command::Help | Command::Version | Command::Service | Command::Run { .. } => ExitCode::from(2),
    }
}

// ---- set ---------------------------------------------------------------------------------------

/// `set auto-update on|off`: the computer's background installs, which only an administrator may
/// change. ArtCraft Launcher runs this, elevated, when "Automatically download and install
/// updates" is ticked or unticked.
fn set_auto_update(on: bool) -> ExitCode {
    match artcraft_launcher_native::policy::set_auto_update(on) {
        Ok(()) => {
            log::info!("background installs turned {} for this computer", if on { "on" } else { "off" });
            println!("Background installs are {} for everyone on this computer.", if on { "on" } else { "off" });
            ExitCode::SUCCESS
        }
        Err(e) => {
            log::warn!("background installs weren't changed: {e}");
            eprintln!("artcraft-launcher-updater: {e}");
            if e.contains("administrator rights") {
                eprintln!(
                    "Run it from an administrator terminal, or change \"Automatically download and install updates\" in ArtCraft Launcher's \
                     settings, which asks for permission."
                );
            }
            ExitCode::FAILURE
        }
    }
}

// ---- check -------------------------------------------------------------------------------------

/// What `check` found, and from where.
#[derive(Debug, Serialize)]
struct Report {
    /// `service` or `local`.
    source: &'static str,
    checked_at: Option<i64>,
    apps: Vec<AppSummary>,
    /// Something worth knowing about how the check went.
    note: Option<String>,
}

fn check(options: Options) -> ExitCode {
    let report = if !options.local && service::running() {
        match service::status(true) {
            Ok(status) => Ok(Report { source: "service", checked_at: status.last_check, apps: status.apps, note: None }),
            // Requests turned off: the service's own last check is still worth showing.
            Err(why) => {
                service::status(false).map(|status| Report { source: "service", checked_at: status.last_check, apps: status.apps, note: Some(why) })
            }
        }
    } else {
        check_here()
    };
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            eprintln!("artcraft-launcher-updater: couldn't check for updates: {e}");
            return ExitCode::FAILURE;
        }
    };
    if options.json {
        print_json(&report);
    } else {
        print_report(&report);
    }
    let installed: Vec<&AppSummary> = report.apps.iter().filter(|a| a.state != AppState::NotInstalled).collect();
    if installed.iter().any(|a| a.state == AppState::UpdateAvailable) {
        ExitCode::from(UPDATES_AVAILABLE)
    } else if !installed.is_empty() && installed.iter().all(|a| a.state == AppState::Unknown) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn check_here() -> Result<Report, String> {
    let engine = Engine::new(Paths::new()?, Scope::User, BackendOptions::default())?;
    let survey = engine.survey();
    Ok(Report { source: "local", checked_at: Some(survey.at), apps: survey.apps.iter().map(Found::summary).collect(), note: None })
}

fn print_report(report: &Report) {
    let now = artcraft_launcher_native::now();
    let when = report.checked_at.map_or_else(|| "not yet".to_string(), |t| artcraft_launcher_core::time::format_ago(t, now));
    match report.source {
        "service" => println!("Checked {when} by the {DISPLAY_NAME}."),
        _ => println!("Checked {when} on this computer (the {DISPLAY_NAME} isn't running)."),
    }
    if let Some(note) = &report.note {
        println!("Note: {note}.");
    }
    println!();
    let (installed, missing): (Vec<&AppSummary>, Vec<&AppSummary>) = report.apps.iter().partition(|a| a.state != AppState::NotInstalled);
    for app in &installed {
        println!("  {:<13} {:<17} {}", name(&app.id), versions(app), describe_state(app));
    }
    if installed.is_empty() {
        println!("  No Craft Apps are installed{}.", if report.source == "service" { " for all users" } else { "" });
    }
    if !missing.is_empty() && !installed.is_empty() {
        let names: Vec<&str> = missing.iter().map(|a| name(&a.id)).collect();
        println!("\nNot installed: {}.", names.join(", "));
    }
    let updates = installed.iter().filter(|a| a.state == AppState::UpdateAvailable).count();
    println!();
    match updates {
        0 => println!("Everything installed is up to date."),
        1 => println!("1 update is available. Install it with: artcraft-launcher-updater update"),
        n => println!("{n} updates are available. Install them with: artcraft-launcher-updater update"),
    }
}

/// `0.8.1 -> 0.8.2` for an update, else the installed version.
fn versions(app: &AppSummary) -> String {
    let installed = app.installed.as_deref().unwrap_or("?");
    match (&app.state, app.latest.as_deref()) {
        (AppState::UpdateAvailable, Some(latest)) => format!("{installed} -> {latest}"),
        _ => installed.to_string(),
    }
}

fn describe_state(app: &AppSummary) -> String {
    let base = match app.state {
        AppState::NotInstalled => "not installed",
        AppState::UpToDate => "up to date",
        AppState::UpdateAvailable => "update available",
        AppState::Unknown => "couldn't check",
        AppState::Blocked => "not updated here",
    };
    match &app.note {
        Some(note) => format!("{base}: {note}"),
        None => base.to_string(),
    }
}

// ---- update ------------------------------------------------------------------------------------

/// How one app's update went, for `--json`.
#[derive(Debug, Serialize)]
struct AppResult {
    app: String,
    outcome: Outcome,
}

/// `update --json`.
#[derive(Serialize)]
struct UpdateReport<'a> {
    results: &'a [AppResult],
}

/// `status --json` when the service isn't answering.
#[derive(Serialize)]
struct NotAnswering {
    running: bool,
    /// What Windows says about the service.
    state: String,
    error: String,
}

fn update(options: Options, apps: &[String]) -> ExitCode {
    let mut progress = Progress::new(options.json);
    let mut results = Vec::new();
    let mut here: Vec<String> = apps.to_vec();
    let mut all_here = true;
    if !options.local && service::running() {
        match update_through_service(apps, &mut progress) {
            Ok(done) => {
                // What the service may not touch (installed for one user, say), this process
                // updates the launcher's way.
                here = done.iter().filter(|r| matches!(r.outcome, Outcome::NotEligible { .. })).map(|r| r.app.clone()).collect();
                results.extend(done.into_iter().filter(|r| !matches!(r.outcome, Outcome::NotEligible { .. })));
                all_here = false;
            }
            Err(why) => progress.note(&format!("{why}; installing here instead")),
        }
    }
    if all_here || !here.is_empty() {
        match update_here(&here, &mut progress) {
            Ok(done) => results.extend(done),
            Err(e) => {
                progress.note(&format!("couldn't update here: {e}"));
                results.push(AppResult { app: String::new(), outcome: Outcome::Failed { error: e } });
            }
        }
    }
    if options.json {
        print_json(&UpdateReport { results: &results });
    } else if results.is_empty() {
        println!("Everything installed is up to date.");
    }
    if results.iter().all(|r| r.outcome.is_success()) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

/// `update` through the service. `Err` when it refused (the launcher's own way still works).
fn update_through_service(apps: &[String], progress: &mut Progress) -> Result<Vec<AppResult>, String> {
    let mut conn = Connection::open()?;
    conn.send(Op::Update { apps: apps.to_vec() })?;
    progress.note(&format!("Installing through the {DISPLAY_NAME}."));
    let mut results = Vec::new();
    loop {
        match conn.wait() {
            Ok(Reply::Progress { app, phase }) => progress.show(&app, phase),
            Ok(Reply::Finished { app, outcome }) => {
                // What the service may not touch is tried here next, which says how it went.
                if !matches!(outcome, Outcome::NotEligible { .. }) {
                    progress.done(&outcome.describe(name(&app)));
                }
                results.push(AppResult { app, outcome });
            }
            Ok(Reply::Refused { reason }) if results.is_empty() => return Err(reason),
            Ok(Reply::Refused { reason }) => progress.note(&reason),
            Ok(Reply::Working | Reply::Status(_)) => {}
            Ok(Reply::Done) => return Ok(results),
            Err(e) => {
                progress.done(&format!("{DISPLAY_NAME}: {e}"));
                results.push(AppResult { app: String::new(), outcome: Outcome::Failed { error: e } });
                return Ok(results);
            }
        }
    }
}

/// `update` in this process: `apps`, or every app with an update when empty.
fn update_here(apps: &[String], progress: &mut Progress) -> Result<Vec<AppResult>, String> {
    let engine = Engine::new(Paths::new()?, Scope::User, BackendOptions::default())?;
    progress.note("Checking for updates...");
    let survey = engine.survey();
    let targets: Vec<&Found> = if apps.is_empty() { survey.updates().collect() } else { apps.iter().filter_map(|id| survey.find(id)).collect() };
    let mut results = Vec::new();
    for found in targets {
        let stop = Stop::default();
        let id = found.app.id;
        let outcome = engine.apply(found, &stop, &mut |p: JobPhase| progress.show(id, p.into()));
        progress.done(&outcome.describe(found.app.name));
        results.push(AppResult { app: id.to_string(), outcome });
    }
    Ok(results)
}

/// Progress on the terminal: one line rewritten in place on a terminal, each new step on a line
/// of its own otherwise, nothing at all with `--json` (whose output is the result alone).
struct Progress {
    quiet: bool,
    terminal: bool,
    /// The step last shown, as `app` and a label without the numbers.
    last: Option<(String, &'static str)>,
    open: bool,
}

impl Progress {
    fn new(json: bool) -> Self {
        Self { quiet: json, terminal: std::io::stdout().is_terminal(), last: None, open: false }
    }

    fn show(&mut self, app: &str, phase: Phase) {
        if self.quiet {
            return;
        }
        let (step, detail) = match phase {
            Phase::Downloading { done, total } => ("downloading", amount(done, total)),
            Phase::Reconnecting { done, total } => ("reconnecting", amount(done, total)),
            Phase::Verifying => ("checking the download", String::new()),
            Phase::Waiting => ("waiting for another install to finish", String::new()),
            Phase::Installing => ("installing", String::new()),
            Phase::Removing => ("removing", String::new()),
        };
        let new_step = self.last.as_ref().is_none_or(|(a, s)| a != app || *s != step);
        self.last = Some((app.to_string(), step));
        let line = if detail.is_empty() { format!("{}: {step}", name(app)) } else { format!("{}: {step} {detail}", name(app)) };
        let mut out = std::io::stdout();
        if self.terminal {
            let _ = write!(out, "\r{line:<78}");
            let _ = out.flush();
            self.open = true;
        } else if new_step {
            let _ = writeln!(out, "{line}");
        }
    }

    /// A line that stays: an app's result.
    fn done(&mut self, line: &str) {
        self.last = None;
        self.note(line);
    }

    fn note(&mut self, line: &str) {
        if self.quiet {
            return;
        }
        let mut out = std::io::stdout();
        if self.open {
            let _ = write!(out, "\r{:<78}\r", "");
            self.open = false;
        }
        let _ = writeln!(out, "{line}");
    }
}

/// `12.3 of 64.3 MB (19%)`.
fn amount(done: u64, total: u64) -> String {
    let mb = |n: u64| n as f64 / 1_000_000.0;
    if total == 0 {
        return format!("{:.1} MB", mb(done));
    }
    format!("{:.1} of {:.1} MB ({}%)", mb(done), mb(total), done.saturating_mul(100) / total)
}

// ---- status ------------------------------------------------------------------------------------

fn status(options: Options) -> ExitCode {
    match service::status(false) {
        Ok(status) => {
            if options.json {
                print_json(&status);
            } else {
                print_status(&status);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if options.json {
                print_json(&NotAnswering { running: false, state: service_state(), error: e });
            } else {
                println!("The {DISPLAY_NAME} isn't answering: {e}.");
                println!("{}", service_state());
            }
            ExitCode::FAILURE
        }
    }
}

fn print_status(status: &Status) {
    let now = artcraft_launcher_native::now();
    let yes = |b: bool| if b { "yes" } else { "no" };
    println!("{DISPLAY_NAME} {} is running.", status.version);
    println!("  Installs updates in the background: {}", automatic(status));
    println!("  Takes requests from the launcher and this command: {}", yes(status.on_demand));
    let last = status.last_check.map_or_else(|| "not yet".to_string(), |t| artcraft_launcher_core::time::format_ago(t, now));
    let next = status.next_check.map_or_else(|| "not scheduled".to_string(), |t| until(t, now));
    println!("  Checks every {} hours. Last check: {last}. Next: {next}.", status.interval_hours);
    if let Some(busy) = &status.busy {
        println!("  Updating {} now.", name(busy));
    }
    let shown: Vec<&AppSummary> = status.apps.iter().filter(|a| a.state != AppState::NotInstalled || a.last_update.is_some()).collect();
    if shown.is_empty() {
        println!("\nNo Craft Apps installed for all users{}.", if status.last_check.is_none() { " seen yet (it hasn't checked)" } else { "" });
        return;
    }
    println!("\nApps installed for all users:");
    for app in shown {
        let last = app.last_update.as_ref().map_or_else(String::new, |l| {
            format!(" (last update {}: {})", artcraft_launcher_core::time::format_ago(l.at, now), l.outcome.describe(name(&app.id)))
        });
        println!("  {:<13} {:<17} {}{last}", name(&app.id), versions(app), describe_state(app));
    }
}

/// Whether the service installs updates in the background, and who decided.
fn automatic(status: &Status) -> String {
    match (status.auto_update, status.auto_update_managed) {
        (true, true) => "yes (set by Group Policy)".into(),
        (false, true) => "no (set by Group Policy)".into(),
        (true, false) => "yes (an administrator turned it on for everyone on this computer)".into(),
        (false, false) => "no (an administrator can turn it on in ArtCraft Launcher's settings, or with \
                           `artcraft-launcher-updater set auto-update on`)"
            .into(),
    }
}

/// `in 3 h`, `in 12 min`, `due now`.
fn until(at: i64, now: i64) -> String {
    match at.saturating_sub(now) {
        s if s <= 60 => "due now".into(),
        s if s < 3600 => format!("in {} min", s / 60),
        s => format!("in {} h", s / 3600),
    }
}

/// What Windows says about the service when it isn't answering.
#[cfg(windows)]
fn service_state() -> String {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    const NOT_INSTALLED: i32 = 1060;
    let state = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|manager| manager.open_service(service::SERVICE_NAME, ServiceAccess::QUERY_STATUS))
        .and_then(|service| service.query_status());
    match state {
        Ok(s) if s.current_state == ServiceState::Running => "Windows says it's running; it may be starting up.".into(),
        Ok(s) => format!("Windows says it's {:?}. Start it with: sc start {}", s.current_state, service::SERVICE_NAME),
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(NOT_INSTALLED) => {
            "It isn't installed on this computer. An administrator installs it once with artcraft-launcher-updater-<version>-windows-<arch>.msi."
                .into()
        }
        Err(e) => format!("Windows couldn't say how it is ({e})."),
    }
}

#[cfg(not(windows))]
fn service_state() -> String {
    "The update service runs on Windows only; here, use `artcraft-launcher-updater update`.".into()
}

fn print_json(value: &impl Serialize) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("artcraft-launcher-updater: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        Command::parse(&args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn reads_each_command() {
        let o = Options::default();
        assert_eq!(parse(&[]), Ok(Command::Help));
        assert_eq!(parse(&["check"]), Ok(Command::Check(o)));
        assert_eq!(parse(&["check", "--json"]), Ok(Command::Check(Options { json: true, ..o })));
        assert_eq!(parse(&["update"]), Ok(Command::Update(o, vec![])));
        assert_eq!(
            parse(&["update", "PhotoCraft", "wordcraft", "--local"]),
            Ok(Command::Update(Options { local: true, ..o }, vec!["photocraft".into(), "wordcraft".into()]))
        );
        assert_eq!(parse(&["status", "--json"]), Ok(Command::Status(Options { json: true, ..o })));
        assert_eq!(parse(&["run"]), Ok(Command::Run { once: false }));
        assert_eq!(parse(&["run", "--once"]), Ok(Command::Run { once: true }));
        assert_eq!(parse(&["service"]), Ok(Command::Service));
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
        assert_eq!(parse(&["update", "--help"]), Ok(Command::Help));
        assert!(parse(&["check", "-v"]).is_ok_and(|c| c.verbose()));
    }

    #[test]
    fn names_apps_any_way_it_was_ever_called() {
        assert_eq!(apps(&["PDFCRAFT", "printcraft", "PrintCraft"]), Ok(vec!["pdfcraft".into(); 3]));
        assert!(apps(&["notepad"]).unwrap_err().contains("isn't a Craft App"));
    }

    #[test]
    fn refuses_what_makes_no_sense() {
        for bad in
            [&["frobnicate"][..], &["check", "photocraft"], &["check", "--once"], &["status", "--local"], &["run", "--json"], &["check", "--force"]]
        {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn shows_sizes_and_waits_readably() {
        assert_eq!(amount(12_345_678, 64_300_000), "12.3 of 64.3 MB (19%)");
        assert_eq!(amount(500_000, 0), "0.5 MB");
        assert_eq!(until(1000, 1000), "due now");
        assert_eq!(until(1000 + 12 * 60, 1000), "in 12 min");
        assert_eq!(until(1000 + 3 * 3600 + 5, 1000), "in 3 h");
    }

    #[test]
    fn status_says_who_decides_about_background_installs() {
        let status = |auto_update, auto_update_managed| Status { auto_update, auto_update_managed, ..Status::default() };
        assert!(automatic(&status(false, false)).starts_with("no (an administrator can turn it on"));
        assert!(automatic(&status(true, false)).starts_with("yes (an administrator turned it on"));
        assert_eq!(automatic(&status(true, true)), "yes (set by Group Policy)");
        assert_eq!(automatic(&status(false, true)), "no (set by Group Policy)");
    }

    #[test]
    fn reads_the_administrators_setting() {
        assert_eq!(parse(&["set", "auto-update", "on"]), Ok(Command::SetAutoUpdate(true)));
        assert_eq!(parse(&["set", "auto-update", "off"]), Ok(Command::SetAutoUpdate(false)));
        for bad in [
            &["set"][..],
            &["set", "auto-update"],
            &["set", "auto-update", "maybe"],
            &["set", "colour", "on"],
            &["set", "auto-update", "on", "--json"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_row_says_where_the_app_stands() {
        let app = AppSummary {
            id: "photocraft".into(),
            installed: Some("0.8.1".into()),
            latest: Some("0.8.2".into()),
            state: AppState::UpdateAvailable,
            ..AppSummary::default()
        };
        assert_eq!((versions(&app).as_str(), describe_state(&app).as_str()), ("0.8.1 -> 0.8.2", "update available"));
        let blocked = AppSummary { state: AppState::Blocked, note: Some("installed for one user".into()), ..app };
        assert_eq!((versions(&blocked).as_str(), describe_state(&blocked).as_str()), ("0.8.1", "not updated here: installed for one user"));
    }
}
