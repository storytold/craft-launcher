//! Giving an installed Craft App the icon the user picked, outside the launcher too.
//!
//! The launcher's own grid draws the bundled artwork directly (`ui_egui::icons`); this is what
//! makes the *installed* app wear it as well — in the Dock, the taskbar, the Start menu, the
//! desktop and the menu. Every platform keeps that answer somewhere different:
//!
//! - **Linux and FreeBSD**: the freedesktop icon theme. A user-level copy of the app's icon under
//!   `$XDG_DATA_HOME/icons/hicolor/` shadows the system's, and every `.desktop` entry for a Craft
//!   App already names its icon `ai.storyteller.<app>` (the bundle id), so no entry has to be
//!   rewritten. Inside the launcher's own Flatpak the host's data folder is used.
//! - **macOS**: `NSWorkspace`'s custom file icon — the same thing Finder's Get Info → paste icon
//!   sets. The bundle's `Contents/Resources` is never touched, so its code signature stays valid.
//! - **Windows**: the icon of the app's shortcuts (Start menu, desktop, taskbar pin), through
//!   PowerShell's `WScript.Shell`. The exe's own embedded icon is never touched.
//!
//! [`apply`] with [`IconStyle::Default`] puts the app's own icon back and takes away everything an
//! earlier call wrote. Nothing here is fatal to an install: the caller logs a failure and carries
//! on.

use artcraft_launcher_core::{CraftApp, IconStyle, Installed};

/// The bundled artwork for `app` in `style`: `None` for [`IconStyle::Default`] (the app's own icon
/// belongs to the app, not to us) and for an app the pack wasn't drawn for. Where the packs and
/// their artwork are is [`artcraft_launcher_core::icons`].
fn artwork(app: &CraftApp, style: IconStyle) -> Option<artcraft_launcher_core::icons::Artwork> {
    artcraft_launcher_core::icons::artwork(app.id, style.pack()?)
}

/// Give `app` the icon `style` everywhere this machine looks for one. [`IconStyle::Default`] puts
/// the app's own icon back. `installed` is where the app is, for the platforms that need to reach
/// it (macOS and Windows); `paths` is where the launcher keeps its own files.
pub fn apply(app: &CraftApp, style: IconStyle, installed: Option<&Installed>, paths: &crate::paths::Paths) -> Result<(), String> {
    let style = if app.supports_icon_style(style) { style } else { IconStyle::Default };
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let _ = (installed, paths);
        return theme::apply(app, style);
    }
    #[cfg(target_os = "macos")]
    return macos::apply(app, style, installed);
    #[cfg(windows)]
    return windows::apply(app, style, installed, paths);
    #[allow(unreachable_code)]
    {
        let _ = (app, style, installed, paths);
        Err("changing an app's icon isn't supported here".into())
    }
}

/// The app's own icon, as bundled in `assets/app-icons` (the 128 px PNG each app's repository
/// publishes). `None` for a name that isn't a Craft App.
pub fn own_png(id: &str) -> Option<&'static [u8]> {
    Some(match id {
        "photocraft" => include_bytes!("../../../assets/app-icons/photocraft.png"),
        "vectorcraft" => include_bytes!("../../../assets/app-icons/vectorcraft.png"),
        "filmcraft" => include_bytes!("../../../assets/app-icons/filmcraft.png"),
        "lightcraft" => include_bytes!("../../../assets/app-icons/lightcraft.png"),
        "pdfcraft" => include_bytes!("../../../assets/app-icons/pdfcraft.png"),
        "effectcraft" => include_bytes!("../../../assets/app-icons/effectcraft.png"),
        "designcraft" => include_bytes!("../../../assets/app-icons/designcraft.png"),
        "wordcraft" => include_bytes!("../../../assets/app-icons/wordcraft.png"),
        "deckcraft" => include_bytes!("../../../assets/app-icons/deckcraft.png"),
        "gridcraft" => include_bytes!("../../../assets/app-icons/gridcraft.png"),
        "cadcraft" => include_bytes!("../../../assets/app-icons/cadcraft.png"),
        "soundcraft" => include_bytes!("../../../assets/app-icons/soundcraft.png"),
        "figcraft" => include_bytes!("../../../assets/app-icons/figcraft.png"),
        _ => return None,
    })
}

/// The master render, decoded. Resizing it to each size an OS asks for reads it once.
#[cfg(any(target_os = "linux", target_os = "freebsd", windows))]
fn decoded(png: &[u8]) -> Result<image::RgbaImage, String> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(|e| format!("the bundled icon is unreadable ({e})"))?;
    Ok(image.to_rgba8())
}

/// `source` at `size` pixels square, as a PNG.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn png_of(source: &image::RgbaImage, size: u32) -> Result<Vec<u8>, String> {
    let small = image::imageops::resize(source, size, size, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    small.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| format!("couldn't write a {size} px icon ({e})"))?;
    Ok(out)
}

/// Linux and FreeBSD: the freedesktop icon theme.
///
/// A user-level copy of an app's icon under `$XDG_DATA_HOME/icons/hicolor/` shadows the system's
/// for every program that looks one up — the dock, the application menu, the window list — because
/// `$XDG_DATA_HOME` comes first in the icon-theme search order. Every Craft App's `.desktop` entry
/// already names its icon `ai.storyteller.<app>` (the bundle id), so no entry has to be rewritten.
///
/// The 128 px folder is the one the launcher's own AppImage integration fills, so going back to
/// [`IconStyle::Default`] puts the app's own icon back there rather than leaving a hole.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod theme {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use artcraft_launcher_core::{CraftApp, IconStyle};

    use super::{artwork, decoded, own_png, png_of};

    /// The sizes the theme's fixed folders are filled at. The master render is 512 px, so nothing
    /// is ever enlarged.
    const SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];
    /// The one folder [`super::install`]'s AppImage integration writes to.
    const OWN_SIZE: u32 = 128;

    /// Where the launcher's own menu entries and icons go: `$XDG_DATA_HOME`, else
    /// `~/.local/share` — the host's, inside the launcher's own Flatpak.
    fn data_dir() -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        return crate::install::linux::host_data_dir();
        #[cfg(not(target_os = "linux"))]
        dirs::data_dir()
    }

    /// The icon-theme folder for each size, named after the app's bundle id.
    fn sized_files(data: &Path, app: &CraftApp) -> Vec<(u32, PathBuf)> {
        let name = app.bundle_id();
        SIZES.iter().map(|size| (*size, data.join(format!("icons/hicolor/{size}x{size}/apps/{name}.png")))).collect()
    }

    /// The `scalable` folder's entry, which every theme reads at any size.
    fn scalable_file(data: &Path, app: &CraftApp) -> PathBuf {
        data.join("icons/hicolor/scalable/apps").join(format!("{}.svg", app.bundle_id()))
    }

    pub fn apply(app: &CraftApp, style: IconStyle) -> Result<(), String> {
        let data = data_dir().ok_or("no per-user data folder on this system")?;
        apply_into(&data, app, style)
    }

    /// [`apply`] into the data folder `data`.
    pub(crate) fn apply_into(data: &Path, app: &CraftApp, style: IconStyle) -> Result<(), String> {
        let art = artwork(app, style);
        let source = art.as_ref().map(|art| decoded(art.png)).transpose()?;
        for (size, path) in sized_files(data, app) {
            match &source {
                Some(source) => crate::paths::write_atomic(&path, &png_of(source, size)?)?,
                None if size == OWN_SIZE => {
                    if let Some(png) = own_png(app.id) {
                        crate::paths::write_atomic(&path, png)?;
                    }
                }
                // Only ever ours: the app's own icon doesn't live in these folders.
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        let scalable = scalable_file(data, app);
        match &art {
            Some(art) => crate::paths::write_atomic(&scalable, art.svg.as_bytes())?,
            None => {
                let _ = std::fs::remove_file(&scalable);
            }
        }
        refresh(data);
        Ok(())
    }

    /// Ask the desktop to look at the icon theme again, so the change shows at once instead of
    /// after the next sign-in. Best effort: the icons on disk are already right, and most desktops
    /// watch the folders themselves.
    fn refresh(data: &Path) {
        let hicolor = data.join("icons/hicolor");
        if !hicolor.is_dir() {
            return;
        }
        let Some(dir) = hicolor.to_str() else { return };
        let _ = crate::process::capture("gtk-update-icon-cache", &["-f", "-t", dir], Duration::from_secs(20));
    }
}

/// macOS: `NSWorkspace`'s custom file icon — exactly what dragging a picture onto Finder's
/// Get Info window sets, and what the Dock, the app switcher and the Finder all show.
///
/// The bundle's `Contents/Resources` is never written to, so its code signature (and the
/// notarization ticket stapled to it) stays valid: a custom icon is Finder metadata beside the
/// bundle, not part of it. A `None` image clears it again, putting the bundle's own icon back.
#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;

    use artcraft_launcher_core::{CraftApp, IconStyle, Installed};
    use objc2::AllocAnyThread;
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSImage, NSWorkspace, NSWorkspaceIconCreationOptions};
    use objc2_foundation::{NSData, NSString};

    use super::artwork;

    /// The largest icon macOS shows is a 256 pt Dock icon at 2x, so the master render is passed
    /// through as it is.
    pub fn apply(app: &CraftApp, style: IconStyle, installed: Option<&Installed>) -> Result<(), String> {
        let Some(bundle) = installed.map(|i| Path::new(&i.location)) else { return Ok(()) };
        if !bundle.exists() {
            return Ok(());
        }
        let path = bundle.to_str().ok_or("unusual characters in the app's path")?;
        let png = artwork(app, style).map(|art| art.png);
        let changed = autoreleasepool(|_| {
            let image = png.map(NSData::with_bytes).and_then(|data| NSImage::initWithData(NSImage::alloc(), &data));
            NSWorkspace::sharedWorkspace().setIcon_forFile_options(
                image.as_deref(),
                &NSString::from_str(path),
                NSWorkspaceIconCreationOptions::empty(),
            )
        });
        if changed { Ok(()) } else { Err(format!("macOS wouldn't change the icon of {}", bundle.display())) }
    }
}

/// Windows: the icon of the app's shortcuts.
///
/// Windows takes a window's taskbar icon from its exe, so the exe is never touched; what changes is
/// the icon each `.lnk` points at — the Start menu entry, a desktop shortcut, a taskbar pin. The
/// work goes through PowerShell's `WScript.Shell`, because reading and writing a `.lnk` needs COM
/// and the workspace forbids `unsafe`.
///
/// Going back to [`IconStyle::Default`] points the shortcuts at the exe again, which is where a
/// Craft App's own installer puts them in the first place, so nothing has to be remembered.
#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use artcraft_launcher_core::{CraftApp, IconStyle, Installed};

    use super::{artwork, decoded};

    /// The sizes a Windows `.ico` carries. 256 px is the largest an icon entry may be.
    const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

    pub fn apply(app: &CraftApp, style: IconStyle, installed: Option<&Installed>, paths: &crate::paths::Paths) -> Result<(), String> {
        let Some(recorded) = installed.map(|i| PathBuf::from(&i.location)) else { return Ok(()) };
        // Detection records the exe on Windows and the folder for a portable copy; take either.
        let (exe, folder) = if recorded.is_dir() {
            (recorded.join(format!("{}.exe", app.id)), recorded.clone())
        } else {
            (recorded.clone(), recorded.parent().map_or_else(|| recorded.clone(), Path::to_path_buf))
        };
        if !folder.is_dir() {
            return Ok(());
        }
        let icon = match artwork(app, style) {
            Some(art) => {
                let file = paths.data.join("icons").join(format!("{}.ico", app.id));
                crate::paths::write_atomic(&file, &ico(art.png)?)?;
                file
            }
            // Back to the exe's own embedded icon, where the app's installer points the shortcuts.
            None => exe.clone(),
        };
        let script = script(&exe, &folder, &icon, &shortcut_roots());
        crate::process::run("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script], Duration::from_secs(180))?;
        // The shell holds shortcut icons in memory; this makes it read them again now rather than
        // at the next sign-in. Best effort.
        let _ = crate::process::capture("ie4uinit.exe", &["-show"], Duration::from_secs(30));
        Ok(())
    }

    /// The folders Windows keeps an app's shortcuts in: the Start menu (this user's, and the
    /// machine's), the desktop (this user's, and the shared one) and the taskbar's pins.
    fn shortcut_roots() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            roots.push(appdata.join("Microsoft/Windows/Start Menu/Programs"));
            roots.push(appdata.join("Microsoft/Internet Explorer/Quick Launch/User Pinned/TaskBar"));
        }
        if let Some(program_data) = std::env::var_os("ProgramData").map(PathBuf::from) {
            roots.push(program_data.join("Microsoft/Windows/Start Menu/Programs"));
        }
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Desktop"));
        }
        if let Some(public) = std::env::var_os("PUBLIC").map(PathBuf::from) {
            roots.push(public.join("Desktop"));
        }
        roots
    }

    /// A `.ico` holding every size Windows might ask for.
    fn ico(png: &[u8]) -> Result<Vec<u8>, String> {
        let source = decoded(png)?;
        let frames = ICO_SIZES
            .iter()
            .map(|size| {
                let small = image::imageops::resize(&source, *size, *size, image::imageops::FilterType::Lanczos3);
                image::codecs::ico::IcoFrame::as_png(small.as_raw(), *size, *size, image::ExtendedColorType::Rgba8)
                    .map_err(|e| format!("couldn't build the {size} px icon ({e})"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        image::codecs::ico::IcoEncoder::new(&mut out).encode_images(&frames).map_err(|e| format!("couldn't write the .ico ({e})"))?;
        Ok(out)
    }

    /// The PowerShell run that points every shortcut of the app at `icon`. A shortcut counts as the
    /// app's when it starts the app's exe or anything else inside its install folder.
    fn script(exe: &Path, folder: &Path, icon: &Path, roots: &[PathBuf]) -> String {
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
        let roots: Vec<String> = roots.iter().map(|p| quote(p)).collect();
        format!(
            "$ErrorActionPreference = 'SilentlyContinue'\n\
             $shell = New-Object -ComObject WScript.Shell\n\
             $exe = {exe}\n\
             $dir = {dir}\n\
             $icon = {icon}\n\
             foreach ($root in @({roots})) {{\n\
             \x20 if (-not (Test-Path -LiteralPath $root)) {{ continue }}\n\
             \x20 foreach ($file in Get-ChildItem -LiteralPath $root -Filter *.lnk -Recurse) {{\n\
             \x20   $shortcut = $shell.CreateShortcut($file.FullName)\n\
             \x20   $target = $shortcut.TargetPath\n\
             \x20   if ($target -and ($target -ieq $exe -or $target.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase))) {{\n\
             \x20     $shortcut.IconLocation = $icon + ',0'\n\
             \x20     $shortcut.Save()\n\
             \x20   }}\n\
             \x20 }}\n\
             }}\n",
            exe = quote(exe),
            dir = quote(folder),
            icon = quote(icon),
            roots = roots.join(", "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pack the catalog offers for an app is one this crate can actually draw and install.
    #[test]
    fn every_style_the_catalog_offers_has_artwork_behind_it() {
        for app in &artcraft_launcher_core::SUITE {
            for style in app.icon_styles() {
                let art = artwork(app, style).unwrap_or_else(|| panic!("{} has no {style:?} artwork", app.id));
                assert!(art.svg.contains("<svg"), "{} {style:?}: the SVG isn't one", app.id);
                assert_eq!(image::guess_format(art.png).ok(), Some(image::ImageFormat::Png), "{} {style:?}", app.id);
            }
            // An app with no artwork of its own is drawn with its own icon, never a pack's.
            if !app.has_alternate_icons() {
                for style in IconStyle::all().filter(|s| s.pack().is_some()) {
                    assert!(artwork(app, style).is_none(), "{} {style:?}", app.id);
                }
            }
        }
    }

    /// A pack a build doesn't have (a saved setting from elsewhere) falls back to the app's own
    /// icon rather than failing.
    #[test]
    fn an_unknown_pack_is_the_apps_own_icon() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let unknown = IconStyle::from("no-such-pack".to_string());
        assert_eq!(unknown, IconStyle::Default);
        assert!(artwork(app, unknown).is_none());
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn scaling_gives_a_square_png_of_the_size_asked_for() {
        let art = artwork(CraftApp::by_id("photocraft").unwrap(), IconStyle::all().nth(1).unwrap()).unwrap();
        let source = decoded(art.png).unwrap();
        for size in [16, 256, 512] {
            let image = image::load_from_memory(&png_of(&source, size).unwrap()).unwrap();
            assert_eq!((image.width(), image.height()), (size, size));
        }
    }

    /// The icon theme's folders: the alternate style fills them, and going back to the app's own
    /// icon leaves the 128 px one the launcher's AppImage integration writes and takes the rest.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn the_icon_theme_gets_the_chosen_style_and_loses_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("share");
        let app = CraftApp::by_id("photocraft").unwrap();
        let icon = |size: u32| data.join(format!("icons/hicolor/{size}x{size}/apps/{}.png", app.bundle_id()));
        let scalable = data.join(format!("icons/hicolor/scalable/apps/{}.svg", app.bundle_id()));

        theme::apply_into(&data, app, IconStyle::all().nth(1).unwrap()).unwrap();
        for size in [16, 24, 32, 48, 64, 128, 256, 512] {
            assert!(icon(size).is_file(), "{}", icon(size).display());
        }
        assert!(scalable.is_file());
        assert_eq!(image::load_from_memory(&std::fs::read(icon(16)).unwrap()).unwrap().width(), 16);

        theme::apply_into(&data, app, IconStyle::Default).unwrap();
        assert!(icon(128).is_file(), "the app's own icon stays where the AppImage integration put it");
        for size in [16, 24, 32, 48, 64, 256, 512] {
            assert!(!icon(size).exists(), "{}", icon(size).display());
        }
        assert!(!scalable.exists());
    }
}
