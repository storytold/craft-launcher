//! Read-only checks on the private copy, before the installer swaps it into place.
//!
//! These establish signature integrity, pinned identity, notarization and macOS policy
//! acceptance at check time. They do not promise a fresh OCSP response for every certificate.

use std::path::Path;
use std::time::Duration;

use artcraft_launcher_core::CraftApp;

const CODESIGN: &str = "/usr/bin/codesign";
const SPCTL: &str = "/usr/sbin/spctl";
const TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn verify_with(
    app: &CraftApp,
    bundle: &Path,
    team: &str,
    mut run: impl FnMut(&str, &[&str], Duration) -> Result<String, String>,
) -> Result<(), String> {
    let path = bundle.to_str().ok_or("couldn't check the app's unusual path, so it wasn't installed")?;
    let requirement = identity_requirement(app, team)?;
    for (check, program, args) in [
        // Do not apply the outer app's team or identifier requirement to its nested code.
        ("signature integrity", CODESIGN, vec!["--verify", "--all-architectures", "--deep", "--strict", path]),
        ("developer identity", CODESIGN, vec!["--verify", "--all-architectures", "--strict", "-R", &requirement, path]),
        ("notarization", CODESIGN, vec!["--verify", "--check-notarization", path]),
        // Ignore existing assessment objects AND avoid storing this assessment. This does not
        // bypass trust/revocation caches or prove a fresh OCSP response.
        ("macOS policy", SPCTL, vec!["--assess", "--type", "execute", "--ignore-cache", "--no-cache", "--raw", path]),
    ] {
        let output = run(program, &args, TIMEOUT)
            .map_err(|reason| format!("macOS verification did not succeed ({check}: {reason}), so the app wasn't installed"))?;
        if program == SPCTL && !assessment_accepted(&output) {
            return Err("macOS didn't return an affirmative policy assessment, so the app wasn't installed".into());
        }
    }
    Ok(())
}

fn identity_requirement(app: &CraftApp, team: &str) -> Result<String, String> {
    if team.is_empty() || !team.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("no valid macOS signing team is configured, so the app wasn't installed".into());
    }
    let mut identifiers = Vec::new();
    // Catalog data only, never an identifier supplied by the downloaded app. Keep approved
    // former IDs so detecting/updating an older bundle still agrees with the catalog.
    for id in app.bundle_ids() {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')) {
            return Err("no valid macOS bundle identity is configured, so the app wasn't installed".into());
        }
        identifiers.push(format!("identifier \"{id}\""));
    }
    // Developer ID Application's standard requirement, also observed in the real app's DR.
    Ok(format!(
        "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU]=\"{team}\" and ({})",
        identifiers.join(" or ")
    ))
}

fn assessment_accepted(output: &str) -> bool {
    let Ok(value) = plist::Value::from_reader_xml(output.as_bytes()) else { return false };
    let Some(dict) = value.as_dictionary() else { return false };
    if dict.get("assessment:verdict").and_then(plist::Value::as_boolean) != Some(true) {
        return false;
    }
    let Some(authority) = dict.get("assessment:authority").and_then(plist::Value::as_dictionary) else { return false };
    // A disabled-policy override is not a successful assessment. A malformed override is
    // unknown too; only its absence or an explicit false is acceptable.
    let no_override = |value: Option<&plist::Value>| value.is_none_or(|v| v.as_boolean() == Some(false));
    no_override(dict.get("assessment:override"))
        && no_override(authority.get("assessment:authority:override"))
        && authority.get("assessment:authority:source").and_then(plist::Value::as_string).is_some_and(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solidcraft() -> &'static CraftApp {
        CraftApp::by_id("solidcraft").unwrap()
    }

    // Verbatim stdout captured by the real macOS --raw assessment (receipt 653b8b).
    fn accepted() -> String {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>assessment:authority</key>
	<dict>
		<key>assessment:authority:flags</key>
		<integer>2</integer>
		<key>assessment:authority:row</key>
		<integer>11</integer>
		<key>assessment:authority:source</key>
		<string>Notarized Developer ID</string>
	</dict>
	<key>assessment:remote</key>
	<true/>
	<key>assessment:verdict</key>
	<true/>
</dict>
</plist>"#
            .into()
    }

    #[test]
    fn mac_install_requires_a_team_in_production_but_allows_unsigned_test_fixtures() {
        use crate::install::macos::required_team;

        let error = required_team(None, false).unwrap_err();
        assert_eq!(error, "no macOS signing team was supplied, so the app wasn't installed");
        assert_eq!(required_team(Some("G5QA5U5X2S"), false).unwrap(), Some("G5QA5U5X2S"));
        assert_eq!(required_team(None, true).unwrap(), None);
        assert_eq!(required_team(Some("G5QA5U5X2S"), true).unwrap(), Some("G5QA5U5X2S"));
    }

    #[test]
    fn mac_install_checks_are_bounded_and_only_pin_the_top_level_identity() {
        let app = solidcraft();
        let mut calls = Vec::new();
        verify_with(app, Path::new("/private/owned/SolidCraft.app"), app.mac_team.unwrap(), |program, args, timeout| {
            assert_eq!(timeout, Duration::from_secs(60));
            calls.push((program.to_string(), args.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            Ok(if program == SPCTL { accepted() } else { String::new() })
        })
        .unwrap();
        assert_eq!(calls.len(), 4);
        let integrity = &calls[0].1;
        assert!(integrity.iter().any(|a| a == "--all-architectures"));
        assert!(integrity.iter().any(|a| a == "--deep"));
        assert!(integrity.iter().any(|a| a == "--strict"));
        assert!(!integrity.iter().any(|a| a == "-R"));
        let identity = &calls[1].1;
        assert!(!identity.iter().any(|a| a == "--deep"));
        assert!(identity.iter().any(|a| a == "--all-architectures"));
        assert!(identity.iter().any(|a| a.contains("G5QA5U5X2S") && a.contains("io.tangencylabs.solidcraft")));
        assert_eq!(calls[2].1, ["--verify", "--check-notarization", "/private/owned/SolidCraft.app"]);
        for flag in ["--ignore-cache", "--no-cache", "--raw"] {
            assert!(calls[3].1.iter().any(|a| a == flag));
        }
    }

    #[test]
    fn mac_install_identity_comes_only_from_the_catalog_and_keeps_official_teams() {
        let sc = identity_requirement(solidcraft(), "G5QA5U5X2S").unwrap();
        assert!(sc.contains("identifier \"io.tangencylabs.solidcraft\""));
        assert!(sc.contains("identifier \"ai.storyteller.solidcraft\""));
        let photo = CraftApp::by_id("photocraft").unwrap();
        let official = identity_requirement(photo, photo.mac_team.unwrap()).unwrap();
        assert!(official.contains("DJ6XS33FX8"));
        assert!(official.contains("identifier \"ai.storyteller.photocraft\""));
        assert!(!official.contains("G5QA5U5X2S"));
        for team in ["", "X\" or anchor apple generic or \"Y"] {
            assert!(identity_requirement(solidcraft(), team).is_err());
        }
        let bad = CraftApp { bundle: Some("io.bad\" or true"), ..*solidcraft() };
        assert!(identity_requirement(&bad, "G5QA5U5X2S").is_err());
    }

    #[test]
    fn mac_install_rejects_missing_malformed_negative_and_overridden_assessments() {
        assert!(assessment_accepted(&accepted()));
        for output in [
            String::new(),
            "accepted".into(),
            "<plist><dict>".into(),
            accepted().replace("<true/>", "<false/>"),
            accepted().replace("<true/>", "<string>true</string>"),
            accepted().replace("<key>assessment:authority</key>", "<key>other</key>"),
            accepted().replace(
                "<key>assessment:authority:source</key>",
                "<key>assessment:authority:override</key><true/><key>assessment:authority:source</key>",
            ),
            accepted().replace(
                "<key>assessment:authority:source</key>",
                "<key>assessment:authority:override</key><string>false</string><key>assessment:authority:source</key>",
            ),
        ] {
            assert!(!assessment_accepted(&output), "accepted {output:?}");
        }
    }

    #[test]
    fn mac_install_tool_errors_stop_the_pipeline_without_blame_or_success() {
        for failed in 0..4 {
            for reason in ["couldn't start helper", "didn't finish within 60 seconds", "invalid strict option", "revoked", "requirement failed"] {
                let mut count = 0;
                let error = verify_with(solidcraft(), Path::new("/private/owned/SolidCraft.app"), "G5QA5U5X2S", |_, _, _| {
                    let call = count;
                    count += 1;
                    if call == failed { Err(reason.into()) } else { Ok(String::new()) }
                })
                .unwrap_err();
                assert_eq!(count, failed + 1);
                assert!(error.contains(reason));
                assert!(!error.contains("isn't signed by its developer"));
            }
        }
    }

    #[test]
    fn mac_install_failed_or_unknown_checks_preserve_old_bytes_records_and_cached_download() {
        use crate::install::macos::{Expect, check_and_swap};
        use artcraft_launcher_core::{InstallSource, Installed};
        let app = solidcraft();
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("new.app");
        let old = dir.path().join("old.app");
        let manifest = dir.path().join("manifest.json");
        let download = dir.path().join("download.dmg");
        std::fs::write(&staged, b"new fixture bytes").unwrap();
        std::fs::write(&old, b"old fixture bytes").unwrap();
        std::fs::write(&manifest, b"old install record").unwrap();
        std::fs::write(&download, b"cached download").unwrap();
        let found = Installed { version: None, source: InstallSource::MacApp, location: staged.display().to_string(), package: None };
        let expect = Expect { team: app.mac_team, version: None };
        // Injected tool outcomes exercise the real pre-swap boundary, not real signatures.
        for failed in 0..4 {
            let mut count = 0;
            let result = check_and_swap(app, &staged, &old, &found, &expect, |_, _, _| {
                let call = count;
                count += 1;
                if call == failed { Err("check unavailable or denied".into()) } else { Ok(String::new()) }
            });
            assert!(result.is_err());
            assert_eq!(count, failed + 1);
            assert_eq!(std::fs::read(&old).unwrap(), b"old fixture bytes");
            assert_eq!(std::fs::read(&staged).unwrap(), b"new fixture bytes");
            assert_eq!(std::fs::read(&manifest).unwrap(), b"old install record");
            assert_eq!(std::fs::read(&download).unwrap(), b"cached download");
        }
        // Exit zero with missing/malformed/negative raw policy data is still a refusal.
        for output in [String::new(), "not a plist".into(), accepted().replace("<true/>", "<false/>")] {
            let result = check_and_swap(app, &staged, &old, &found, &expect, |program, _, _| {
                Ok(if program == SPCTL { output.clone() } else { String::new() })
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(&old).unwrap(), b"old fixture bytes");
            assert_eq!(std::fs::read(&staged).unwrap(), b"new fixture bytes");
        }
    }

    #[test]
    #[ignore = "needs the independently staged real release; no app execution or swap"]
    fn mac_install_real_release_passes_the_production_verifier() {
        let bundle = std::env::var_os("ARTCRAFT_LAUNCHER_TEST_STAGED_APP").expect("set the owned staged release path");
        verify_with(solidcraft(), Path::new(&bundle), "G5QA5U5X2S", crate::process::run).unwrap();
    }
}
