// Authored synthetic archives and disposable generated test keys only. Not release signing,
// not a reviewed production key, and never reachable through a runtime trust override.
use artcraft_launcher_core::nightly::InstallChannel;
use sha2::{Digest, Sha256};

struct DisposableKey {
    home: tempfile::TempDir,
    fingerprint: String,
}
impl Drop for DisposableKey {
    fn drop(&mut self) {
        let mut command = crate::process::command("/usr/bin/gpgconf");
        command.args(["--homedir"]).arg(self.home.path()).args(["--kill", "gpg-agent"]);
        let _ = crate::process::run_command(command, "disposable test agent cleanup", Duration::from_secs(10));
    }
}
fn key_command(home: &Path, args: &[&str]) -> String {
    let mut command = crate::process::command(GPG);
    command
        .env_clear()
        .env("HOME", home)
        .env("GNUPGHOME", home)
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin")
        .args(["--no-options", "--homedir"])
        .arg(home)
        .args(["--batch", "--yes", "--pinentry-mode", "loopback", "--passphrase", ""])
        .args(args);
    crate::process::run_command(command, "disposable test gpg", Duration::from_secs(30)).unwrap()
}
fn key(root: &Path, old: bool) -> DisposableKey {
    let home = tempfile::Builder::new().prefix("disposable-key-").tempdir_in(root).unwrap();
    let mut args = vec!["--quick-generate-key", "Disposable Nightly Test (NOT A RELEASE KEY)", "ed25519", "cert", "1d"];
    // A trailing `!` freezes gpg's faked clock. Without it the time advances while each gpg process
    // runs, so a slow primary-key step can land a second later than the next process's start and
    // the subkey step then refuses with "Time conflict".
    if old {
        args.splice(0..0, ["--faked-system-time", "1700000000!"]);
    }
    key_command(home.path(), &args);
    let listed = key_command(home.path(), &["--with-colons", "--list-keys"]);
    let fingerprint = listed.lines().find(|line| line.starts_with("fpr:")).unwrap().split(':').nth(9).unwrap().to_owned();
    let mut subkey = vec!["--quick-add-key", fingerprint.as_str(), "ed25519", "sign", "1d"];
    if old {
        subkey.splice(0..0, ["--faked-system-time", "1700000000!"]);
    }
    key_command(home.path(), &subkey);
    DisposableKey { home, fingerprint }
}
fn signed_archive(root: &Path, signer: &DisposableKey, old: bool, content: &[u8]) -> (Asset, Download, Vec<u8>, PathBuf) {
    let file = root.join(if old { "expired.tar.gz" } else { "synthetic.tar.gz" });
    let encoder = flate2::write::GzEncoder::new(std::fs::File::create(&file).unwrap(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for (name, bytes, mode) in [("photocraft", content, 0o755), ("photocraft-nightly.png", b"synthetic badge".as_slice(), 0o644)] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        archive.append_data(&mut header, name, bytes).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap();
    let detached = file.with_extension("gz.asc");
    let mut args = vec!["--armor", "--detach-sign", "--output", detached.to_str().unwrap(), file.to_str().unwrap()];
    if old {
        args.splice(0..0, ["--faked-system-time", "1700000001!"]);
    }
    key_command(signer.home.path(), &args);
    let bytes = std::fs::read(detached).unwrap();
    let asset = Asset {
        name: "synthetic.tar.gz".into(),
        url: "unused exact synthetic fixture".into(),
        size: std::fs::metadata(&file).unwrap().len(),
        sha256: Some(crate::http::sha256_file(&file).unwrap()),
    };
    let signature = Download {
        name: "synthetic.tar.gz.asc".into(),
        url: "unused exact synthetic fixture".into(),
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    };
    (asset, signature, bytes, file)
}
fn elf() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..20].copy_from_slice(&[2, 0, 62, 0]);
    bytes
}
fn export(key: &DisposableKey) -> String {
    key_command(key.home.path(), &["--armor", "--export", &key.fingerprint])
}

#[test]
fn signed_archive_test_key_passes_exact_bytes_and_bad_missing_wrong_key_tool_failures_preserve_copies() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path()).for_channel(InstallChannel::Nightly);
    let signer = key(root.path(), false);
    let public = export(&signer);
    let trusted = LinuxSigner { public_key: &public, primary_fingerprint: &signer.fingerprint };
    let (asset, signature, bytes, file) = signed_archive(root.path(), &signer, false, &elf());
    let verified = prepare_with(&file, &asset, &signature, &bytes, &paths, &trusted, GPG).unwrap();
    let adapter = crate::nightly_profile::Adapter::PhotoCraftLinuxV1;
    let version = "1.0.0-nightly.20261121.987654.1.gabcdefabcdef";
    let mut commits = 0;
    let initial = crate::nightly_unix::install(
        crate::nightly_unix::Package {
            adapter,
            file: &verified.file,
            version,
            previous: None,
            paths: &paths,
            archive_sha256: asset.sha256.clone().unwrap(),
            lease: None,
        },
        &mut |_| {
            commits += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(commits, 1);
    let profile = adapter.profile_root(&paths).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    let personal = profile.join("personal.txt");
    std::fs::write(&personal, b"preserved personal data").unwrap();
    std::fs::write(paths.manifest(), b"preserved old manifest").unwrap();
    let before = std::fs::read(&initial.path).unwrap();
    let modified = std::fs::metadata(&initial.path).unwrap().modified().unwrap();
    let wrong = key(root.path(), false);
    let wrong_public = export(&wrong);
    for case in 0..10 {
        let mut candidate = bytes.clone();
        let mut declared = signature.clone();
        let wrong_pin = "0000000000000000000000000000000000000000";
        let (key, pin) = match case {
            1 => (&wrong_public, signer.fingerprint.as_str()),
            2 => (&public, wrong_pin),
            _ => (&public, signer.fingerprint.as_str()),
        };
        match case {
            0 => candidate.clear(),
            3 => candidate = b"not an armored signature".to_vec(),
            4 => {
                candidate[80] ^= 1;
                declared.sha256 = format!("{:x}", Sha256::digest(&candidate));
            }
            5 => declared.size += 1,
            6 => declared.sha256 = "0".repeat(64),
            8 => {
                candidate.extend_from_slice(&bytes);
                declared.size = candidate.len() as u64;
                declared.sha256 = format!("{:x}", Sha256::digest(&candidate));
            }
            9 => {
                candidate = vec![b'x'; MAX_SIGNATURE_BYTES + 1];
                declared.size = candidate.len() as u64;
                declared.sha256 = format!("{:x}", Sha256::digest(&candidate));
            }
            _ => {}
        }
        let trust = LinuxSigner { public_key: key, primary_fingerprint: pin };
        assert!(
            prepare_with(&file, &asset, &declared, &candidate, &paths, &trust, if case == 7 { "/no/such/nightly-verifier" } else { GPG }).is_err(),
            "case {case}"
        );
        assert_eq!(std::fs::read(&initial.path).unwrap(), before);
        assert_eq!(std::fs::metadata(&initial.path).unwrap().modified().unwrap(), modified);
        assert_eq!(std::fs::read(&personal).unwrap(), b"preserved personal data");
        assert_eq!(std::fs::read(paths.manifest()).unwrap(), b"preserved old manifest");
        assert_eq!(commits, 1);
    }
    // A changed archive cannot borrow the original signature even with a fresh matching checksum.
    let mut corrupted = std::fs::read(&file).unwrap();
    corrupted.push(1);
    std::fs::write(&file, corrupted).unwrap();
    let mut changed = asset.clone();
    changed.size += 1;
    changed.sha256 = Some(crate::http::sha256_file(&file).unwrap());
    assert!(prepare_with(&file, &changed, &signature, &bytes, &paths, &trusted, GPG).is_err());
    // The authenticated private copy survives later mutation of the download, but any mutation
    // of that private copy is detected before extraction/replacement by the transaction digest.
    std::fs::write(&verified.file, b"changed after verification").unwrap();
    let refused = crate::nightly_unix::install(
        crate::nightly_unix::Package {
            adapter,
            file: &verified.file,
            version,
            previous: Some(&initial),
            paths: &paths,
            archive_sha256: asset.sha256.unwrap(),
            lease: None,
        },
        &mut |_| {
            commits += 1;
            Ok(())
        },
    )
    .unwrap_err();
    // The digest guard itself must refuse, not a later lease or manifest check.
    assert!(refused.contains("authenticated Nightly archive changed"), "{refused}");
    assert_eq!(std::fs::read(paths.manifest()).unwrap(), b"preserved old manifest");
    assert_eq!(commits, 1);
    assert_eq!(std::fs::read(&initial.path).unwrap(), before);
    assert_eq!(std::fs::read(&personal).unwrap(), b"preserved personal data");
}

#[test]
fn expired_and_revoked_disposable_keys_are_rejected_by_the_real_verifier() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path()).for_channel(InstallChannel::Nightly);
    let expired = key(root.path(), true);
    let public = export(&expired);
    let (asset, sig, bytes, file) = signed_archive(root.path(), &expired, true, &elf());
    assert!(
        prepare_with(&file, &asset, &sig, &bytes, &paths, &LinuxSigner { public_key: &public, primary_fingerprint: &expired.fingerprint }, GPG)
            .is_err()
    );
    let revoked = key(root.path(), false);
    let (asset, sig, bytes, file) = signed_archive(root.path(), &revoked, false, &elf());
    let revocation = std::fs::read_to_string(revoked.home.path().join("openpgp-revocs.d").join(format!("{}.rev", revoked.fingerprint))).unwrap();
    let certificate = revocation.split_once(":-----BEGIN PGP PUBLIC KEY BLOCK-----").unwrap().1;
    let cert = root.path().join("disposable-revocation.asc");
    std::fs::write(&cert, format!("-----BEGIN PGP PUBLIC KEY BLOCK-----{certificate}")).unwrap();
    key_command(revoked.home.path(), &["--import", cert.to_str().unwrap()]);
    let public = export(&revoked);
    assert!(
        prepare_with(&file, &asset, &sig, &bytes, &paths, &LinuxSigner { public_key: &public, primary_fingerprint: &revoked.fingerprint }, GPG)
            .is_err()
    );
}

#[test]
fn machine_status_never_accepts_error_multiple_signer_or_expiry_even_with_validsig() {
    let pin = "A".repeat(40);
    let valid =
        format!("[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG 0123456789ABCDEF test\n[GNUPG:] VALIDSIG {pin} 2026-11-21 1795280000 0 4 0 22 8 00 {pin}\n");
    assert!(status(&crate::process::RunOutput::success(&valid), &pin, crate::now()).is_ok());
    for extra in ["REVKEYSIG", "EXPKEYSIG", "EXPSIG", "KEYEXPIRED", "SIGEXPIRED", "ERRSIG", "NO_PUBKEY", "FAILURE", "NODATA", "BADARMOR", "UNKNOWN"] {
        let output = crate::process::RunOutput::success(format!("{valid}[GNUPG:] {extra}\n"));
        assert!(status(&output, &pin, crate::now()).is_err(), "{extra}");
    }
    assert!(status(&crate::process::RunOutput::success(format!("{valid}{valid}")), &pin, crate::now()).is_err());
    assert!(status(&crate::process::RunOutput::exit(1, &valid, "failure"), &pin, crate::now()).is_err());
    assert!(status(&crate::process::RunOutput::success(&valid), &"B".repeat(40), crate::now()).is_err());
}

// A real PhotoCraft v0.23.1 stable release file and its published detached signature, made by the
// production Linux release key's signing subkey. See ATTRIBUTION.md for provenance.
const RELEASE_SIGNED: &[u8] = include_bytes!("../tests/fixtures/linux-release-signed/SHA256SUMS.txt");
const RELEASE_SIGNATURE: &[u8] = include_bytes!("../tests/fixtures/linux-release-signed/SHA256SUMS.txt.asc");

fn configured_signer() -> &'static LinuxSigner<'static> {
    crate::nightly_trust::LINUX_SIGNER.as_ref().expect("the reviewed Linux Nightly signer is configured")
}

#[test]
fn configured_signer_key_is_its_pinned_primary_and_refuses_other_keys() {
    let signer = configured_signer();
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::Builder::new().prefix("pinned-key-").tempdir_in(root.path()).unwrap();
    let pinned = home.path().join("pinned.asc");
    std::fs::write(&pinned, signer.public_key).unwrap();
    let shown = key_command(home.path(), &["--with-colons", "--show-keys", pinned.to_str().unwrap()]);
    let records: Vec<Vec<&str>> = shown.lines().map(|line| line.split(':').collect()).collect();
    // Exactly one certificate, whose primary (not a subkey) is the pin, usable and not revoked or expired.
    let primaries: Vec<_> = records.iter().filter(|fields| fields[0] == "pub").collect();
    assert_eq!(primaries.len(), 1, "{shown}");
    assert!(!matches!(primaries[0][1], "r" | "e" | "i" | "d"), "{shown}");
    let primary = records.iter().skip_while(|fields| fields[0] != "pub").find(|fields| fields[0] == "fpr").unwrap()[9];
    assert!(primary.eq_ignore_ascii_case(signer.primary_fingerprint), "{shown}");
    assert!(records.iter().any(|fields| matches!(fields[0], "pub" | "sub") && fields[11].contains('s')), "no signing key: {shown}");
    let paths = Paths::under(root.path()).for_channel(InstallChannel::Nightly);
    let other = key(root.path(), false);
    let (asset, signature, bytes, file) = signed_archive(root.path(), &other, false, &elf());
    assert!(prepare_with(&file, &asset, &signature, &bytes, &paths, signer, GPG).is_err());
}

fn release_fixture(root: &Path, name: &str, content: &[u8]) -> (Asset, Download, PathBuf) {
    let file = root.join(name);
    std::fs::write(&file, content).unwrap();
    let asset = Asset {
        name: name.into(),
        url: "unused exact release fixture".into(),
        size: content.len() as u64,
        sha256: Some(crate::http::sha256_file(&file).unwrap()),
    };
    let signature = Download {
        name: format!("{name}.asc"),
        url: "unused exact release fixture".into(),
        size: RELEASE_SIGNATURE.len() as u64,
        sha256: format!("{:x}", Sha256::digest(RELEASE_SIGNATURE)),
    };
    (asset, signature, file)
}

#[test]
fn configured_signer_accepts_a_real_release_signature_only_for_its_exact_bytes_and_primary_pin() {
    let signer = configured_signer();
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::under(root.path()).for_channel(InstallChannel::Nightly);
    let (asset, signature, file) = release_fixture(root.path(), "release-signed", RELEASE_SIGNED);
    let verified = prepare_with(&file, &asset, &signature, RELEASE_SIGNATURE, &paths, signer, GPG).unwrap();
    assert_eq!(std::fs::read(&verified.file).unwrap(), RELEASE_SIGNED);
    // One changed byte, even with a fresh matching checksum, does not borrow the real signature.
    let mut changed = RELEASE_SIGNED.to_vec();
    changed[0] ^= 1;
    let (asset, signature, file) = release_fixture(root.path(), "release-changed", &changed);
    assert!(prepare_with(&file, &asset, &signature, RELEASE_SIGNATURE, &paths, signer, GPG).is_err());
    // The pin is the primary identity: pinning the signing subkey that made the signature refuses.
    let home = tempfile::Builder::new().prefix("pinned-key-").tempdir_in(root.path()).unwrap();
    let pinned = home.path().join("pinned.asc");
    std::fs::write(&pinned, signer.public_key).unwrap();
    let shown = key_command(home.path(), &["--with-colons", "--show-keys", pinned.to_str().unwrap()]);
    let subkey = shown.lines().skip_while(|line| !line.starts_with("sub:")).find(|line| line.starts_with("fpr:")).unwrap().split(':').nth(9).unwrap();
    assert!(!subkey.eq_ignore_ascii_case(signer.primary_fingerprint));
    let subkey_pin = LinuxSigner { public_key: signer.public_key, primary_fingerprint: subkey };
    let (asset, signature, file) = release_fixture(root.path(), "release-subkey-pin", RELEASE_SIGNED);
    assert!(prepare_with(&file, &asset, &signature, RELEASE_SIGNATURE, &paths, &subkey_pin, GPG).is_err());
}
