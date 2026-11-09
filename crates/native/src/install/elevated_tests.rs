use super::*;

fn package(file: &Path, bytes: &[u8]) -> Package {
    std::fs::write(file, bytes).unwrap();
    let sha256 = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    Package { file: file.to_path_buf(), sha256, version: "0.13.0".into() }
}

/// Sets a folder's modified time, which Windows only allows through a handle with write-attributes access.
fn age(dir: &Path, when: std::time::SystemTime) {
    #[cfg(windows)]
    let handle = {
        use std::os::windows::fs::OpenOptionsExt;
        /// `FILE_WRITE_ATTRIBUTES`.
        const WRITE_ATTRIBUTES: u32 = 0x100;
        /// `FILE_FLAG_BACKUP_SEMANTICS`, without which a folder can't be opened.
        const BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::File::options().access_mode(WRITE_ATTRIBUTES).custom_flags(BACKUP_SEMANTICS).open(dir).unwrap()
    };
    #[cfg(not(windows))]
    let handle = std::fs::File::open(dir).unwrap();
    handle.set_modified(when).unwrap();
}

#[test]
fn a_run_id_is_32_hex_digits_and_nothing_else_passes() {
    let run = new_run().unwrap();
    assert!(valid_run(&run), "{run}");
    assert_ne!(run, new_run().unwrap());
    for bad in ["", "..", &run[..31], &format!("{run}0"), &run.to_uppercase(), "0123456789abcdef0123456789abcde/", "0123456789abcdef0123456789abcdeg"]
    {
        assert!(!valid_run(bad), "{bad}");
    }
}

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn job(folder: Option<&str>) -> Job {
    let downloads = r"C:\Users\Jo O'Neil\AppData\Local\ArtCraft Launcher\downloads";
    Job {
        app: "filmcraft".into(),
        chosen: Package { file: PathBuf::from(format!(r"{downloads}\filmcraft-0.13.0.msi")), sha256: SHA_A.into(), version: "0.13.0".into() },
        previous: Package {
            file: PathBuf::from(format!(r"{downloads}\filmcraft-0.14.0-nightly.2.msi")),
            sha256: SHA_B.into(),
            version: "0.14.0-nightly.2".into(),
        },
        folder: folder.map(PathBuf::from),
    }
}

/// Splits a command line the way the helper receives it: quotes group, and are removed.
fn split(line: &str) -> Vec<OsString> {
    let mut out = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in line.chars() {
        match c {
            '"' => (quoted, any) = (!quoted, true),
            ' ' if !quoted => {
                if any || !current.is_empty() {
                    out.push(std::mem::take(&mut current).into());
                }
                any = false;
            }
            c => current.push(c),
        }
    }
    if any || !current.is_empty() {
        out.push(current.into());
    }
    out
}

#[test]
fn the_whole_job_travels_on_the_command_line_and_round_trips() {
    let run = new_run().unwrap();
    for folder in [Some(r"C:\Program Files\ArtCraft\FilmCraft"), None] {
        let job = job(folder);
        let line = parameters(&run, &job).unwrap();
        assert!(line.starts_with(&format!("{FLAG} {run} filmcraft 0.13.0 {SHA_A} \"")), "{line}");
        assert_eq!(parse_args(&split(&line)), Some(Ok((run.clone(), job))), "{line}");
    }
}

#[test]
fn the_command_line_refuses_what_could_break_out_of_its_quotes_or_isnt_a_checksum() {
    let run = new_run().unwrap();
    let good = job(None);
    let with = |change: &dyn Fn(&mut Job)| {
        let mut job = good.clone();
        change(&mut job);
        parameters(&run, &job)
    };
    assert!(with(&|j| j.chosen.file = PathBuf::from("C:\\a\"b.msi")).is_err());
    assert!(with(&|j| j.previous.file = PathBuf::from("C:\\a\nb.msi")).is_err());
    assert!(with(&|j| j.folder = Some(PathBuf::from("C:\\Program Files\\FilmCraft\\"))).is_err());
    assert!(with(&|j| j.chosen.sha256 = "ab".into()).is_err());
    assert!(with(&|j| j.previous.sha256 = SHA_A.replace('a', "g")).is_err());
    assert!(with(&|j| j.chosen.version = "0.13.0 /x".into()).is_err());
    assert!(with(&|j| j.app = "FilmCraft \"x\"".into()).is_err());
    assert!(parameters("../../Windows", &good).is_err());
}

#[test]
fn only_the_flag_starts_the_helper_and_a_bad_job_is_refused() {
    assert_eq!(parse_args(&[]), None);
    assert_eq!(parse_args(&["--hidden".into()]), None);
    assert!(matches!(parse_args(&[FLAG.into()]), Some(Err(_))));
    let run = new_run().unwrap();
    let args = split(&parameters(&run, &job(None)).unwrap());
    let mut extra = args.clone();
    extra.push("extra".into());
    assert!(matches!(parse_args(&extra), Some(Err(_))), "one argument too many");
    // Each checked part is checked again on arrival, whatever the launcher sent.
    for (at, bad) in [(1, "..\\x"), (2, "Film Craft"), (3, "0.13.0;"), (4, "ab"), (5, "")] {
        let mut changed = args.clone();
        changed[at] = bad.into();
        assert!(matches!(parse_args(&changed), Some(Err(_))), "{at}: {bad}");
    }
}

#[test]
fn an_install_folder_must_be_one_the_app_is_registered_in_for_all_users() {
    let registered = [PathBuf::from(r"C:\Program Files\ArtCraft\FilmCraft"), PathBuf::from(r"D:\Apps\FilmCraft")];
    // No folder would let msiexec take the package's default and move a chosen folder.
    assert_eq!(checked_folder(None, &registered, "FilmCraft"), Err(no_folder("FilmCraft")));
    assert_eq!(checked_folder(None, &[], "FilmCraft"), Err(no_folder("FilmCraft")));
    let same = Path::new(r"c:\program files\artcraft\filmcraft\");
    assert_eq!(checked_folder(Some(same), &registered, "FilmCraft"), Ok(Some(same)));
    for elsewhere in [r"C:\Windows\System32", r"C:\Program Files\ArtCraft\FilmCraft\sub", r"C:\Program Files\ArtCraft"] {
        let error = checked_folder(Some(Path::new(elsewhere)), &registered, "FilmCraft").unwrap_err();
        assert!(error.contains("isn't where FilmCraft is installed for all users"), "{error}");
    }
    assert!(checked_folder(Some(Path::new(r"D:\Apps\FilmCraft")), &[], "FilmCraft").is_err(), "nothing registered, no folder");
}

#[test]
fn a_staged_copy_is_kept_only_when_it_matches_the_release_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let good = package(&dir.path().join("good.msi"), b"checked bytes");
    let staged = stage(&good, dir.path(), CHOSEN).unwrap();
    assert_eq!(std::fs::read(&staged).unwrap(), b"checked bytes");

    let mut swapped = package(&dir.path().join("swapped.msi"), b"checked bytes");
    std::fs::write(&swapped.file, b"swapped bytes").unwrap();
    let error = stage(&swapped, dir.path(), PREVIOUS).unwrap_err();
    assert!(error.contains("changed after it was checked"), "{error}");
    assert!(!dir.path().join(PREVIOUS).exists(), "a source that fails its check is never copied");

    // A name already there is never written through: an existing file or link stops the stage.
    swapped.file = good.file.clone();
    swapped.sha256 = good.sha256.clone();
    std::fs::write(dir.path().join(PREVIOUS), b"planted").unwrap();
    assert!(stage(&swapped, dir.path(), PREVIOUS).is_err());
    assert_eq!(std::fs::read(dir.path().join(PREVIOUS)).unwrap(), b"planted");
}

#[test]
fn the_result_says_what_happened_and_a_missing_one_says_to_check() {
    let dir = tempfile::tempdir().unwrap();
    let missing = read_result(dir.path(), "FilmCraft").unwrap_err();
    assert!(missing.contains("Check which version of FilmCraft is installed"), "{missing}");

    write_result(dir.path(), &Ok(())).unwrap();
    assert_eq!(read_result(dir.path(), "FilmCraft"), Ok(()));
    // Written new: a second result can't replace the first.
    assert!(write_result(dir.path(), &Err("late".into())).is_err());

    let failed = tempfile::tempdir().unwrap();
    let message = "FilmCraft 0.13.0 couldn't be installed (error 1603). FilmCraft 0.14.0 was put back.";
    write_result(failed.path(), &Err(message.into())).unwrap();
    assert_eq!(read_result(failed.path(), "FilmCraft"), Err(message.into()));

    let odd = tempfile::tempdir().unwrap();
    std::fs::write(odd.path().join(RESULT), "maybe\n").unwrap();
    assert!(read_result(odd.path(), "FilmCraft").unwrap_err().contains("stopped unexpectedly"));
}

#[test]
fn old_runs_are_cleared_file_by_file_and_nothing_else_is_touched() {
    let base = tempfile::tempdir().unwrap();
    let run = run_dir(base.path(), &new_run().unwrap());
    std::fs::create_dir(&run).unwrap();
    std::fs::write(run.join(RESULT), "ok\n").unwrap();
    std::fs::write(run.join("chosen.log"), "log").unwrap();
    let stranger = run_dir(base.path(), &new_run().unwrap());
    std::fs::create_dir(&stranger).unwrap();
    std::fs::write(stranger.join("not ours.txt"), "keep").unwrap();
    let other = base.path().join("SomeoneElse");
    std::fs::create_dir(&other).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 60 * 60);
    for dir in [&run, &stranger, &other] {
        age(dir, old);
    }
    let fresh = run_dir(base.path(), &new_run().unwrap());
    std::fs::create_dir(&fresh).unwrap();
    std::fs::write(fresh.join(RESULT), "ok\n").unwrap();

    clear_old_runs(base.path());

    assert!(!run.exists(), "an old run's own files and folder go");
    assert!(stranger.join("not ours.txt").exists(), "a file a run never writes stays, and so does its folder");
    assert!(other.exists(), "a folder without the run prefix stays");
    assert!(fresh.join(RESULT).exists(), "a run from the last day stays");
}

#[cfg(windows)]
#[test]
fn the_launcher_waits_while_the_lock_is_held_and_goes_on_once_it_is_let_go() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join(LOCK);
    assert_eq!(lock_state(&lock), Ok(Lock::Missing));
    let held = std::fs::File::options().write(true).create_new(true).share_mode(0).open(&lock).unwrap();
    assert_eq!(lock_state(&lock), Ok(Lock::Held));
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        drop(held);
    });
    let began = std::time::Instant::now();
    assert_eq!(wait(&lock, std::time::Duration::from_secs(5)), Ok(()));
    assert!(began.elapsed() >= std::time::Duration::from_millis(500), "it waited for the lock");
    releaser.join().unwrap();
    assert_eq!(lock_state(&lock), Ok(Lock::Released));
}

#[cfg(windows)]
#[test]
fn a_helper_that_never_starts_is_reported_without_waiting_for_ever() {
    let dir = tempfile::tempdir().unwrap();
    let error = wait(&dir.path().join(LOCK), std::time::Duration::from_millis(300)).unwrap_err();
    assert!(error.contains("nothing was changed"), "{error}");
}

/// The source stays exactly as hashed until it is copied: nobody can write, move or delete it.
#[cfg(windows)]
#[test]
fn a_source_being_staged_cannot_be_changed_moved_or_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.msi");
    std::fs::write(&source, b"checked bytes").unwrap();
    let held = open_unchangeable(&source).unwrap();
    assert!(std::fs::OpenOptions::new().write(true).open(&source).is_err(), "written while held");
    assert!(std::fs::rename(&source, dir.path().join("moved.msi")).is_err(), "moved while held");
    assert!(std::fs::remove_file(&source).is_err(), "deleted while held");
    drop(held);
    assert!(std::fs::rename(&source, dir.path().join("moved.msi")).is_ok());
}

#[cfg(windows)]
#[test]
fn an_unsigned_package_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let unsigned = dir.path().join("unsigned.msi");
    std::fs::write(&unsigned, b"not a signed package").unwrap();
    assert!(signed_like(&unsigned, &unsigned).is_err());
}

#[test]
fn the_runs_folder_and_its_parent_must_be_real_folders() {
    let parent = tempfile::tempdir().unwrap();
    let base = parent.path().join(RUNS);
    std::fs::create_dir(&base).unwrap();
    assert_eq!(real_folders(&base), Ok(()));

    assert!(real_folders(&parent.path().join("missing")).is_err(), "a folder that isn't there");
    let file = parent.path().join("a file");
    std::fs::write(&file, "x").unwrap();
    assert!(real_folders(&file).is_err(), "a file in the folder's place");
    assert!(real_folders(&file.join(RUNS)).is_err(), "a parent that isn't a folder");
}

/// A link where the runs folder should be is refused, not followed. Skipped where this account
/// may not make links (Windows without Developer Mode).
#[test]
fn a_link_in_place_of_the_runs_folder_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("elsewhere");
    std::fs::create_dir(&target).unwrap();
    let link = parent.path().join(RUNS);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&target, &link);
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, &link);
    if made.is_err() {
        return;
    }
    assert!(real_folders(&link).is_err());
}

#[cfg(windows)]
#[test]
fn a_package_must_share_the_subject_and_the_issuing_organisation() {
    let name = |cn: &str, o: Option<&str>| verifysign::Name { common_name: Some(cn.into()), organization: o.map(Into::into), ..Default::default() };
    let signer = |subject: &str, ca: &str, ca_org: Option<&str>| Signer { subject: name(subject, Some(subject)), issuer: name(ca, ca_org) };
    let ours = signer("Learning Machines Inc", "Microsoft ID Verified CS AOC CA 04", Some("Microsoft Corporation"));

    // Trusted Signing rotates the issuing CA between releases: still our publisher.
    assert!(same_publisher(&ours, &signer("Learning Machines Inc", "Microsoft ID Verified CS AOC CA 03", Some("Microsoft Corporation"))));
    assert!(!same_publisher(&ours, &signer("Someone Else", "Microsoft ID Verified CS AOC CA 04", Some("Microsoft Corporation"))), "another subject");
    assert!(!same_publisher(&ours, &signer("Learning Machines Inc", "Other CA", Some("Other Trust Services"))), "our name from another authority");
    let nameless = signer("Learning Machines Inc", "Self", None);
    assert!(!same_publisher(&nameless, &nameless), "an issuer with no organisation proves nothing");
}

#[test]
fn staged_copies_go_however_the_run_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (chosen, previous) = (dir.path().join(CHOSEN), dir.path().join(PREVIOUS));
    std::fs::write(&chosen, "c").unwrap();
    let staged = Staged(vec![chosen.clone(), previous.clone()]);
    drop(staged);
    assert!(!chosen.exists(), "a staged copy goes");
    assert!(!previous.exists(), "a copy that was never staged is no error");
}

#[test]
fn the_helper_is_only_ever_the_one_beside_the_launcher() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir.path().join("artcraft-launcher.exe");
    let error = helper_beside(&launcher).unwrap_err();
    assert!(error.contains("nothing was changed"), "{error}");
    std::fs::create_dir(dir.path().join(HELPER)).unwrap();
    assert!(helper_beside(&launcher).is_err(), "a folder of that name is not the helper");
    std::fs::remove_dir(dir.path().join(HELPER)).unwrap();
    std::fs::write(dir.path().join(HELPER), b"helper").unwrap();
    assert_eq!(helper_beside(&launcher).unwrap(), dir.path().join(HELPER));
}

#[cfg(windows)]
#[test]
fn an_unsigned_helper_is_never_started() {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join(HELPER);
    std::fs::write(&helper, b"not a signed program").unwrap();
    let error = checked_helper(&helper, &helper).unwrap_err();
    assert!(error.contains("isn't signed") && error.contains("Nothing was changed"), "{error}");
    let error = checked_helper(&helper, &dir.path().join("missing.exe")).unwrap_err();
    assert!(error.contains("couldn't find the installer helper"), "{error}");
}

/// The helper is held while it starts: Windows must still run a program held this way.
#[cfg(windows)]
#[test]
fn a_held_program_can_still_be_started_but_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join(HELPER);
    std::fs::copy(super::super::windows::msiexec().with_file_name("hostname.exe"), &program).unwrap();
    let held = open_unchangeable(&program).unwrap();
    assert!(std::fs::OpenOptions::new().write(true).open(&program).is_err(), "changed while held");
    assert!(std::fs::rename(&program, dir.path().join("moved.exe")).is_err(), "moved while held");
    let ran = std::process::Command::new(&program).output().unwrap();
    assert!(ran.status.success(), "{ran:?}");
    drop(held);
}

#[test]
fn a_community_app_is_refused_before_any_prompt() {
    let community = CraftApp::by_id("solidcraft").unwrap();
    assert_eq!(community.kind, AppKind::Community);
    let refused = refusal(community).unwrap();
    assert!(refused.contains("Nothing was changed") && refused.contains(community.name), "{refused}");
    assert_eq!(refusal(CraftApp::by_id("photocraft").unwrap()), None);
}
