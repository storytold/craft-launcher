//! Craft Apps built in the user's own source checkouts (the `local-checkouts` feature).
//!
//! A developer points the launcher at the folder that holds their checkouts
//! (`~/projects/storytold`), and each app found there as `<folder>/<app id>` opens its own build,
//! `target/release/<app id>`, instead of an installed copy. Nothing about that build was
//! downloaded, so no checksum or signature applies to it, and the launcher never updates,
//! replaces or removes it ([`InstallSource::Checkout`]).

use std::path::{Path, PathBuf};

use artcraft_launcher_core::{CraftApp, InstallSource, Installed};

/// Whether this build reads a checkouts folder at all. Release builds never enable the feature, so
/// an old setting can't make a shipped launcher run unverified programs.
pub const ENABLED: bool = cfg!(feature = "local-checkouts");

/// The local build of `app` in `root`, when `root` holds a checkout of it. A checkout that hasn't
/// been built yet is reported too, at the path its build will have, so opening it can say what's
/// missing instead of opening an installed copy the developer didn't ask for.
pub fn detect(app: &CraftApp, root: &Path) -> Option<Installed> {
    let dir = find_checkout(app, root)?;
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    Some(Installed {
        version: artcraft_launcher_core::checkout::version(&manifest),
        source: InstallSource::Checkout,
        location: built_program(app, &dir).display().to_string(),
        package: None,
        externally_managed: true,
        origin: Some(dir.display().to_string()),
        installation: None,
    })
}

/// `root`'s folder for `app`: named like its id, in any case (`AniCraft` for `anicraft`), and a
/// Cargo project. The folder is taken from the listing, so its path is spelled as on disk even
/// where the file system ignores case; a folder named exactly like the id goes first.
fn find_checkout(app: &CraftApp, root: &Path) -> Option<PathBuf> {
    let candidates: Vec<(String, PathBuf)> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .filter(|(name, dir)| name.eq_ignore_ascii_case(app.id) && dir.join("Cargo.toml").is_file())
        .collect();
    let exact = candidates.iter().position(|(name, _)| name == app.id);
    let (_, dir) = candidates.into_iter().nth(exact.unwrap_or(0))?;
    Some(dir)
}

/// Where `cargo build --release` puts the app's program in checkout `dir`.
fn built_program(app: &CraftApp, dir: &Path) -> PathBuf {
    dir.join("target").join("release").join(format!("{}{}", app.id, std::env::consts::EXE_SUFFIX))
}

/// Why `installed`, a local build, can't be opened, or `None` when its program is there.
pub fn not_built(app: &CraftApp, installed: &Installed) -> Option<String> {
    if Path::new(&installed.location).is_file() {
        return None;
    }
    let dir = installed.origin.as_deref().unwrap_or(&installed.location);
    Some(format!("{}'s local build isn't there yet. Run cargo build --release -p {} in {dir}.", app.name, app.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> &'static CraftApp {
        CraftApp::by_id("anicraft").unwrap()
    }

    fn checkout(root: &Path, folder: &str, version: &str) -> PathBuf {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), format!("[workspace.package]\nversion = \"{version}\"\n")).unwrap();
        dir
    }

    #[test]
    fn a_checkout_named_in_another_case_is_found_with_its_version() {
        let root = tempfile::tempdir().unwrap();
        let dir = checkout(root.path(), "AniCraft", "0.3.0");
        let found = detect(app(), root.path()).unwrap();
        assert_eq!(found.source, InstallSource::Checkout);
        assert_eq!(found.version, artcraft_launcher_core::version::parse("0.3.0"));
        assert!(Path::new(&found.location).starts_with(&dir), "{} is inside {}", found.location, dir.display());
    }

    #[test]
    fn a_folder_that_is_not_a_cargo_project_is_no_checkout() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("anicraft")).unwrap();
        assert!(detect(app(), root.path()).is_none());
        assert!(detect(app(), &root.path().join("missing")).is_none());
    }

    #[test]
    fn an_unbuilt_checkout_says_how_to_build_it_until_its_program_exists() {
        let root = tempfile::tempdir().unwrap();
        checkout(root.path(), "anicraft", "0.3.0");
        let found = detect(app(), root.path()).unwrap();
        assert!(not_built(app(), &found).is_some_and(|why| why.contains("cargo build --release -p anicraft")));
        let program = Path::new(&found.location);
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(program, b"").unwrap();
        assert_eq!(not_built(app(), &found), None);
    }
}
