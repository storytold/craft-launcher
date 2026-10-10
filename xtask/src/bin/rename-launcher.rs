//! `cargo run -p xtask --bin rename-launcher -- <out-dir> <name>`
//!
//! Stages a copy of the workspace in `<out-dir>` in which ArtCraft Launcher is called `<name>`, for
//! `packaging/windows/build-renamed.ps1`. The workspace itself is never changed.
//!
//! In the copy:
//! - The display name is replaced inside Rust text string literals (window title, tray menu,
//!   Settings, notifications, messages, the exe's version info), in the UI catalogs
//!   (`crates/ui-egui/src/i18n/*.tsv`, whose English keys must match the code's literals) and in the
//!   Windows installer (`packaging/windows/*.wxs`, `*.wxl`, `portable.txt`). Identifiers, comments,
//!   byte strings, paths and registry keys (`…\ArtCraft Launcher`), `ArtCraft Launcher.app` and the
//!   app id `ai.storyteller.artcraft-launcher` stay as they are, so a renamed build still finds what
//!   the official one installed and recorded.
//! - The exe and the installers are named after the new name in lower case letters and digits
//!   ("My Launcher" -> `mylauncher.exe`, `mylauncher-<version>-windows-x64.msi`).
//! - Every build gets `ARTCRAFT_LAUNCHER_NO_SELF_UPDATE` (`.cargo/config.toml`), so the renamed
//!   launcher never replaces itself with the official release, which would bring the original name
//!   back.
//!
//! Only files whose content changed are rewritten, so cargo rebuilds only what changed.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The display name being replaced.
const ORIGINAL: &str = "ArtCraft Launcher";
/// The cargo package, binary and file-name stem.
const PKG: &str = "artcraft-launcher";
const MAX_NAME_CHARS: usize = 60;
/// Characters that would break a Rust format string, a WiX formatted field, an XML attribute, a
/// file name or a PowerShell literal.
const FORBIDDEN: &str = "\"\\/:*?<>|{}[]%";

const USAGE: &str = "usage: cargo run -p xtask --bin rename-launcher -- <out-dir> <name>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let [out, name] = args else { return Err(USAGE.into()) };
    let rename = Rename::new(name)?;
    let root = root();
    let out = prepare_out(&root, Path::new(out))?;
    let files = source_files(&root, &out)?;
    let (written, removed) = stage(&root, &out, &files, &rename)?;
    println!("staged {} files in {} ({written} written, {removed} removed): \"{}\", {}.exe", files.len(), out.display(), rename.name, rename.machine);
    Ok(())
}

/// Workspace root (parent of the xtask crate).
fn root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap_or(manifest_dir).to_path_buf()
}

/// Creates `out` and returns it as an absolute path; refuses the workspace or a folder holding it.
fn prepare_out(root: &Path, out: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("couldn't create {} ({e})", out.display()))?;
    let out = std::path::absolute(out).map_err(|e| format!("couldn't resolve {} ({e})", out.display()))?;
    if root.starts_with(&out) {
        return Err(format!("refusing to stage into {}: it holds the workspace", out.display()));
    }
    Ok(out)
}

/// The workspace's files as git sees them (tracked, plus new files that aren't ignored), with
/// `/` separators. Files deleted from the disk and anything under `out` are left out.
fn source_files(root: &Path, out: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .map_err(|e| format!("couldn't run git to list the workspace's files ({e}); a renamed build needs a git checkout"))?;
    if !output.status.success() {
        return Err(format!("git ls-files failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let list = String::from_utf8(output.stdout).map_err(|_| "git listed a file name that isn't UTF-8".to_string())?;
    let mut files: Vec<String> = list
        .split('\0')
        .filter(|rel| !rel.is_empty())
        .filter(|rel| {
            let path = root.join(rel_path(rel));
            path.is_file() && !path.starts_with(out)
        })
        .map(str::to_string)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// A `/`-separated relative path as a native one.
fn rel_path(rel: &str) -> PathBuf {
    rel.split('/').collect()
}

/// Mirrors `files` from `root` into `out`, renamed. Returns (files written, stale files removed).
fn stage(root: &Path, out: &Path, files: &[String], rename: &Rename) -> Result<(usize, usize), String> {
    let mut written = 0usize;
    let mut keep = BTreeSet::new();
    for rel in files {
        let bytes = std::fs::read(root.join(rel_path(rel))).map_err(|e| format!("couldn't read {rel} ({e})"))?;
        let bytes = rename.file(rel, bytes)?;
        let dst = out.join(rel_path(rel));
        if std::fs::read(&dst).ok().as_deref() != Some(bytes.as_slice()) {
            if let Some(dir) = dst.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {} ({e})", dir.display()))?;
            }
            std::fs::write(&dst, &bytes).map_err(|e| format!("couldn't write {} ({e})", dst.display()))?;
            written = written.saturating_add(1);
        }
        keep.insert(dst);
    }
    let removed = remove_stale(out, true, &keep)?;
    Ok((written, removed))
}

/// Deletes files under `dir` that the workspace no longer has. A build's `target` and `dist` at
/// the top of the copy are kept.
fn remove_stale(dir: &Path, top: bool, keep: &BTreeSet<PathBuf>) -> Result<usize, String> {
    let mut removed = 0usize;
    let entries = std::fs::read_dir(dir).map_err(|e| format!("couldn't list {} ({e})", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("couldn't list {} ({e})", dir.display()))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| format!("couldn't inspect {} ({e})", path.display()))?;
        if kind.is_dir() {
            if top && (entry.file_name() == "target" || entry.file_name() == "dist") {
                continue;
            }
            removed = removed.saturating_add(remove_stale(&path, false, keep)?);
        } else if !keep.contains(&path) {
            std::fs::remove_file(&path).map_err(|e| format!("couldn't remove {} ({e})", path.display()))?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

/// How the name is written where it's replaced.
#[derive(Clone, Copy)]
enum Flavor {
    /// Text with backslash escapes (Rust strings, the catalogs): `\nArtCraft Launcher` is a match.
    Escaped,
    /// Text where a backslash is a backslash (raw strings, plain text).
    Plain,
    /// XML: the name is escaped.
    Xml,
}

struct Rename {
    name: String,
    /// The exe and file-name stem.
    machine: String,
}

impl Rename {
    fn new(name: &str) -> Result<Self, String> {
        validate(name)?;
        Ok(Self { name: name.to_string(), machine: machine_name(name)? })
    }

    /// The staged content of the workspace file `rel`.
    fn file(&self, rel: &str, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        let rust_source = rel.ends_with(".rs") && (rel.starts_with("apps/artcraft-launcher/") || rel.starts_with("crates/"));
        let catalog = rel.starts_with("crates/ui-egui/src/i18n/") && rel.ends_with(".tsv");
        let installer = rel.starts_with("packaging/windows/") && (rel.ends_with(".wxs") || rel.ends_with(".wxl"));
        let edit: fn(&Self, &str) -> Result<String, String> = match rel {
            "apps/artcraft-launcher/Cargo.toml" => Self::app_manifest,
            "apps/artcraft-launcher/build.rs" => |r, t| Ok(r.exe(&r.rust_literals(t)?)),
            ".cargo/config.toml" => Self::cargo_config,
            "packaging/windows/package.ps1" => Self::package_script,
            "packaging/windows/portable.txt" => |r, t| Ok(r.exe(&r.replace(t, Flavor::Plain))),
            _ if rust_source => Self::rust_literals,
            _ if catalog => |r, t| Ok(r.replace(t, Flavor::Escaped)),
            _ if installer => |r, t| Ok(r.exe(&r.replace(t, Flavor::Xml))),
            _ => return Ok(bytes),
        };
        let text = String::from_utf8(bytes).map_err(|_| format!("{rel} isn't UTF-8"))?;
        edit(self, &text).map(String::into_bytes).map_err(|e| format!("{rel}: {e}"))
    }

    /// Replaces the name in `s` where it stands as the name, not as part of a path, a registry
    /// key, a dotted id (`ArtCraft Launcher.app`) or a longer word.
    fn replace(&self, s: &str, flavor: Flavor) -> String {
        let escaped = matches!(flavor, Flavor::Escaped);
        let to = match flavor {
            Flavor::Xml => self.name.replace('&', "&amp;").replace('\'', "&apos;"),
            Flavor::Escaped | Flavor::Plain => self.name.clone(),
        };
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for (i, m) in s.match_indices(ORIGINAL) {
            let end = i.saturating_add(m.len());
            if boundary_before(s, i, escaped) && boundary_after(s, end, escaped) && !credits(s, end) {
                out.push_str(s.get(last..i).unwrap_or_default());
                out.push_str(&to);
                last = end;
            }
        }
        out.push_str(s.get(last..).unwrap_or_default());
        out
    }

    /// Replaces the name only inside Rust text string literals (`"…"` and `r#"…"#`); comments,
    /// identifiers, byte and C strings and character literals are copied as they are.
    fn rust_literals(&self, src: &str) -> Result<String, String> {
        let b = src.as_bytes();
        let n = b.len();
        let at = |k: usize| b.get(k).copied().unwrap_or(0);
        let part = |from: usize, to: usize| src.get(from..to).ok_or_else(|| format!("bad slice {from}..{to}"));
        let find = |pat: &str, from: usize| src.get(from..).and_then(|s| s.find(pat)).map(|p| p.saturating_add(from));
        let mut out = String::with_capacity(n.saturating_add(64));
        let mut i = 0usize;
        while i < n {
            let c = at(i);
            let next = i.saturating_add(1);
            if c == b'/' && at(next) == b'/' {
                let end = find("\n", i).unwrap_or(n);
                out.push_str(part(i, end)?);
                i = end;
            } else if c == b'/' && at(next) == b'*' {
                let (mut depth, mut k) = (0usize, i);
                while k < n {
                    if at(k) == b'/' && at(k.saturating_add(1)) == b'*' {
                        depth = depth.saturating_add(1);
                        k = k.saturating_add(2);
                    } else if at(k) == b'*' && at(k.saturating_add(1)) == b'/' {
                        depth = depth.saturating_sub(1);
                        k = k.saturating_add(2);
                        if depth == 0 {
                            break;
                        }
                    } else {
                        k = k.saturating_add(1);
                    }
                }
                let k = k.min(n);
                out.push_str(part(i, k)?);
                i = k;
            } else if c == b'"' {
                let Some(end) = scan_string(b, next) else {
                    out.push_str(part(i, n)?);
                    break;
                };
                out.push('"');
                out.push_str(&self.replace(part(next, end.saturating_sub(1))?, Flavor::Escaped));
                out.push('"');
                i = end;
            } else if c == b'\'' {
                // A character literal ('x', '\n', '"') or a lifetime / label ('a).
                let end = if at(next) == b'\\' {
                    find("'", i.saturating_add(3)).map_or(n, |k| k.saturating_add(1))
                } else {
                    match src.get(next..).and_then(|s| s.chars().next()) {
                        Some(ch) if at(next.saturating_add(ch.len_utf8())) == b'\'' => next.saturating_add(ch.len_utf8()).saturating_add(1),
                        _ => next,
                    }
                };
                out.push_str(part(i, end)?);
                i = end;
            } else if is_ident_byte(c) && !c.is_ascii_digit() {
                let mut k = next;
                while k < n && is_ident_byte(at(k)) {
                    k = k.saturating_add(1);
                }
                let id = part(i, k)?;
                if matches!(id, "b" | "c" | "br" | "cr") && at(k) == b'"' {
                    let end = scan_string(b, k.saturating_add(1)).unwrap_or(n);
                    out.push_str(part(i, end)?);
                    i = end;
                    continue;
                }
                if id == "b" && at(k) == b'\'' {
                    let q = k.saturating_add(if at(k.saturating_add(1)) == b'\\' { 3 } else { 2 });
                    let end = find("'", q).map_or(n, |e| e.saturating_add(1));
                    out.push_str(part(i, end)?);
                    i = end;
                    continue;
                }
                if matches!(id, "r" | "br" | "cr") {
                    let mut h = k;
                    while at(h) == b'#' {
                        h = h.saturating_add(1);
                    }
                    if at(h) == b'"' {
                        let close = format!("\"{}", "#".repeat(h.saturating_sub(k)));
                        let body_start = h.saturating_add(1);
                        let Some(body_end) = find(&close, body_start) else {
                            out.push_str(part(i, n)?);
                            break;
                        };
                        let body = part(body_start, body_end)?;
                        out.push_str(part(i, body_start)?);
                        out.push_str(&if id == "r" { self.replace(body, Flavor::Plain) } else { body.to_string() });
                        out.push_str(&close);
                        i = body_end.saturating_add(close.len());
                        continue;
                    }
                }
                out.push_str(id);
                i = k;
            } else {
                // ASCII punctuation, whitespace or a digit (anything else starts an identifier).
                out.push(char::from(c));
                i = next;
            }
        }
        Ok(out)
    }

    /// `artcraft-launcher.exe` -> `<machine>.exe`.
    fn exe(&self, s: &str) -> String {
        if self.machine == PKG {
            return s.to_string();
        }
        let from = format!("{PKG}.exe");
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for (i, _) in s.match_indices(&from) {
            let standalone = s.get(..i).and_then(|before| before.chars().next_back()).is_none_or(|p| !(is_word(p) || p == '-'));
            if standalone {
                out.push_str(s.get(last..i).unwrap_or_default());
                out.push_str(&self.machine);
                out.push_str(".exe");
                last = i.saturating_add(from.len());
            }
        }
        out.push_str(s.get(last..).unwrap_or_default());
        out
    }

    /// Names the binary `<machine>` so cargo builds `<machine>.exe`.
    fn app_manifest(&self, text: &str) -> Result<String, String> {
        if self.machine == PKG {
            return Ok(text.to_string());
        }
        let mut t = replace_required(text, &format!("default-run = \"{PKG}\""), &format!("default-run = \"{}\"", self.machine))?;
        if !t.ends_with('\n') {
            t.push('\n');
        }
        t.push_str(&format!(
            "\n# The renamed build's exe (xtask/src/bin/rename-launcher.rs).\n[[bin]]\nname = \"{}\"\npath = \"src/main.rs\"\n",
            self.machine
        ));
        Ok(t)
    }

    /// Builds the renamed copy with self-update off (`crates/native/src/self_update.rs`).
    fn cargo_config(&self, text: &str) -> Result<String, String> {
        if text.lines().any(|l| l.trim() == "[env]") {
            return Err("already has an [env] table; rename-launcher needs to add ARTCRAFT_LAUNCHER_NO_SELF_UPDATE to it".into());
        }
        let mut t = text.to_string();
        if !t.ends_with('\n') {
            t.push('\n');
        }
        t.push_str(
            "\n# Renamed build (xtask/src/bin/rename-launcher.rs): never replace itself with the official release.\n\
             [env]\nARTCRAFT_LAUNCHER_NO_SELF_UPDATE = \"1\"\n",
        );
        Ok(t)
    }

    /// The display name, and the exe and installer file names.
    fn package_script(&self, text: &str) -> Result<String, String> {
        let mut t = replace_required(text, &format!("$AppName = '{ORIGINAL}'"), &format!("$AppName = '{}'", self.name.replace('\'', "''")))?;
        if self.machine != PKG {
            t = replace_required(&t, "$Exe = \"$Pkg.exe\"", &format!("$Exe = '{}.exe'", self.machine))?;
            t = replace_required(&t, "$Pkg-$Version-", &format!("{}-$Version-", self.machine))?;
        }
        Ok(t)
    }
}

/// Replaces every `from`; it must occur, or the file has changed under the renamer.
fn replace_required(text: &str, from: &str, to: &str) -> Result<String, String> {
    if !text.contains(from) {
        return Err(format!("expected `{from}`; update xtask/src/bin/rename-launcher.rs to match"));
    }
    Ok(text.replace(from, to))
}

/// The same rules as the names the Craft apps accept: printable, no surrounding spaces, and none of
/// the characters in [`FORBIDDEN`].
fn validate(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("the name is empty".into());
    }
    if name != name.trim() {
        return Err(format!("the name '{name}' starts or ends with a space"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("the name is longer than {MAX_NAME_CHARS} characters"));
    }
    if name.chars().any(|c| c.is_control() || FORBIDDEN.contains(c)) {
        return Err(format!("the name '{name}' contains a character that isn't allowed: \" \\ / : * ? < > | {{ }} [ ] %"));
    }
    Ok(())
}

/// The exe and file-name stem: `artcraft-launcher` while the name is unchanged, otherwise the name
/// in lower-case ASCII letters and digits ("My Launcher 2" -> `mylauncher2`).
fn machine_name(name: &str) -> Result<String, String> {
    if name == ORIGINAL {
        return Ok(PKG.into());
    }
    let machine: String = name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect();
    match machine.as_str() {
        "" => Ok(PKG.into()),
        "xtask" => Err("the name would give the exe the same name as the workspace's xtask tool; choose another".into()),
        _ => Ok(machine),
    }
}

/// The index just after the closing quote of a string whose body starts at `k`, if it's closed.
fn scan_string(b: &[u8], mut k: usize) -> Option<usize> {
    while let Some(&c) = b.get(k) {
        match c {
            b'\\' => k = k.saturating_add(2),
            b'"' => return Some(k.saturating_add(1)),
            _ => k = k.saturating_add(1),
        }
    }
    None
}

/// An identifier byte; any non-ASCII byte counts, so a Unicode identifier is copied whole.
fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn boundary_before(s: &str, i: usize, escaped: bool) -> bool {
    let before = s.get(..i).unwrap_or_default();
    let mut rev = before.chars().rev();
    let Some(p) = rev.next() else { return true };
    // `\n`, `\r`, `\t` or `\0` just before: an escape, not part of a word.
    if escaped && matches!(p, 'n' | 'r' | 't' | '0') {
        let run = before.strip_suffix(p).unwrap_or_default().bytes().rev().take_while(|&b| b == b'\\').count();
        if run % 2 == 1 {
            return true;
        }
    }
    if is_word(p) || "\\/%#$@<".contains(p) {
        return false; // part of a word, a path, a registry key or a marker
    }
    !(p == '.' && rev.next().is_some_and(is_word)) // a dotted id
}

/// "the ArtCraft Launcher authors" and "contributors" (the exe's copyright line) keep the original
/// name: they say who wrote it.
fn credits(s: &str, j: usize) -> bool {
    s.get(j..).is_some_and(|rest| rest.starts_with(" authors") || rest.starts_with(" contributors"))
}

fn boundary_after(s: &str, j: usize, escaped: bool) -> bool {
    let mut it = s.get(j..).unwrap_or_default().chars();
    let Some(c) = it.next() else { return true };
    if c == '\\' {
        // An escape such as `\n` or `\"` ends the name; a real backslash continues a path.
        return escaped && it.next().is_some_and(|e| "nrt0\"'u".contains(e));
    }
    if is_word(c) || c == '/' || c == '%' {
        return false;
    }
    !(c == '.' && it.next().is_some_and(char::is_alphabetic)) // `ArtCraft Launcher.app`
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename() -> Rename {
        Rename::new("My Launcher").unwrap()
    }

    #[test]
    fn renames_rust_string_literals_only() {
        let src = r##"//! ArtCraft Launcher: install, update and open the Craft App Suite.
/// The `ArtCraft Launcher.app` bundle.
const DIR: &str = "ArtCraft Launcher";
let t = format!("ArtCraft Launcher {}", v); /* "ArtCraft Launcher" in a comment */
let k = r"Software\Learning Machines LLC\ArtCraft Launcher";
let raw = r#"Quit ArtCraft Launcher"#;
let bytes = b"ArtCraft Launcher";
let app = "ArtCraft Launcher.app";
let mac = "Contents/MacOS/ArtCraft Launcher";
let desk = "[Desktop Entry]\nName=ArtCraft Launcher\nComment=x";
let q = '"'; let s = "ArtCraft Launcher · {n} updates"; fn f<'a>(x: &'a str) {}
let c = "Copyright (c) the ArtCraft Launcher authors. MIT OR Apache-2.0.";
"##;
        let want = r##"//! ArtCraft Launcher: install, update and open the Craft App Suite.
/// The `ArtCraft Launcher.app` bundle.
const DIR: &str = "My Launcher";
let t = format!("My Launcher {}", v); /* "ArtCraft Launcher" in a comment */
let k = r"Software\Learning Machines LLC\ArtCraft Launcher";
let raw = r#"Quit My Launcher"#;
let bytes = b"ArtCraft Launcher";
let app = "ArtCraft Launcher.app";
let mac = "Contents/MacOS/ArtCraft Launcher";
let desk = "[Desktop Entry]\nName=My Launcher\nComment=x";
let q = '"'; let s = "My Launcher · {n} updates"; fn f<'a>(x: &'a str) {}
let c = "Copyright (c) the ArtCraft Launcher authors. MIT OR Apache-2.0.";
"##;
        assert_eq!(rename().rust_literals(src).unwrap(), want);
    }

    #[test]
    fn renames_both_columns_of_a_catalog_row() {
        let row = "\tArtCraft Launcher {version} is available.\tДоступен ArtCraft Launcher {version}.\n";
        assert_eq!(rename().replace(row, Flavor::Escaped), "\tMy Launcher {version} is available.\tДоступен My Launcher {version}.\n");
    }

    #[test]
    fn renames_the_installer_but_not_its_registry_key() {
        let r = Rename::new("Tom & Jerry's Launcher").unwrap();
        let wxs = r#"<Package Name="ArtCraft Launcher" /><RegistrySearch Key="Software\Learning Machines LLC\ArtCraft Launcher" />
<File Source="$(var.BinDir)\artcraft-launcher.exe" /><RegistryValue Key="Software\Microsoft\Windows\CurrentVersion\App Paths\artcraft-launcher.exe" />"#;
        let want = r#"<Package Name="Tom &amp; Jerry&apos;s Launcher" /><RegistrySearch Key="Software\Learning Machines LLC\ArtCraft Launcher" />
<File Source="$(var.BinDir)\tomjerryslauncher.exe" /><RegistryValue Key="Software\Microsoft\Windows\CurrentVersion\App Paths\tomjerryslauncher.exe" />"#;
        assert_eq!(r.exe(&r.replace(wxs, Flavor::Xml)), want);
        assert_eq!(r.exe("my-artcraft-launcher.exe"), "my-artcraft-launcher.exe");
    }

    #[test]
    fn checks_names() {
        for ok in ["My Launcher", "Photo & Light's Launcher!", "Лаунчер", "启动器"] {
            assert!(validate(ok).is_ok(), "{ok}");
        }
        for bad in ["", "   ", " Lead", "Trail ", "Brace{s}", "a/b", "50%", "Tab\there", &"x".repeat(61)] {
            assert!(validate(bad).is_err(), "{bad:?}");
        }
        assert_eq!(machine_name("My Launcher 2").unwrap(), "mylauncher2");
        assert_eq!(machine_name(ORIGINAL).unwrap(), PKG);
        assert_eq!(machine_name("启动器").unwrap(), PKG);
        assert!(machine_name("X Task").is_err());
    }

    /// The files the renamer edits by anchor still have those anchors, and the result is what the
    /// Windows packaging expects.
    #[test]
    fn the_workspace_files_it_edits_still_match() {
        let r = rename();
        let read = |rel: &str| std::fs::read(root().join(rel_path(rel))).unwrap();
        let staged = |rel: &str| String::from_utf8(r.file(rel, read(rel)).unwrap()).unwrap();

        let manifest = staged("apps/artcraft-launcher/Cargo.toml");
        assert!(manifest.contains("default-run = \"mylauncher\"") && manifest.contains("[[bin]]\nname = \"mylauncher\""));
        assert!(staged(".cargo/config.toml").contains("[env]\nARTCRAFT_LAUNCHER_NO_SELF_UPDATE = \"1\""));
        let ps1 = staged("packaging/windows/package.ps1");
        assert!(ps1.contains("$AppName = 'My Launcher'") && ps1.contains("$Exe = 'mylauncher.exe'"));
        assert!(ps1.contains("\"$Pkg.wxs\"") && !ps1.contains("$Pkg-$Version-"));
        let wxs = staged("packaging/windows/artcraft-launcher.wxs");
        assert!(wxs.contains("Name=\"My Launcher\"") && wxs.contains("\\mylauncher.exe\"") && !wxs.contains("artcraft-launcher.exe"));
        assert!(wxs.contains("Key=\"Software\\Learning Machines LLC\\ArtCraft Launcher\""));
        assert!(staged("apps/artcraft-launcher/src/main.rs").contains(".with_title(\"My Launcher\")"));
        // A key the code looks up is renamed in the catalogs too.
        assert!(staged("apps/artcraft-launcher/src/tray.rs").contains("\"Quit My Launcher\""));
        assert!(staged("crates/ui-egui/src/i18n/ru.tsv").contains("\tQuit My Launcher\t"));
        // Untouched files are copied byte for byte.
        assert_eq!(r.file("README.md", read("README.md")).unwrap(), read("README.md"));
    }
}
