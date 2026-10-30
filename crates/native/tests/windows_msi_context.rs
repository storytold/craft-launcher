//! A missing or unreadable MSI identity must stop an update before downloading or installing.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use artcraft_launcher_core::protocol::InstallLocations;
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::{CraftApp, InstallSource, Installed, PackageKind, version};
use artcraft_launcher_native::http::{Http, Stop};
use artcraft_launcher_native::install::{self, Job};
use artcraft_launcher_native::paths::Paths;

#[test]
fn uncertain_msi_updates_do_not_download_or_start_an_installer() {
    let app = CraftApp::by_id("photocraft").unwrap();
    let release = Release { tag: "99.0.0".into(), version: version::parse("99.0.0"), ..Default::default() };
    // No HTTP request should reach this URL, and it cannot contact a live service if regressed.
    let asset =
        Asset { name: "photocraft-99.0.0-windows-x64.msi".into(), url: "http://127.0.0.1:9/must-not-download.msi".into(), ..Default::default() };
    let http = Http::new().unwrap();
    let stop = Stop::default();
    let locations = InstallLocations::default();
    for code in [None, Some("not-a-product-code"), Some("{00000000-0000-0000-0000-000000000000}")] {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        let existing = Installed {
            version: version::parse("1.0.0"),
            source: InstallSource::Msi,
            location: root.path().join("not-running-context-regression.exe").display().to_string(),
            package: code.map(str::to_owned),
        };
        let mut phases = Vec::new();
        let mut phase = |phase| phases.push(phase);
        let mut job =
            Job { http: &http, paths: &paths, stop: &stop, phase: &mut phase, install_locations: &locations, replace: false, downgrade: None };
        let result = install::install(app, &release, &asset, PackageKind::Msi, Some(&existing), None, &mut job);
        assert!(matches!(&result, Err(error) if error.contains("could not confirm who this app is installed for")), "{result:?}");
        assert!(phases.is_empty(), "an uncertain update must not enter download or installation phases");
        assert!(!paths.downloads().exists(), "no download folder should have been created");
    }
}
