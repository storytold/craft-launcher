//! Read-only checks on the private copy, before the installer swaps it into place.
//!
//! These establish signature integrity, pinned identity and notarization at check time. When
//! macOS assessments are enabled, they also require policy acceptance. A global assessment disable
//! is a user choice; it skips only that final policy check. These checks do not promise a fresh
//! OCSP response for every certificate.

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
    mut run: impl FnMut(&str, &[&str], Duration) -> Result<crate::process::RunOutput, String>,
) -> Result<(), String> {
    let path = bundle.to_str().ok_or("couldn't check the app's unusual path, so it wasn't installed")?;
    let requirement = identity_requirement(app, team)?;
    for (check, program, args) in [
        // Do not apply the outer app's team or identifier requirement to its nested code.
        ("signature integrity", CODESIGN, vec!["--verify", "--all-architectures", "--deep", "--strict", path]),
        ("developer identity", CODESIGN, vec!["--verify", "--all-architectures", "--strict", "-R", &requirement, path]),
        // `--check-notarization` looks for a ticket; the explicit requirement makes a positive
        // notarized result part of verification too (Apple DTS: codesign -R="notarized").
        ("notarization", CODESIGN, vec!["--verify", "--check-notarization", "-R=notarized", path]),
    ] {
        let output = run(program, &args, TIMEOUT)
            .map_err(|reason| format!("macOS verification did not succeed ({check}: {reason}), so the app wasn't installed"))?;
        if !output.success {
            let reason = output.stderr.trim();
            let reason = if reason.is_empty() { output.status } else { reason.to_string() };
            return Err(format!("macOS verification did not succeed ({check}: {reason}), so the app wasn't installed"));
        }
    }

    // This documented, read-only query distinguishes a user's global disable from an assessment
    // override on a particular app. Never infer that preference from the raw assessment result.
    let status = run(SPCTL, &["--status"], TIMEOUT)
        .map_err(|reason| format!("couldn't determine Gatekeeper status ({reason}), so the app wasn't installed"))?;
    if assessment_status(&status)? == AssessmentStatus::Disabled {
        return Ok(());
    }

    // Ignore existing assessment objects AND avoid storing this assessment. This does not
    // bypass trust/revocation caches or prove a fresh OCSP response.
    let output = run(SPCTL, &["--assess", "--type", "execute", "--ignore-cache", "--no-cache", "--raw", path], TIMEOUT)
        .map_err(|reason| format!("macOS verification did not succeed (macOS policy: {reason}), so the app wasn't installed"))?;
    if !output.success {
        let reason = output.stderr.trim();
        let reason = if reason.is_empty() { output.status } else { reason.to_string() };
        return Err(format!("macOS verification did not succeed (macOS policy: {reason}), so the app wasn't installed"));
    }
    if !assessment_accepted(&output.stdout) {
        return Err("macOS didn't return an affirmative policy assessment, so the app wasn't installed".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssessmentStatus {
    Enabled,
    Disabled,
}

fn assessment_status(output: &crate::process::RunOutput) -> Result<AssessmentStatus, String> {
    let stdout = output.stdout.trim();
    let stderr = output.stderr.trim();
    let disabled = (stdout == "assessments disabled" && stderr.is_empty()) || (stderr == "assessments disabled" && stdout.is_empty());
    if disabled && (output.success || output.code == Some(1)) {
        // `spctl --status` uses exit 1 for its explicit disabled-state output. Preserve and
        // interpret that result; `process::run` intentionally treats nonzero exits as errors.
        return Ok(AssessmentStatus::Disabled);
    }
    if !output.success {
        let reason = if !stderr.is_empty() { stderr.to_string() } else { output.status.clone() };
        return Err(format!("couldn't determine Gatekeeper status ({reason}), so the app wasn't installed"));
    }
    if stdout == "assessments enabled" && stderr.is_empty() {
        return Ok(AssessmentStatus::Enabled);
    }
    Err("macOS returned an unknown Gatekeeper status, so the app wasn't installed".into())
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

    fn success(output: impl Into<String>) -> crate::process::RunOutput {
        crate::process::RunOutput::success(output)
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
            Ok(success(if program == SPCTL && args == ["--status"] {
                "assessments enabled".into()
            } else if program == SPCTL {
                accepted()
            } else {
                String::new()
            }))
        })
        .unwrap();
        assert_eq!(calls.len(), 5);
        let integrity = &calls[0].1;
        assert!(integrity.iter().any(|a| a == "--all-architectures"));
        assert!(integrity.iter().any(|a| a == "--deep"));
        assert!(integrity.iter().any(|a| a == "--strict"));
        assert!(!integrity.iter().any(|a| a == "-R"));
        let identity = &calls[1].1;
        assert!(!identity.iter().any(|a| a == "--deep"));
        assert!(identity.iter().any(|a| a == "--all-architectures"));
        assert!(identity.iter().any(|a| a.contains("G5QA5U5X2S") && a.contains("io.tangencylabs.solidcraft")));
        assert_eq!(calls[2].1, ["--verify", "--check-notarization", "-R=notarized", "/private/owned/SolidCraft.app"]);
        assert_eq!(calls[3].1, ["--status"]);
        for flag in ["--ignore-cache", "--no-cache", "--raw"] {
            assert!(calls[4].1.iter().any(|a| a == flag));
        }
    }

    #[test]
    fn mac_install_respects_disabled_assessments_after_all_independent_checks() {
        let app = solidcraft();
        let mut calls = Vec::new();
        verify_with(app, Path::new("/private/owned/SolidCraft.app"), app.mac_team.unwrap(), |program, args, timeout| {
            assert_eq!(timeout, TIMEOUT);
            calls.push((program.to_string(), args.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            if program == SPCTL && args == ["--status"] {
                crate::process::run_with_output("/bin/sh", &["-c", "printf 'assessments disabled\\n'; exit 1"], timeout)
            } else {
                Ok(success(String::new()))
            }
        })
        .unwrap();
        assert_eq!(calls.len(), 4);
        assert!(calls[..3].iter().all(|(program, _)| program == CODESIGN));
        assert_eq!(calls[2].1, ["--verify", "--check-notarization", "-R=notarized", "/private/owned/SolidCraft.app"]);
        assert_eq!(calls[3], (SPCTL.into(), vec!["--status".into()]));
    }

    #[test]
    fn mac_install_with_disabled_assessments_still_rejects_tampering_wrong_identity_and_missing_notarization() {
        let app = solidcraft();
        for (failed_call, reason) in [
            (0, "signature integrity failed"),
            (1, "developer identity requirement failed"),
            (2, "notarized requirement failed"),
            (2, "notarization service unavailable"),
        ] {
            let mut calls = 0;
            let error = verify_with(app, Path::new("/private/owned/SolidCraft.app"), "G5QA5U5X2S", |program, args, _| {
                let call = calls;
                calls += 1;
                if call == failed_call {
                    Err(reason.into())
                } else if program == SPCTL && args == ["--status"] {
                    Ok(crate::process::RunOutput::exit(1, "assessments disabled", ""))
                } else {
                    Ok(success(String::new()))
                }
            })
            .unwrap_err();
            assert!(error.contains(reason), "{error}");
            assert!(error.contains("wasn't installed"), "{error}");
            assert_eq!(calls, failed_call + 1);
        }
    }

    #[test]
    fn mac_install_fails_closed_on_unknown_or_unavailable_assessment_status() {
        let app = solidcraft();
        for output in ["", "disabled", "assessments disabled plus other output", "<plist/>"] {
            let mut calls = 0;
            let error = verify_with(app, Path::new("/private/owned/SolidCraft.app"), "G5QA5U5X2S", |program, args, _| {
                calls += 1;
                Ok(success(if program == SPCTL && args == ["--status"] { output } else { "" }))
            })
            .unwrap_err();
            assert!(error.contains("unknown Gatekeeper status"), "{error}");
            assert_eq!(calls, 4);
        }

        let mut calls = 0;
        let error = verify_with(app, Path::new("/private/owned/SolidCraft.app"), "G5QA5U5X2S", |program, args, _| {
            calls += 1;
            if program == SPCTL && args == ["--status"] { Err("helper unavailable".into()) } else { Ok(success(String::new())) }
        })
        .unwrap_err();
        assert!(error.contains("couldn't determine Gatekeeper status"), "{error}");
        assert!(error.contains("helper unavailable"), "{error}");
        assert_eq!(calls, 4);
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
            accepted().replace("<key>assessment:authority</key>", "<key>assessment:override</key><true/><key>assessment:authority</key>"),
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
        for failed in 0..5 {
            for reason in [
                "couldn't start helper",
                "didn't finish within 60 seconds",
                "invalid strict option",
                "revoked",
                "requirement failed",
                "network unavailable",
            ] {
                let mut count = 0;
                let error = verify_with(solidcraft(), Path::new("/private/owned/SolidCraft.app"), "G5QA5U5X2S", |program, args, _| {
                    let call = count;
                    count += 1;
                    if call == failed {
                        Err(reason.into())
                    } else if program == SPCTL && args == ["--status"] {
                        Ok(success("assessments enabled"))
                    } else if program == SPCTL {
                        Ok(success(accepted()))
                    } else {
                        Ok(success(String::new()))
                    }
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
        for failed in 0..5 {
            let mut count = 0;
            let result = check_and_swap(app, &staged, &old, &found, &expect, |program, args, _| {
                let call = count;
                count += 1;
                if call == failed {
                    Err("check unavailable or denied".into())
                } else if program == SPCTL && args == ["--status"] {
                    Ok(success("assessments enabled"))
                } else if program == SPCTL {
                    Ok(success(accepted()))
                } else {
                    Ok(success(String::new()))
                }
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
            let result = check_and_swap(app, &staged, &old, &found, &expect, |program, args, _| {
                Ok(if program == SPCTL && args == ["--status"] {
                    success("assessments enabled")
                } else if program == SPCTL {
                    success(output.clone())
                } else {
                    success(String::new())
                })
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(&old).unwrap(), b"old fixture bytes");
            assert_eq!(std::fs::read(&staged).unwrap(), b"new fixture bytes");
        }
    }

    #[test]
    fn disabled_assessments_allow_only_the_verified_staged_copy_to_swap() {
        use crate::install::macos::{Expect, check_and_swap};
        use artcraft_launcher_core::{InstallSource, Installed};

        let app = solidcraft();
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("new.app");
        let old = dir.path().join("old.app");
        std::fs::write(&staged, b"new fixture bytes").unwrap();
        std::fs::write(&old, b"old fixture bytes").unwrap();
        let found = Installed { version: None, source: InstallSource::MacApp, location: staged.display().to_string(), package: None };
        let expect = Expect { team: app.mac_team, version: None };
        let mut calls = Vec::new();

        check_and_swap(app, &staged, &old, &found, &expect, |program, args, _| {
            calls.push((program.to_string(), args.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            Ok(if program == SPCTL && args == ["--status"] {
                crate::process::RunOutput::exit(1, "assessments disabled", "")
            } else {
                success(String::new())
            })
        })
        .unwrap();

        assert_eq!(calls.len(), 4);
        assert_eq!(calls[3], (SPCTL.into(), vec!["--status".into()]));
        assert_eq!(std::fs::read(&old).unwrap(), b"new fixture bytes");
        assert!(!staged.exists());
    }

    #[test]
    #[ignore = "needs the independently staged real release; no app execution or swap"]
    fn mac_install_real_release_passes_the_production_verifier() {
        let bundle = std::env::var_os("ARTCRAFT_LAUNCHER_TEST_STAGED_APP").expect("set the owned staged release path");
        verify_with(solidcraft(), Path::new(&bundle), "G5QA5U5X2S", crate::process::run_with_output).unwrap();
    }

    #[test]
    #[ignore = "needs a staged signed/notarized release; simulates disabled status without changing host settings"]
    fn mac_install_real_release_keeps_its_own_checks_when_assessments_are_disabled() {
        use crate::install::macos::{Expect, check_and_swap};
        use artcraft_launcher_core::{InstallSource, Installed, version::Version};

        let source = std::path::PathBuf::from(std::env::var_os("ARTCRAFT_LAUNCHER_TEST_STAGED_APP").expect("set the owned staged release path"));
        let dir = tempfile::tempdir().unwrap();
        let staged_dir = dir.path().join("staged");
        let destination_dir = dir.path().join("Applications");
        std::fs::create_dir_all(&staged_dir).unwrap();
        std::fs::create_dir_all(&destination_dir).unwrap();
        let staged = staged_dir.join("SolidCraft.app");
        crate::process::run(
            "/usr/bin/ditto",
            &["--norsrc", "--noextattr", "--noacl", source.to_str().unwrap(), staged.to_str().unwrap()],
            Duration::from_secs(600),
        )
        .unwrap();

        let app = solidcraft();
        let destination = destination_dir.join("SolidCraft.app");
        let version = Version::new(0, 28, 0);
        let found =
            Installed { version: Some(version.clone()), source: InstallSource::MacApp, location: staged.display().to_string(), package: None };
        let expect = Expect { team: app.mac_team, version: Some(&version) };
        check_and_swap(app, &staged, &destination, &found, &expect, |program, args, timeout| {
            if program == SPCTL && args == ["--status"] {
                Ok(crate::process::RunOutput::exit(1, "assessments disabled", ""))
            } else if program == SPCTL {
                Err("policy assessment should be skipped when disabled".into())
            } else {
                crate::process::run_with_output(program, args, timeout)
            }
        })
        .unwrap();

        assert!(destination.is_dir());
        assert!(!staged.exists());
    }
}
