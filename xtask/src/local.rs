//! `cargo xtask local`: install the Craft apps from builds on this machine instead of GitHub.
//!
//! - `sync` pulls every app's checkout (the sibling folders of this repo), builds and packages the
//!   ones whose version isn't in the mirror yet, and files the results under
//!   `<mirror>/<app>/<version>/` with a `SHA256SUMS.txt`.
//! - `serve` serves the mirror on 127.0.0.1 and answers `release_info.json` with a feed (the same
//!   shape as ArtCraft's) written fresh from whatever is in the mirror at that moment.
//!
//! Run the launcher with `ARTCRAFT_LAUNCHER_DOWNLOAD_BASE=http://127.0.0.1:<port>/` and it installs
//! and updates from the mirror alone.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Every Craft App: (id = folder name, display name). Keep in step with `catalog::SUITE`.
const APPS: &[(&str, &str)] = &[
    ("photocraft", "PhotoCraft"),
    ("vectorcraft", "VectorCraft"),
    ("filmcraft", "FilmCraft"),
    ("lightcraft", "LightCraft"),
    ("pdfcraft", "PdfCraft"),
    ("effectcraft", "EffectCraft"),
    ("designcraft", "DesignCraft"),
    ("figcraft", "FigCraft"),
    ("wordcraft", "WordCraft"),
    ("soundcraft", "SoundCraft"),
    ("deckcraft", "DeckCraft"),
    ("cadcraft", "CADCraft"),
    ("gridcraft", "GridCraft"),
];

const DEFAULT_PORT: u16 = 8787;
/// Marks a version folder whose build finished; anything without it is ignored and rebuilt.
const COMPLETE: &str = ".complete";
const SUMS: &str = "SHA256SUMS.txt";
const NOTES: &str = "NOTES.md";
/// The commit a version folder was built from.
const COMMIT: &str = ".commit";

pub const USAGE: &str = "\
usage: cargo xtask local <command> [options]

commands:
  sync [options] [-- <package.sh args>]
                  git pull every app checkout, then build and package each app whose version
                  isn't in the mirror yet
      --root <dir>     folder holding the app checkouts (default: this repo's parent)
      --mirror <dir>   where builds are kept (default: <root>/local-mirror)
      --only <a,b>     just these apps
      --no-pull        don't pull; build what's checked out
      --rebuild        build even when the version is already mirrored
      --every-commit   build each new commit, as <next patch>-dev.<commit time> (0.4.0 builds
                       as 0.4.1-dev.20261010183500), so the launcher offers it as an update
      --arch <arch>    macOS: universal | aarch64 | x86_64 (default: this Mac's)
      --keep <n>       versions kept per app (default 2)
  serve [--mirror <dir>] [--port <n>] [--sync-every <interval> [sync options]]
                  serve the mirror and its feed on 127.0.0.1 (default port 8787); with
                  --sync-every (30m, 1h, 1d…), also run `sync` now and then on that interval
  status [--root <dir>] [--mirror <dir>]
                  each app's checked-out version and commit next to what's mirrored

then: ARTCRAFT_LAUNCHER_DOWNLOAD_BASE=http://127.0.0.1:8787/ cargo run -p artcraft-launcher --features local-mirror
";

#[derive(Clone)]
struct Opts {
    root: PathBuf,
    mirror: PathBuf,
    only: Option<Vec<String>>,
    pull: bool,
    rebuild: bool,
    every_commit: bool,
    arch: Option<String>,
    keep: usize,
    port: u16,
    /// `serve` also syncs this often (seconds).
    sync_every: Option<u64>,
    passthrough: Vec<String>,
}

pub fn run(args: &[&str]) -> Result<(), String> {
    let (cmd, rest) = args.split_first().ok_or_else(|| USAGE.to_string())?;
    let opts = parse(rest)?;
    match *cmd {
        "sync" => sync(&opts),
        "serve" => serve(&opts),
        "status" => status(&opts),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command `local {other}`\n\n{USAGE}")),
    }
}

fn parse(args: &[&str]) -> Result<Opts, String> {
    let default_root = crate::root().parent().map(Path::to_path_buf).ok_or("this repo has no parent folder")?;
    let mut root = None;
    let mut mirror = None;
    let mut o = Opts {
        root: PathBuf::new(),
        mirror: PathBuf::new(),
        only: None,
        pull: true,
        rebuild: false,
        every_commit: false,
        arch: None,
        keep: 2,
        port: DEFAULT_PORT,
        sync_every: None,
        passthrough: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().map(|v| (*v).to_string()).ok_or_else(|| format!("{arg} needs a value"));
        match *arg {
            "--root" => root = Some(PathBuf::from(value()?)),
            "--mirror" => mirror = Some(PathBuf::from(value()?)),
            "--only" => o.only = Some(value()?.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()),
            "--no-pull" => o.pull = false,
            "--rebuild" => o.rebuild = true,
            "--every-commit" => o.every_commit = true,
            "--arch" => o.arch = Some(value()?),
            "--keep" => o.keep = value()?.parse().ok().filter(|n| *n >= 1).ok_or("--keep needs a number of at least 1")?,
            "--port" => o.port = value()?.parse().map_err(|_| "--port needs a port number")?,
            "--sync-every" => o.sync_every = Some(parse_interval(&value()?)?),
            "--" => {
                o.passthrough = it.by_ref().map(|s| (*s).to_string()).collect();
            }
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    o.root = root.unwrap_or(default_root);
    o.mirror = mirror.unwrap_or_else(|| o.root.join("local-mirror"));
    if let Some(only) = &o.only
        && let Some(bad) = only.iter().find(|id| !APPS.iter().any(|(a, _)| a == id))
    {
        return Err(format!("unknown app `{bad}` (known: {})", APPS.iter().map(|a| a.0).collect::<Vec<_>>().join(", ")));
    }
    Ok(o)
}

fn selected(o: &Opts) -> impl Iterator<Item = &'static (&'static str, &'static str)> + '_ {
    APPS.iter().filter(|(id, _)| o.only.as_ref().is_none_or(|only| only.iter().any(|x| x == id)))
}

// ---- sync ---------------------------------------------------------------------------------------

enum Outcome {
    Built(String),
    UpToDate(String),
    Skipped(String),
    Failed(String),
}

fn sync(o: &Opts) -> Result<(), String> {
    std::fs::create_dir_all(&o.mirror).map_err(|e| format!("couldn't create {} ({e})", o.mirror.display()))?;
    let mut results = Vec::new();
    for (id, name) in selected(o) {
        eprintln!("\n=== {name} ===");
        let outcome = sync_one(o, id, name).unwrap_or_else(Outcome::Failed);
        results.push((*name, outcome));
    }
    println!("\nlocal sync: {}", o.mirror.display());
    let mut failed = 0;
    for (name, outcome) in &results {
        let (tag, what) = match outcome {
            Outcome::Built(w) => ("built", w),
            Outcome::UpToDate(w) => ("ok", w),
            Outcome::Skipped(w) => ("skip", w),
            Outcome::Failed(w) => {
                failed += 1;
                ("FAIL", w)
            }
        };
        println!("  {tag:<6} {name:<12} {what}");
    }
    if failed == 0 { Ok(()) } else { Err(format!("{failed} app(s) failed")) }
}

fn sync_one(o: &Opts, id: &str, name: &str) -> Result<Outcome, String> {
    let repo = o.root.join(id);
    if !repo.join(".git").exists() {
        return Ok(Outcome::Skipped(format!("no checkout at {} (git clone git@github.com:storytold/{id}.git)", repo.display())));
    }
    let mut pull_note = String::new();
    if o.pull {
        // --ff-only never makes a merge commit in someone's checkout; a diverged or dirty one is
        // reported and built as it stands.
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["pull", "--ff-only", "--no-stat", "--recurse-submodules"])
            .status()
            .map_err(|e| format!("couldn't run git ({e})"))?
            .success();
        if !ok {
            pull_note = " (pull failed; built the checkout as it is)".into();
        }
    }
    let checkout = checkout_version(&repo)?;
    let commit = git(&repo, &["rev-parse", "--short=12", "HEAD"])?;
    let app_dir = o.mirror.join(id);
    let version = if o.every_commit {
        // The newest mirrored build is already this commit: nothing to do.
        if !o.rebuild
            && let Some(latest) = latest_version(&app_dir)
            && std::fs::read_to_string(app_dir.join(&latest).join(COMMIT)).is_ok_and(|c| c.trim() == commit)
        {
            return Ok(Outcome::UpToDate(format!("{latest} is {commit}{pull_note}")));
        }
        let committed = git(&repo, &["show", "-s", "--format=%ct", "HEAD"])?.parse::<i64>().map_err(|_| "unreadable commit time")?;
        dev_version(&checkout, committed)?
    } else {
        checkout
    };
    let dest = app_dir.join(&version);
    if !o.rebuild && dest.join(COMPLETE).is_file() {
        let built_from = std::fs::read_to_string(dest.join(COMMIT)).unwrap_or_default();
        let note = if built_from.trim() == commit {
            String::new()
        } else {
            format!(
                "; {commit} is newer than the mirrored {} but has the same version (--every-commit builds each commit, --rebuild replaces this one)",
                built_from.trim()
            )
        };
        return Ok(Outcome::UpToDate(format!("{version} already mirrored{note}{pull_note}")));
    }

    // Build into a scratch folder, then swap it in, so a failed build never leaves half a version.
    let building = app_dir.join(format!(".building-{version}"));
    let _ = std::fs::remove_dir_all(&building);
    std::fs::create_dir_all(&building).map_err(|e| format!("couldn't create {} ({e})", building.display()))?;
    let mut cmd = package_command(&repo, o)?;
    cmd.current_dir(&repo).env("DIST", &building);
    if o.every_commit {
        // Every app's packaging reads <APP>_VERSION in place of its Cargo.toml version, and keeps
        // the full version (with the -dev suffix) in the installed copy, where the launcher reads it.
        cmd.env(format!("{}_VERSION", id.to_ascii_uppercase()), &version);
    }
    eprintln!("$ {name}: packaging {version} ({commit})");
    let status = cmd.status().map_err(|e| format!("couldn't run the packaging script ({e})"))?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&building);
        return Err(format!("packaging failed ({status}){pull_note}"));
    }
    let files = asset_files(&building)?;
    if files.is_empty() {
        let _ = std::fs::remove_dir_all(&building);
        return Err("packaging produced no files".into());
    }
    write_sums(&building, &files)?;
    let previous = latest_version(&app_dir).filter(|v| *v != version);
    let notes = notes(&repo, previous.as_ref().and_then(|v| std::fs::read_to_string(app_dir.join(v).join(COMMIT)).ok()).as_deref(), &commit);
    write(&building.join(NOTES), &notes)?;
    write(&building.join(COMMIT), &commit)?;
    write(&building.join(COMPLETE), "")?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&building, &dest).map_err(|e| format!("couldn't move the build into place ({e})"))?;
    prune(&app_dir, o.keep);
    Ok(Outcome::Built(format!("{version} ({commit}, {} files){pull_note}", files.len())))
}

/// This OS's packaging script for the checkout at `repo`.
fn package_command(repo: &Path, o: &Opts) -> Result<Command, String> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("bash");
        c.arg(repo.join("packaging/macos/package.sh"));
        let arch = o.arch.clone().unwrap_or_else(|| if cfg!(target_arch = "aarch64") { "aarch64".into() } else { "x86_64".into() });
        c.args(["--arch", &arch]);
        c
    } else if cfg!(target_os = "linux") || cfg!(target_os = "freebsd") {
        let os = if cfg!(target_os = "linux") { "linux" } else { "freebsd" };
        let mut c = Command::new("bash");
        c.arg(repo.join(format!("packaging/{os}/package.sh")));
        c
    } else if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(repo.join("packaging/windows/package.ps1"));
        c
    } else {
        return Err("no packaging script for this OS".into());
    };
    cmd.args(&o.passthrough);
    Ok(cmd)
}

/// `[workspace.package] version` (or `[package]`) of the checkout.
fn checkout_version(repo: &Path) -> Result<String, String> {
    let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).map_err(|e| format!("couldn't read Cargo.toml ({e})"))?;
    crate::version::read(&manifest)
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().map_err(|e| format!("couldn't run git ({e})"))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Release notes: the commits since the last mirrored build (or the last 20).
fn notes(repo: &Path, since: Option<&str>, commit: &str) -> String {
    let range = since.map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("{s}..HEAD"));
    let mut args = vec!["log", "--no-merges", "--format=* %s (%h)"];
    match &range {
        Some(r) => args.push(r),
        None => args.push("-20"),
    }
    let log = git(repo, &args).unwrap_or_default();
    let mut out = format!("Built locally from `{commit}`.\n");
    if !log.is_empty() {
        out.push_str("\n## Changes\n");
        out.push_str(&log);
        out.push('\n');
    }
    out
}

/// Keeps the newest `keep` complete versions of an app.
fn prune(app_dir: &Path, keep: usize) {
    let mut versions = complete_versions(app_dir);
    versions.sort_by(|a, b| compare_versions(b, a));
    for old in versions.iter().skip(keep) {
        let _ = std::fs::remove_dir_all(app_dir.join(old));
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("couldn't write {} ({e})", path.display()))
}

// ---- status -------------------------------------------------------------------------------------

fn status(o: &Opts) -> Result<(), String> {
    println!("{:<12} {:<12} {:<14} {:<12} built from", "app", "checkout", "commit", "mirrored");
    for (id, name) in selected(o) {
        let repo = o.root.join(id);
        let (version, commit) = if repo.join(".git").exists() {
            (checkout_version(&repo).unwrap_or_else(|_| "?".into()), git(&repo, &["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|_| "?".into()))
        } else {
            ("-".into(), "-".into())
        };
        let app_dir = o.mirror.join(id);
        let mirrored = latest_version(&app_dir);
        let built_from = mirrored.as_ref().and_then(|v| std::fs::read_to_string(app_dir.join(v).join(COMMIT)).ok()).unwrap_or_default();
        println!("{name:<12} {version:<12} {commit:<14} {:<12} {}", mirrored.as_deref().unwrap_or("-"), built_from.trim());
    }
    Ok(())
}

// ---- mirror contents ----------------------------------------------------------------------------

fn complete_versions(app_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(app_dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().join(COMPLETE).is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|v| parse_version(v).is_some())
        .collect()
}

fn latest_version(app_dir: &Path) -> Option<String> {
    complete_versions(app_dir).into_iter().max_by(|a, b| compare_versions(a, b))
}

/// The installable files in a version folder (everything but our own bookkeeping).
fn asset_files(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("couldn't read {} ({e})", dir.display()))?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.starts_with('.') && n != SUMS && n != NOTES && safe_name(n))
        .collect();
    names.sort();
    Ok(names)
}

fn write_sums(dir: &Path, files: &[String]) -> Result<(), String> {
    let mut sums = String::new();
    for name in files {
        let _ = writeln!(sums, "{}  {name}", sha256(&dir.join(name))?);
    }
    write(&dir.join(SUMS), &sums)
}

fn read_sums(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(dir.join(SUMS))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (hash, name) = l.split_once(char::is_whitespace)?;
            Some((name.trim().trim_start_matches('*').to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// SHA-256 through the system's own tool (files are hundreds of megabytes).
fn sha256(path: &Path) -> Result<String, String> {
    let out = if cfg!(windows) {
        Command::new("certutil").arg("-hashfile").arg(path).arg("SHA256").output()
    } else if cfg!(target_os = "macos") {
        Command::new("shasum").args(["-a", "256"]).arg(path).output()
    } else {
        Command::new("sha256sum").arg(path).output()
    }
    .map_err(|e| format!("couldn't hash {} ({e})", path.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    // shasum/sha256sum: "<hash>  <file>"; certutil: the hash alone on the second line.
    text.split_whitespace()
        .find(|w| w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("couldn't hash {}", path.display()))
}

// ---- feed ---------------------------------------------------------------------------------------

/// A feed (ArtCraft's `release_info` shape) for the newest complete version of every app.
fn feed(mirror: &Path, base: &str) -> String {
    let now = rfc3339(unix_now());
    let mut apps = String::new();
    for (id, name) in APPS {
        let app_dir = mirror.join(id);
        let Some(version) = latest_version(&app_dir) else { continue };
        let dir = app_dir.join(&version);
        let Ok(files) = asset_files(&dir) else { continue };
        let sums = read_sums(&dir);
        let mut assets = Vec::new();
        for f in &files {
            let size = std::fs::metadata(dir.join(f)).map(|m| m.len()).unwrap_or(0);
            let sha = sums.get(f).map_or_else(|| "null".into(), |h| json_str(h));
            assets.push(format!(
                r#"{{"name":{},"size_bytes":{size},"download_url":{},"sha256":{sha}}}"#,
                json_str(f),
                json_str(&format!("{base}{id}/{version}/{f}"))
            ));
        }
        let built = std::fs::metadata(dir.join(COMPLETE)).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok());
        let published = built.map_or_else(|| now.clone(), |d| rfc3339(i64::try_from(d.as_secs()).unwrap_or(0)));
        let notes = std::fs::read_to_string(dir.join(NOTES)).unwrap_or_default();
        if !apps.is_empty() {
            apps.push(',');
        }
        let _ = write!(
            apps,
            r#"{}:{{"name":{},"repo":{},"latest_release":{{"tag":{},"name":{},"published_at":{},"html_url":"","prerelease":false,"notes_markdown":{},"assets":[{}]}},"app_last_fetched":{}}}"#,
            json_str(id),
            json_str(name),
            json_str(&format!("storytold/{id}")),
            json_str(&format!("v{version}")),
            json_str(&format!("{name} v{version} (local build)")),
            json_str(&published),
            json_str(&notes),
            assets.join(","),
            json_str(&now),
        );
    }
    format!(
        r#"{{"success":true,"release_info":{{"apps":{{{apps}}},"last_fetched":{}}},"app_renames":[],"repo_renames":[],"important_notices":{{"announcements":[],"alerts":[]}}}}"#,
        json_str(&now)
    )
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// `2026-10-10T12:00:00Z` (through jiff's civil calendar). jiff spans only years -9999..=9999, far
/// narrower than the whole `i64` the old hand-rolled Hinnant code converted, so the instant is
/// moved into range by whole 400-year Gregorian cycles (146_097 days) and the real year restored
/// afterwards. jiff still does the month/day; the cycle count only shifts the year, keeping every
/// `i64` exact (e.g. `i64::MAX` → year 292277026596) with no epoch fallback.
fn rfc3339(secs: i64) -> String {
    let rem = secs.rem_euclid(86_400);
    let epoch = jiff::civil::Date::constant(1970, 1, 1);
    let days = secs.div_euclid(86_400);
    let (cycles, rest) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    // `rest` is in `0..146_097` (under one 400-year cycle), so `epoch + rest days` lands in
    // 1970..=2369 (inside jiff's range) and `rest * 86_400` never overflows.
    let date = epoch.checked_add(jiff::SignedDuration::from_secs(rest * 86_400)).unwrap_or(epoch);
    // Restore the real year through `i128` so `cycles * 400` can't overflow at the `i64` extremes.
    let y = i128::from(date.year()) + i128::from(cycles) * 400;
    let y = i64::try_from(y).unwrap_or(if y < 0 { i64::MIN } else { i64::MAX });
    format!("{y:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", date.month(), date.day(), rem / 3600, rem % 3600 / 60, rem % 60)
}

// ---- versions -----------------------------------------------------------------------------------

/// `1.2.3` or `1.2.3-pre.4`: the numeric core and the pre-release, if any.
fn parse_version(v: &str) -> Option<([u64; 3], Option<String>)> {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let n = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some((n, pre))
}

/// The version a build of an unreleased commit gets: after `checkout` and before whatever is
/// released next, ordered by commit time. `0.4.0` → `0.4.1-dev.20261010183500`;
/// `1.0.0-rc.1` → `1.0.0-rc.1.dev.20261010183500`.
fn dev_version(checkout: &str, committed: i64) -> Result<String, String> {
    let ([major, minor, patch], pre) = parse_version(checkout).ok_or_else(|| format!("can't read version `{checkout}`"))?;
    let stamp: String = rfc3339(committed).chars().filter(char::is_ascii_digit).collect();
    Ok(match pre {
        None => format!("{major}.{minor}.{}-dev.{stamp}", patch.checked_add(1).ok_or("version too large")?),
        Some(pre) => format!("{major}.{minor}.{patch}-{pre}.dev.{stamp}"),
    })
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (parse_version(a), parse_version(b)) {
        (Some((na, pa)), Some((nb, pb))) => na.cmp(&nb).then_with(|| match (pa, pb) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(x), Some(y)) => x.cmp(&y),
        }),
        _ => a.cmp(b),
    }
}

// ---- serve --------------------------------------------------------------------------------------

fn serve(o: &Opts) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", o.port)).map_err(|e| format!("couldn't listen on 127.0.0.1:{} ({e})", o.port))?;
    let base = format!("http://127.0.0.1:{}/", o.port);
    println!("serving {} at {base}", o.mirror.display());
    println!("feed:    {base}release_info.json");
    println!("launch:  ARTCRAFT_LAUNCHER_DOWNLOAD_BASE={base} cargo run -p artcraft-launcher --features local-mirror");
    if let Some(every) = o.sync_every {
        // Syncs run one after another on their own thread, so a long build never holds up a
        // download; a version only joins the feed once its build is complete.
        let opts = o.clone();
        std::thread::spawn(move || {
            loop {
                if let Err(e) = sync(&opts) {
                    eprintln!("sync: {e}");
                }
                eprintln!("next sync in {}", describe_interval(every));
                std::thread::sleep(std::time::Duration::from_secs(every));
            }
        });
    }
    // Bounded admission: a local dev tool still shouldn't let idle or flooding clients spawn
    // unbounded threads. Refuse new connections past the cap, and never let a failed thread spawn
    // take down the listener (CWE-770).
    const MAX_HANDLERS: usize = 64;
    let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        let (mirror, base) = (o.mirror.clone(), base.clone());
        if active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MAX_HANDLERS {
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            let mut out = stream;
            let _ = respond(&mut out, "503 Service Unavailable", "text/plain", b"busy", false);
            continue;
        }
        let worker_active = active.clone();
        let spawned = std::thread::Builder::new().spawn(move || {
            if let Err(e) = handle(stream, &mirror, &base) {
                eprintln!("  {e}");
            }
            worker_active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
        if spawned.is_err() {
            // Couldn't start a worker: drop this connection but keep serving.
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            eprintln!("  couldn't start a connection handler; dropping the connection");
        }
    }
    Ok(())
}

fn handle(stream: TcpStream, mirror: &Path, base: &str) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    // A single wall-clock deadline across the request line and every header: an idle or trickling
    // client must not be able to hold a handler thread open indefinitely (CWE-770).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut reader = BufReader::new(stream.try_clone().map_err(io)?);
    let read_line_until = |reader: &mut BufReader<TcpStream>, buf: &mut String| -> Result<usize, String> {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("request header deadline exceeded".into());
        }
        reader.get_ref().set_read_timeout(Some(remaining)).map_err(io)?;
        reader.read_line(buf).map_err(io)
    };
    let mut line = String::new();
    read_line_until(&mut reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut range_start: Option<u64> = None;
    loop {
        let mut h = String::new();
        if read_line_until(&mut reader, &mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.trim().eq_ignore_ascii_case("range")
        {
            range_start = v.trim().strip_prefix("bytes=").and_then(|r| r.strip_suffix('-')).and_then(|n| n.parse().ok());
        }
    }
    let mut out = stream;
    out.set_write_timeout(Some(std::time::Duration::from_secs(15))).map_err(io)?;
    // Headers are in; a legitimate download can take as long as it needs, so lift the deadline.
    let _ = out.set_read_timeout(None);
    let head = method == "HEAD";
    if method != "GET" && !head {
        return respond(&mut out, "405 Method Not Allowed", "text/plain", b"", head);
    }
    let path = target.split(['?', '#']).next().unwrap_or("").trim_start_matches('/');
    eprintln!("{method} /{path}");
    if path == "release_info.json" {
        return respond(&mut out, "200 OK", "application/json", feed(mirror, base).as_bytes(), head);
    }
    // Only <app>/<version>/<file>, and only plain names: nothing outside the mirror.
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() != 3 || !segments.iter().all(|s| safe_name(s)) || segments.iter().any(|s| s.starts_with('.')) {
        return respond(&mut out, "404 Not Found", "text/plain", b"not found", head);
    }
    let file_path = mirror.join(segments.join("/"));
    let Ok(mut file) = std::fs::File::open(&file_path) else {
        return respond(&mut out, "404 Not Found", "text/plain", b"not found", head);
    };
    let size = file.metadata().map_err(io)?.len();
    let start = range_start.unwrap_or(0);
    if start > 0 && start >= size {
        let header = format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return out.write_all(header.as_bytes()).map_err(io);
    }
    let header = if start > 0 {
        format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{size}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            size - start,
            size.saturating_sub(1)
        )
    } else {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {size}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
        )
    };
    out.write_all(header.as_bytes()).map_err(io)?;
    if head {
        return Ok(());
    }
    file.seek(SeekFrom::Start(start)).map_err(io)?;
    std::io::copy(&mut file.take(size - start), &mut out).map(|_| ()).map_err(io)
}

/// `90s`, `30m`, `1h`, `1d` (or plain seconds); at least a minute.
fn parse_interval(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let n: u64 = number.parse().map_err(|_| format!("`{text}` isn't an interval (try 30m, 1h or 1d)"))?;
    let unit = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(format!("`{text}` isn't an interval (try 30m, 1h or 1d)")),
    };
    n.checked_mul(unit).filter(|s| *s >= 60).ok_or_else(|| format!("--sync-every {text}: use at least a minute"))
}

fn describe_interval(secs: u64) -> String {
    match secs {
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

fn respond(out: &mut TcpStream, status: &str, kind: &str, body: &[u8], head: bool) -> Result<(), String> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    out.write_all(header.as_bytes()).map_err(|e| e.to_string())?;
    if !head {
        out.write_all(body).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_versions() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_791_500_000), "2026-10-08T22:53:20Z");
        // Year 10000 and the `i64` extremes are past jiff's own calendar range (-9999..=9999), but
        // the 400-year cycle reduction keeps the dates the old Hinnant code produced, not the epoch.
        assert_eq!(rfc3339(253_402_300_800), "10000-01-01T00:00:00Z");
        assert_eq!(rfc3339(i64::MAX), "292277026596-12-04T15:30:07Z");
        assert_eq!(rfc3339(i64::MIN), "-292277022657-01-27T08:29:52Z");
        assert_eq!(compare_versions("0.10.0", "0.9.0"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc.1"), std::cmp::Ordering::Greater);
        assert!(parse_version("1.2").is_none() && parse_version(".building-1.2.3").is_none());
    }

    #[test]
    fn writes_a_feed_the_launcher_reads() {
        let mirror = std::env::temp_dir().join(format!("xtask-local-{}", std::process::id()));
        let dir = mirror.join("photocraft").join("0.6.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("photocraft-0.6.0-macos-aarch64.dmg"), b"dmg").unwrap();
        std::fs::write(dir.join(SUMS), format!("{}  photocraft-0.6.0-macos-aarch64.dmg\n", "ab".repeat(32))).unwrap();
        std::fs::write(dir.join(NOTES), "Built \"locally\".\n").unwrap();
        // Unfinished builds are invisible.
        std::fs::create_dir_all(mirror.join("photocraft").join("0.7.0")).unwrap();
        std::fs::write(dir.join(COMPLETE), "").unwrap();
        let json = feed(&mirror, "http://127.0.0.1:8787/");
        let _ = std::fs::remove_dir_all(&mirror);
        assert!(json.contains(r#""tag":"v0.6.0""#), "{json}");
        assert!(json.contains(r#""download_url":"http://127.0.0.1:8787/photocraft/0.6.0/photocraft-0.6.0-macos-aarch64.dmg""#));
        assert!(json.contains(r#""size_bytes":3"#) && json.contains(&"ab".repeat(32)));
        assert!(json.contains(r#"Built \"locally\".\n"#));
        assert!(!json.contains("0.7.0"));
    }

    #[test]
    fn dev_versions_sit_between_releases_and_follow_commit_time() {
        const T: i64 = 1_791_500_000; // 2026-10-08T22:53:20Z
        assert_eq!(dev_version("0.4.0", T).as_deref(), Ok("0.4.1-dev.20261008225320"));
        assert_eq!(dev_version("1.0.0-rc.1", T).as_deref(), Ok("1.0.0-rc.1.dev.20261008225320"));
        let (dev, later) = (dev_version("0.4.0", T).unwrap(), dev_version("0.4.0", T + 60).unwrap());
        assert_eq!(compare_versions(&dev, "0.4.0"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions(&later, &dev), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("0.4.1", &later), std::cmp::Ordering::Greater);
        assert!(dev_version("main", T).is_err());
    }

    #[test]
    fn intervals() {
        assert_eq!(parse_interval("30m"), Ok(1800));
        assert_eq!(parse_interval("1h"), Ok(3600));
        assert_eq!(parse_interval("2d"), Ok(172_800));
        assert_eq!(parse_interval("120"), Ok(120));
        for bad in ["", "h", "10s", "1w", "-1h", "99999999999999999999d"] {
            assert!(parse_interval(bad).is_err(), "{bad}");
        }
        assert_eq!(describe_interval(5400), "90m");
    }

    #[test]
    fn only_plain_names_are_served() {
        assert!(safe_name("photocraft-0.6.0-macos-universal.dmg"));
        for bad in ["", ".", "..", "a/b", "a\\b", "a%2e"] {
            assert!(!safe_name(bad), "{bad}");
        }
    }
}
