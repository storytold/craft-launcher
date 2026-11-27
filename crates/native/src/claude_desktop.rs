//! Restarting Claude Desktop on Windows through Windows' own process list and Start menu, without
//! starting a script host (see docs/incident-2026-10-windows-antivirus.md).
//!
//! Its processes are picked by install folder (the Microsoft Store package or the classic per-user
//! install), never by name alone: `claude.exe` is also Claude Code's command-line program, whose
//! sessions must keep running. It is started again through its Start menu entry, which works for
//! both installs.

/// The program name every Claude Desktop process runs as.
const EXE: &str = "claude.exe";

/// The Start menu name of Claude Desktop.
const START_MENU_NAME: &str = "Claude";

/// The install a running `claude.exe` belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Install {
    /// The Microsoft Store package. `family` is its package family name (`Claude_<publisher id>`),
    /// taken from the package folder `...\WindowsApps\<package full name>`.
    Store { family: String },
    /// The classic per-user install, under `...\AnthropicClaude`.
    Classic,
}

/// The Claude Desktop install that `image`, a process's full program path, was started from.
/// `None` for any other program, Claude Code's `claude.exe` included.
pub(crate) fn install_of(image: &str) -> Option<Install> {
    let parts: Vec<&str> = image.split(['\\', '/']).collect();
    let (file, dirs) = parts.split_last()?;
    if !file.eq_ignore_ascii_case(EXE) {
        return None;
    }
    for (i, dir) in dirs.iter().enumerate() {
        if dir.eq_ignore_ascii_case("WindowsApps")
            && let Some(family) = dirs.get(i + 1).and_then(|package| family_name(package))
        {
            return Some(Install::Store { family });
        }
        if dir.eq_ignore_ascii_case("AnthropicClaude") {
            return Some(Install::Classic);
        }
    }
    None
}

/// The package family name (`Name_PublisherId`) of Claude Desktop's package full name
/// (`Name_Version_Architecture_ResourceId_PublisherId`), or `None` for another package. Package
/// names can't contain `_`, so the full name has exactly five parts.
fn family_name(full_name: &str) -> Option<String> {
    let parts: Vec<&str> = full_name.split('_').collect();
    let [name, version, _architecture, _resource, publisher] = parts.as_slice() else { return None };
    let valid = name.eq_ignore_ascii_case(START_MENU_NAME)
        && !version.is_empty()
        && !publisher.is_empty()
        && publisher.chars().all(|c| c.is_ascii_alphanumeric());
    valid.then(|| format!("{name}_{publisher}"))
}

/// The Start menu entry to start Claude Desktop with: of `entries` (each a display name and the id
/// that follows `shell:AppsFolder\`), one named Claude. With both installs present, the one that was
/// running wins: a Store entry's id is `<package family name>!<app id>`, and a classic one has no `!`.
pub(crate) fn start_entry<'a>(entries: &'a [(String, String)], running: Option<&Install>) -> Option<&'a str> {
    let mut named = entries.iter().filter(|(name, id)| name == START_MENU_NAME && !id.is_empty()).map(|(_, id)| id.as_str());
    let first = named.clone().next()?;
    let matches = |id: &&str| match running {
        Some(Install::Store { family }) => id.split_once('!').is_some_and(|(f, _)| f.eq_ignore_ascii_case(family)),
        Some(Install::Classic) => !id.contains('!'),
        None => false,
    };
    Some(named.find(matches).unwrap_or(first))
}

/// How long the stopped processes get to exit before Claude Desktop is started again.
#[cfg(windows)]
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Quit Claude Desktop and start it again, so it loads the MCP servers in its configuration.
#[cfg(windows)]
pub(crate) fn restart() -> Result<(), String> {
    use winsafe::co;

    // The Start menu is read and the app started through the shell, which needs COM on this thread.
    let com = winsafe::CoInitializeEx(co::COINIT::APARTMENTTHREADED | co::COINIT::DISABLE_OLE1DDE)
        .map_err(|e| format!("couldn't read the Start menu ({e})"))?;
    if com.hr() == co::HRESULT::RPC_E_CHANGED_MODE {
        // COM was already set up on this thread in another mode, and this call didn't add a
        // reference that CoUninitialize should drop.
        std::mem::forget(com);
    }

    // Found before anything is stopped, so a missing entry leaves Claude Desktop running.
    let running = running()?;
    let entries = start_menu_entries()?;
    let first_install = running.first().map(|(install, _)| install);
    let id = start_entry(&entries, first_install).ok_or("Claude Desktop is not in the Start menu")?.to_string();

    for (_, process) in &running {
        if let Err(e) = process.TerminateProcess(1) {
            log::warn!("couldn't stop a Claude Desktop process: {e}");
        }
    }
    let deadline = std::time::Instant::now() + EXIT_WAIT;
    for (_, process) in &running {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        // u32::MAX would mean INFINITE.
        let ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX - 1);
        if !process.WaitForSingleObject(Some(ms)).is_ok_and(|w| w == co::WAIT::OBJECT_0) {
            return Err("Claude Desktop didn't close within 10 seconds, so it wasn't started again".into());
        }
    }

    let target = format!(r"shell:AppsFolder\{id}");
    // NOASYNC: this thread ends right after, so the shell must finish starting it before returning.
    winsafe::ShellExecuteEx(&winsafe::SHELLEXECUTEINFO {
        mask: co::SEE_MASK::NOASYNC,
        verb: Some("open"),
        file: &target,
        show: co::SW::SHOWNORMAL,
        ..Default::default()
    })
    .map_err(|e| format!("couldn't start Claude Desktop ({e})"))
}

/// This account's running Claude Desktop processes, each with its install and a handle that can
/// stop it and wait for it. Holding the handle keeps a process id from being reused before the
/// process is stopped.
#[cfg(windows)]
fn running() -> Result<Vec<(Install, winsafe::guard::CloseHandleGuard<winsafe::HPROCESS>)>, String> {
    use winsafe::{HPROCESS, HPROCESSLIST, co};

    let failed = |e: co::ERROR| format!("couldn't list the running programs ({e})");
    let mut list = HPROCESSLIST::CreateToolhelp32Snapshot(co::TH32CS::SNAPPROCESS, None).map_err(failed)?;
    let mut found = Vec::new();
    for entry in list.iter_processes() {
        let entry = entry.map_err(failed)?;
        if !entry.szExeFile().eq_ignore_ascii_case(EXE) {
            continue;
        }
        let access = co::PROCESS::QUERY_LIMITED_INFORMATION | co::PROCESS::TERMINATE | co::PROCESS::SYNCHRONIZE;
        // Another account's process, or one that has just exited, can't be opened: not ours to stop.
        let Ok(process) = HPROCESS::OpenProcess(access, false, entry.th32ProcessID) else { continue };
        let Ok(image) = process.QueryFullProcessImageName(co::PROCESS_NAME::WIN32) else { continue };
        if let Some(install) = install_of(&image) {
            found.push((install, process));
        }
    }
    Ok(found)
}

/// The Start menu's apps named Claude (from the shell's Applications folder), each as its display
/// name and the id that follows `shell:AppsFolder\`. COM must be set up on this thread.
#[cfg(windows)]
fn start_menu_entries() -> Result<Vec<(String, String)>, String> {
    use winsafe::{IBindCtx, IEnumShellItems, IShellItem, co, prelude::*};

    let failed = |e: co::HRESULT| format!("couldn't read the Start menu ({e})");
    let folder = winsafe::SHCreateItemFromParsingName::<IShellItem>("shell:AppsFolder", None::<&IBindCtx>).map_err(failed)?;
    let items = folder.BindToHandler::<IEnumShellItems>(None::<&IBindCtx>, &co::BHID::EnumItems).map_err(failed)?;
    let mut entries = Vec::new();
    for item in items.iter() {
        let item = item.map_err(failed)?;
        let Ok(name) = item.GetDisplayName(co::SIGDN::NORMALDISPLAY) else { continue };
        if name != START_MENU_NAME {
            continue;
        }
        if let Ok(id) = item.GetDisplayName(co::SIGDN::PARENTRELATIVEPARSING) {
            entries.push((name, id));
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: &str = r"C:\Program Files\WindowsApps\Claude_0.14.10.0_x64__pzs8sxrjxfjjc\app\claude.exe";

    fn store() -> Install {
        Install::Store { family: "Claude_pzs8sxrjxfjjc".into() }
    }

    #[test]
    fn the_store_package_and_the_classic_install_are_claude_desktop() {
        assert_eq!(install_of(STORE), Some(store()));
        assert_eq!(install_of(&STORE.to_ascii_lowercase()), Some(Install::Store { family: "claude_pzs8sxrjxfjjc".into() }));
        assert_eq!(install_of(r"C:\Users\A B\AppData\Local\AnthropicClaude\app-0.14.10\claude.exe"), Some(Install::Classic));
        assert_eq!(install_of(r"C:\Users\a\AppData\Local\AnthropicClaude\Claude.exe"), Some(Install::Classic));
    }

    /// Claude Code's `claude.exe` keeps running: it is never under either install folder, even when
    /// it sits in Claude Desktop's data folders.
    #[test]
    fn claude_code_and_other_programs_are_left_alone() {
        for image in [
            r"C:\Users\a\.local\bin\claude.exe",
            r"C:\Users\a\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\claude.exe",
            r"C:\Users\a\AppData\Roaming\Claude\claude-code\2.0.1\claude.exe",
            r"C:\Users\a\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude\claude-code\2.0.1\claude.exe",
            r"C:\Program Files\WindowsApps\ClaudeCode_1.0.0.0_x64__pzs8sxrjxfjjc\claude.exe",
            r"C:\Program Files\WindowsApps\Claude_0.14.10.0_x64__pzs8sxrjxfjjc\app\crashpad_handler.exe",
            r"C:\Program Files\WindowsApps\claude.exe",
            r"C:\Users\a\AppData\Local\AnthropicClaude\Update.exe",
            "",
        ] {
            assert_eq!(install_of(image), None, "{image}");
        }
    }

    #[test]
    fn the_running_install_picks_its_own_start_menu_entry() {
        let entry = |name: &str, id: &str| (name.to_string(), id.to_string());
        let classic = "com.squirrel.AnthropicClaude.claude";
        let packaged = "Claude_pzs8sxrjxfjjc!Claude";
        let both = [entry("Claude Code", "x"), entry("Claude", classic), entry("Claude", packaged)];
        assert_eq!(start_entry(&both, Some(&store())), Some(packaged));
        assert_eq!(start_entry(&both, Some(&Install::Classic)), Some(classic));
        assert_eq!(start_entry(&both, None), Some(classic));
        assert_eq!(start_entry(&[entry("Claude", packaged)], Some(&Install::Classic)), Some(packaged));
        assert_eq!(start_entry(&[entry("Claude Code", "x"), entry("Claude", "")], None), None);
    }
}
