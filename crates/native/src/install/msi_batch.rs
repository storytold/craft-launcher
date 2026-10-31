//! Helpers for Windows Installer PowerShell scripts running under elevation.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

use super::MsiScope;

/// What PowerShell exits with when the permission prompt is declined (`ERROR_CANCELLED`).
pub(super) const DECLINED: i32 = 1223;

/// What PowerShell exits with when the elevated session could not be started for any other
/// reason. Bit 29 is the "customer" bit, which no Windows or Windows Installer error code sets,
/// so it cannot be mistaken for an exit code from `msiexec`.
pub(super) const LAUNCH_FAILED: i32 = 0x2000_0001;

/// `path` as text that can sit inside quotes on a command line.
pub(super) fn text(path: &Path) -> Result<&str, String> {
    let text = path.to_str().ok_or("unusual characters in a path")?;
    if text.chars().any(|c| c == '"' || c.is_control()) {
        return Err(format!("couldn't use {text}: the path contains invalid characters"));
    }
    Ok(text)
}

/// The `msiexec` arguments for one package, the same ones `msi_command` and the folder and scope
/// arguments give the single-app route, as one command line.
pub(super) fn line(msi: &Path, destination: Option<&Path>, scope: Option<MsiScope>) -> Result<String, String> {
    let msi = super::msi_path(msi)?;
    let log = super::msi_log(&msi);
    let mut line = format!("/i \"{}\" /passive /norestart /l*v \"{}\"", text(&msi)?, text(&log)?);
    if let Some(folder) = destination {
        line.push(' ');
        line.push_str(&super::msi_folder_argument(folder)?);
    }
    for argument in super::msi_scope_arguments(scope) {
        line.push(' ');
        line.push_str(argument);
    }
    Ok(line)
}

/// `text` as a PowerShell single-quoted string. PowerShell also reads the typographic single
/// quotes as quotes, so those are doubled too.
pub(super) fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// What the launcher runs without elevation: it starts the elevated session (the one prompt) and
/// waits for it. Start-Process throws when the prompt is declined and when the session cannot
/// start at all; [`launch_catch`] tells the two apart.
pub(super) fn outer_script(powershell: &str, encoded: &str) -> String {
    format!(
        "try {{ $elevated = Start-Process -FilePath {} -Verb RunAs -WindowStyle Hidden -Wait -PassThru \
         -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'; exit $elevated.ExitCode }} \
         catch {{ {} }}",
        quoted(powershell),
        launch_catch()
    )
}

/// The `catch` body for a failed Start-Process: [`DECLINED`] when a Windows error "cancelled by the
/// user" (1223) is anywhere in the exception chain, which is what a declined prompt throws, and
/// [`LAUNCH_FAILED`] for anything else, so the launcher falls back instead of cancelling.
fn launch_catch() -> String {
    format!(
        "$e = $_.Exception; while ($e) {{ if ($e -is [System.ComponentModel.Win32Exception] -and $e.NativeErrorCode -eq {DECLINED}) {{ exit {DECLINED} }}; \
         $e = $e.InnerException }}; exit {LAUNCH_FAILED}"
    )
}

/// A checked package, kept exactly as checked until its installer runs.
///
/// The download is checked against its release checksum before it is renamed into the download
/// folder, but an elevated session runs it later from that user-writable folder. The prompt for
/// the session names PowerShell, not the package's publisher, so a file swapped in between would go
/// unnoticed (CWE-367). Holding the file open while sharing only reading stops anyone changing,
/// moving or deleting it; hashing it again through that handle proves the held bytes are the
/// checked ones. `msiexec` can still open it to read.
#[cfg(windows)]
pub(super) struct Held {
    _file: std::fs::File,
}

/// Opens `msi` so that it cannot be changed, moved or deleted while held, and checks that it still
/// has the `sha256` it was downloaded against.
#[cfg(windows)]
pub(super) fn hold(msi: &Path, sha256: &str) -> Result<Held, String> {
    use std::os::windows::fs::OpenOptionsExt;

    use sha2::{Digest, Sha256};

    /// Others may read the file, but not write, rename or delete it.
    const FILE_SHARE_READ: u32 = 0x1;
    let mut file = std::fs::File::options()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(msi)
        .map_err(|e| format!("couldn't keep the checked package {} safe until it is installed ({e})", msi.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("couldn't read {} ({e})", msi.display()))?;
    let digest: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    if !digest.eq_ignore_ascii_case(sha256.trim()) {
        return Err(format!("{} changed after it was checked, so it wasn't installed. Try again.", msi.display()));
    }
    Ok(Held { _file: file })
}

/// The base64 form of `script` that `-EncodedCommand` takes: UTF-16 little-endian, so no quoting
/// can go wrong however odd a path is.
pub(super) fn encoded(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |at: usize| u32::from(chunk.get(at).copied().unwrap_or(0));
        let bits = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for (position, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if position > chunk.len() {
                out.push('=');
            } else if let Some(letter) = usize::try_from((bits >> shift) & 63).ok().and_then(|index| ALPHABET.get(index)) {
                out.push(char::from(*letter));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
