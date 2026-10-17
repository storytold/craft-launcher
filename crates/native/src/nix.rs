//! Ownership of outputs in Nix's conventional immutable store.
//!
//! This does not change application discovery, PATH precedence, or package-manager metadata.
//! Custom store roots cannot be inferred from a path and are not covered here.

use std::path::Path;

pub(crate) fn is_managed(path: &Path) -> bool {
    resolved_in_store(path, Path::new("/nix/store"))
}

fn resolved_in_store(path: &Path, store: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    // Resolve profile and Applications-folder links before checking ownership. Keep direct
    // store paths protected even when the output has disappeared since the detection pass.
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    output_path(&resolved, store)
}

fn output_path(path: &Path, store: &Path) -> bool {
    path.strip_prefix(store).is_ok_and(|relative| {
        let mut components = relative.components();
        components.next().is_some_and(|first| matches!(first, std::path::Component::Normal(_)))
            && components.all(|component| matches!(component, std::path::Component::Normal(_)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_outputs_and_their_contents_are_managed() {
        for path in
            ["/nix/store/hash-photocraft", "/nix/store/hash-photocraft/bin/photocraft", "/nix/store/hash-photocraft/Applications/PhotoCraft.app"]
        {
            assert!(is_managed(Path::new(path)), "{path}");
        }
        for path in [
            "/nix/store",
            "/nix/storefront/hash/bin/app",
            "/tmp/nix/store/hash/bin/app",
            "/nix/store/missing/../../tmp/app",
            "ai.storyteller.photocraft",
            "nix/store/hash/bin/app",
        ] {
            assert!(!is_managed(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn profile_and_bundle_links_keep_external_ownership() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let store = root.join("store");
        let bundle = store.join("hash-photocraft/Applications/PhotoCraft.app");
        let executable = bundle.join("Contents/MacOS/PhotoCraft");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let profile = root.join("photocraft");
        let applications = root.join("PhotoCraft.app");
        symlink(&executable, &profile).unwrap();
        symlink(&bundle, &applications).unwrap();
        assert!(resolved_in_store(&profile, &store));
        assert!(resolved_in_store(&applications, &store));
        assert!(resolved_in_store(&executable, &store));
        assert!(!resolved_in_store(&root, &store));
        let manual = root.join("Manual.app");
        std::fs::create_dir(&manual).unwrap();
        let manual_link = root.join("Manual link.app");
        symlink(&manual, &manual_link).unwrap();
        assert!(!resolved_in_store(&manual_link, &store));
        let external_executable = root.join("external-app");
        std::fs::write(&external_executable, b"#!/bin/sh\nexit 0\n").unwrap();
        let external_link = root.join("external-link");
        symlink(&external_executable, &external_link).unwrap();
        assert!(!resolved_in_store(&external_link, &store));
        assert!(profile.is_file());
        assert!(applications.is_dir());
    }
}
