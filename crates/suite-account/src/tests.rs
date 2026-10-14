//! Tests for the store, the migration and the fallback when nobody is signed in.
//!
//! A keychain is awkward to drive from a test. Everything above it therefore runs against
//! [`Memory`](crate::store::Memory). The deployment runs against [`Canned`].

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::accounts::{self, Account};
use crate::paths::{self, Probe, Rule};
use crate::service::{Flow, Identity, Polled, Service};
use crate::store::{Memory, TokenStore};
use crate::{Adopted, Error, NewAccount, Secret, Shared, SignedOut, Store};

const SERVICE: &str = "https://craft.example.com";
const USER: &str = "e9b1c0e4-0000-4000-8000-000000000001";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const NOW: i64 = 1_760_000_000;

fn probe(env: &[(&str, &str)], exe_dir: Option<&str>, config: Option<&str>) -> Probe {
    let env: Vec<(String, String)> = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    Probe {
        env: Box::new(move |name| env.iter().find(|(k, _)| k == name).map(|(_, v)| OsString::from(v))),
        exe_dir: exe_dir.map(PathBuf::from),
        platform_config_dir: config.map(PathBuf::from),
    }
}

fn new_account() -> NewAccount {
    NewAccount { service: SERVICE.to_owned(), user: USER.to_owned(), display: "Ada Lovelace".to_owned(), email: None, linked: true }
}

/// A deployment with canned answers, recording what it was asked to do.
#[derive(Default)]
struct Canned {
    /// What `revoke` should answer, when it should fail.
    refuse_revoke: Option<String>,
    revoked: RefCell<Vec<String>>,
}

impl Canned {
    fn refusing(why: &str) -> Canned {
        Canned { refuse_revoke: Some(why.to_owned()), revoked: RefCell::default() }
    }
}

impl Service for Canned {
    fn start(&self, _base: &str) -> Result<Flow, String> {
        Ok(Flow { device_code: "dc".to_owned(), verify_url: "https://idp.example.com/authorize".to_owned(), expires_in: 600, interval: 2 })
    }

    fn poll(&self, _base: &str, _device_code: &str) -> Result<Polled, String> {
        Ok(Polled::Done(Secret::new(TOKEN)))
    }

    fn identity(&self, _base: &str, _token: &Secret) -> Result<Option<Identity>, String> {
        Ok(Some(Identity { user: USER.to_owned(), display: "Ada Lovelace".to_owned(), email: None, linked: true }))
    }

    fn revoke(&self, base: &str, _token: &Secret) -> Result<(), String> {
        self.revoked.borrow_mut().push(base.to_owned());
        match &self.refuse_revoke {
            Some(why) => Err(why.clone()),
            None => Ok(()),
        }
    }
}

// ---- where the store lives -------------------------------------------------

#[test]
fn the_variable_names_the_folder() {
    let r = paths::resolve(&probe(&[(paths::ENV_VAR, "/somewhere/else")], None, Some("/home/a/.config")));
    assert_eq!(r.rule, Rule::EnvVar);
    assert_eq!(r.dir.as_deref(), Some(Path::new("/somewhere/else")));
}

#[test]
fn an_empty_variable_keeps_nothing() {
    let r = paths::resolve(&probe(&[(paths::ENV_VAR, "")], None, Some("/home/a/.config")));
    assert_eq!(r.rule, Rule::Isolated);
    assert_eq!(r.dir, None);
}

#[test]
fn the_platform_folder_is_the_default() {
    let r = paths::resolve(&probe(&[], None, Some("/home/a/.config")));
    assert_eq!(r.rule, Rule::Platform);
    assert_eq!(r.dir.as_deref(), Some(Path::new("/home/a/.config").join(paths::DIR).as_path()));
}

#[test]
fn a_portable_marker_wins_over_the_platform_folder() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("portable.txt"), b"").unwrap();
    let exe_dir = dir.path().display().to_string();
    let r = paths::resolve(&probe(&[], Some(&exe_dir), Some("/home/a/.config")));
    assert_eq!(r.rule, Rule::Portable);
    assert_eq!(r.dir.as_deref(), Some(dir.path().join(paths::PORTABLE_DIR).as_path()));
}

#[test]
fn no_platform_folder_keeps_nothing() {
    let r = paths::resolve(&probe(&[], None, None));
    assert_eq!(r.rule, Rule::Nowhere);
    assert_eq!(r.dir, None);
}

#[test]
fn the_handle_is_stable_and_separates_accounts() {
    let a = accounts::handle(SERVICE, USER);
    assert_eq!(a, accounts::handle(SERVICE, USER), "the same account twice is the same entry");
    // A trailing slash is the same deployment typed differently.
    assert_eq!(a, accounts::handle("https://craft.example.com/", USER));
    assert_ne!(a, accounts::handle("https://other.example.com", USER), "another deployment is another account");
    assert_ne!(a, accounts::handle(SERVICE, "somebody-else"), "another person is another account");
    assert_eq!(a.len(), 24);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

// ---- signing in and out ----------------------------------------------------

#[test]
fn a_sign_in_can_be_read_back_by_the_next_application() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(dir.path().to_path_buf()), keychain.clone());
    let account = store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();

    // A second application, its own Store over the same folder and keychain.
    let other = Store::at(Some(dir.path().to_path_buf()), keychain);
    match other.shared() {
        Shared::Session { account: found, token } => {
            assert_eq!(found.id, account.id);
            assert_eq!(found.service, SERVICE);
            assert_eq!(found.display, "Ada Lovelace");
            assert_eq!(token, Secret::new(TOKEN));
        }
        other => panic!("expected a session, got {other:?}"),
    }
}

#[test]
fn the_accounts_file_never_holds_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::new());
    store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();
    let on_disk = std::fs::read_to_string(store.path().unwrap()).unwrap();
    assert!(!on_disk.contains(TOKEN), "the token reached the accounts file");
    assert!(on_disk.contains(SERVICE), "the file should still name the deployment");
}

#[test]
fn signing_in_again_replaces_the_entry_rather_than_adding_one() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(dir.path().to_path_buf()), keychain.clone());
    let first = store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();
    let second = store.sign_in(&new_account(), &Secret::new("a-newer-token"), NOW + 60).unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.added, second.added, "the first sign-in's date is kept");
    assert_eq!(keychain.count(), 1);
    assert_eq!(store.accounts().unwrap().accounts.len(), 1);
    match store.shared() {
        Shared::Session { token, .. } => assert_eq!(token, Secret::new("a-newer-token")),
        other => panic!("expected a session, got {other:?}"),
    }
}

#[test]
fn two_deployments_are_two_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(dir.path().to_path_buf()), keychain.clone());
    store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();
    let elsewhere = NewAccount { service: "https://other.example.com".to_owned(), ..new_account() };
    let second = store.sign_in(&elsewhere, &Secret::new("another-token"), NOW).unwrap();
    assert_eq!(keychain.count(), 2);
    let list = store.accounts().unwrap();
    assert_eq!(list.accounts.len(), 2);
    assert_eq!(list.active.as_deref(), Some(second.id.as_str()), "the newest sign-in becomes current");
}

#[test]
fn signing_out_ends_the_session_at_the_deployment_and_clears_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(dir.path().to_path_buf()), keychain.clone());
    let account = store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();

    let deployment = Canned::default();
    assert_eq!(store.sign_out(&deployment, &account.id).unwrap(), SignedOut::Everywhere);
    assert_eq!(deployment.revoked.borrow().as_slice(), [SERVICE.to_owned()]);
    assert!(!keychain.has(&account.id), "the keychain entry should be gone");
    assert!(store.accounts().unwrap().accounts.is_empty());
    assert!(matches!(store.shared(), Shared::None));
}

#[test]
fn a_deployment_that_cannot_be_reached_still_clears_the_local_half() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(dir.path().to_path_buf()), keychain.clone());
    let account = store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();

    let deployment = Canned::refusing("the deployment could not be reached");
    match store.sign_out(&deployment, &account.id).unwrap() {
        SignedOut::LocalOnly { why } => assert!(why.contains("could not be reached")),
        other => panic!("expected a local sign-out, got {other:?}"),
    }
    assert!(!keychain.has(&account.id));
    assert!(matches!(store.shared(), Shared::None));
}

#[test]
fn signing_out_an_unknown_account_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::new());
    assert_eq!(store.sign_out(&Canned::default(), "nobody").unwrap_err(), Error::Unknown("nobody".to_owned()));
}

// ---- nobody is signed in ---------------------------------------------------

#[test]
fn an_empty_suite_offers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::new());
    assert!(matches!(store.shared(), Shared::None));
    assert!(store.accounts().unwrap().accounts.is_empty());
}

#[test]
fn an_isolated_store_offers_nothing_and_signs_nobody_in() {
    let store: Store<Memory> = Store::at(None, Memory::new());
    assert!(matches!(store.shared(), Shared::None));
    assert_eq!(store.usable().unwrap_err(), Error::Isolated);
    assert_eq!(store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap_err(), Error::Isolated);
}

#[test]
fn a_machine_with_no_keychain_signs_nobody_in() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::missing("no Secret Service on this machine"));
    assert!(matches!(store.shared(), Shared::None));
    match store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap_err() {
        Error::Keychain(why) => assert!(why.contains("no Secret Service")),
        other => panic!("expected a keychain failure, got {other:?}"),
    }
    assert!(!store.path().unwrap().exists(), "a refused sign-in should write no file");
}

#[test]
fn an_unreadable_keychain_says_so_rather_than_pretending_nobody_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    // Sign in against a working keychain, then read the same folder through a locked one.
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::new());
    let account = store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();

    let locked = Store::at(Some(dir.path().to_path_buf()), Memory::refusing("the keychain is locked"));
    match locked.shared() {
        Shared::Unreadable { account: found, why } => {
            assert_eq!(found.id, account.id);
            assert!(why.contains("locked"));
        }
        other => panic!("expected an unreadable session, got {other:?}"),
    }
}

#[test]
fn an_account_whose_entry_is_gone_reads_as_nobody_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(dir.path().to_path_buf()), keychain.clone());
    let account = store.sign_in(&new_account(), &Secret::new(TOKEN), NOW).unwrap();
    // Somebody deleted the credential from the platform's own window.
    keychain.remove(&account.id).unwrap();
    assert!(matches!(store.shared(), Shared::None));
}

#[test]
fn an_active_account_that_is_no_longer_listed_reads_as_nobody_signed_in() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(paths::FILE);
    let list = accounts::Accounts { format: accounts::FORMAT, accounts: Vec::new(), active: Some("vanished".to_owned()) };
    accounts::write(&path, &list).unwrap();
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::new());
    assert!(matches!(store.shared(), Shared::None));
}

#[test]
fn a_file_from_a_later_format_is_refused_rather_than_misread() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(paths::FILE);
    std::fs::write(&path, br#"{"format":99,"accounts":[],"active":null}"#).unwrap();
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::new());
    assert!(matches!(store.accounts(), Err(Error::Malformed(_))));
    // An application still starts: the shared answer is that nobody is signed in.
    assert!(matches!(store.shared(), Shared::None));
}

#[test]
fn unreadable_json_does_not_stop_an_application_starting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(paths::FILE), b"this is not JSON").unwrap();
    let store = Store::at(Some(dir.path().to_path_buf()), Memory::new());
    assert!(matches!(store.shared(), Shared::None));
}

// ---- taking over a per-application session file ----------------------------

/// What GridCraft's `collabcraft-session.json` is read as.
fn legacy_file(dir: &Path) -> PathBuf {
    let path = dir.join("collabcraft-session.json");
    let body = format!(r#"{{"service":"{SERVICE}","token":"{TOKEN}","user":"{USER}","display":"Ada Lovelace"}}"#);
    std::fs::write(&path, body.as_bytes()).unwrap();
    path
}

#[test]
fn a_per_application_session_moves_into_the_suite_store_and_the_file_goes() {
    let suite = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(suite.path().to_path_buf()), keychain.clone());
    let path = legacy_file(app.path());

    match crate::migrate::adopt(&store, &path, NOW).unwrap() {
        Adopted::Taken(account) => {
            assert_eq!(account.service, SERVICE);
            assert_eq!(account.user, USER);
            assert!(keychain.has(&account.id), "the token should be in the keychain");
        }
        other => panic!("expected the session to be taken over, got {other:?}"),
    }
    assert!(!path.exists(), "the session file should be gone");
    match store.shared() {
        Shared::Session { token, .. } => assert_eq!(token, Secret::new(TOKEN)),
        other => panic!("expected a session, got {other:?}"),
    }
}

#[test]
fn a_second_migration_finds_nothing_to_do() {
    let suite = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let store = Store::at(Some(suite.path().to_path_buf()), Memory::new());
    let path = legacy_file(app.path());
    crate::migrate::adopt(&store, &path, NOW).unwrap();
    assert_eq!(crate::migrate::adopt(&store, &path, NOW).unwrap(), Adopted::Nothing);
}

#[test]
fn migration_with_no_keychain_leaves_the_file_alone() {
    let suite = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let store = Store::at(Some(suite.path().to_path_buf()), Memory::missing("no keychain here"));
    let path = legacy_file(app.path());

    match crate::migrate::adopt(&store, &path, NOW).unwrap() {
        Adopted::Kept { why } => assert!(why.contains("no keychain here")),
        other => panic!("expected the file to be kept, got {other:?}"),
    }
    assert!(path.exists(), "removing it would have signed the user out");
}

#[test]
fn migration_in_an_isolated_process_leaves_the_file_alone() {
    let app = tempfile::tempdir().unwrap();
    let store: Store<Memory> = Store::at(None, Memory::new());
    let path = legacy_file(app.path());
    assert!(matches!(crate::migrate::adopt(&store, &path, NOW).unwrap(), Adopted::Kept { .. }));
    assert!(path.exists());
}

#[test]
fn a_session_file_carrying_no_session_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("collabcraft-session.json");
    std::fs::write(&path, br#"{"service":"https://craft.example.com","token":"  "}"#).unwrap();
    assert!(matches!(crate::migrate::read(&path), Err(Error::Malformed(_))));
}

/// The four keys an application writes today, spelled exactly as it writes them.
#[test]
fn the_four_keys_an_application_writes_today_adopt_cleanly() {
    let suite = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(suite.path().to_path_buf()), keychain.clone());
    let path = app.path().join("collabcraft-session.json");
    let body = format!(r#"{{"token":"{TOKEN}","service":"{SERVICE}","subject":"{USER}","display":"Ada Lovelace"}}"#);
    std::fs::write(&path, body.as_bytes()).unwrap();

    match crate::migrate::adopt(&store, &path, NOW).unwrap() {
        Adopted::Taken(account) => {
            assert_eq!(account.service, SERVICE);
            assert_eq!(account.user, USER, "`subject` is the subject the handle is keyed on");
            assert_eq!(account.display, "Ada Lovelace");
            assert_eq!(account.id, accounts::handle(SERVICE, USER));
            assert!(keychain.has(&account.id));
        }
        other => panic!("expected the session to be taken over, got {other:?}"),
    }
    assert!(!path.exists());
}

/// What is on every machine that signed in before the application recorded anything else.
#[test]
fn a_token_only_session_file_is_left_alone_until_the_application_records_more() {
    let suite = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(suite.path().to_path_buf()), keychain.clone());
    let path = app.path().join("collabcraft-session.json");
    std::fs::write(&path, format!(r#"{{"token":"{TOKEN}"}}"#).as_bytes()).unwrap();

    // It parses, and it says nothing to key an account on.
    let legacy = crate::migrate::read(&path).unwrap().unwrap();
    assert_eq!(legacy.token, Secret::new(TOKEN));
    assert_eq!(legacy.service, "");
    assert_eq!(legacy.user, "", "the subject is never stood in for");
    assert_eq!(legacy.incomplete(), Some("which deployment it belongs to"));

    match crate::migrate::adopt(&store, &path, NOW).unwrap() {
        Adopted::Kept { why } => assert!(why.contains("does not say"), "{why}"),
        other => panic!("expected the file to be kept, got {other:?}"),
    }
    assert!(path.exists(), "removing it would have signed the user out");
    assert_eq!(keychain.count(), 0, "nothing should be filed under a made-up handle");
    assert!(store.accounts().unwrap().accounts.is_empty());
    assert!(matches!(store.shared(), Shared::None));

    // The application rewrites the file once it knows the rest. The next startup adopts it.
    let body = format!(r#"{{"token":"{TOKEN}","service":"{SERVICE}","subject":"{USER}","display":"Ada Lovelace"}}"#);
    std::fs::write(&path, body.as_bytes()).unwrap();
    assert!(matches!(crate::migrate::adopt(&store, &path, NOW).unwrap(), Adopted::Taken(_)));
    assert!(!path.exists());
    match store.shared() {
        Shared::Session { account, token } => {
            assert_eq!(account.id, accounts::handle(SERVICE, USER));
            assert_eq!(token, Secret::new(TOKEN));
        }
        other => panic!("expected a session, got {other:?}"),
    }
}

/// The deployment is recorded the moment the device flow completes. The subject only arrives when
/// `/api/v1/me` first answers, so a file can hold one without the other.
#[test]
fn a_session_file_with_no_subject_is_left_alone() {
    let suite = tempfile::tempdir().unwrap();
    let app = tempfile::tempdir().unwrap();
    let keychain = Memory::new();
    let store = Store::at(Some(suite.path().to_path_buf()), keychain.clone());
    let path = app.path().join("collabcraft-session.json");
    std::fs::write(&path, format!(r#"{{"token":"{TOKEN}","service":"{SERVICE}"}}"#).as_bytes()).unwrap();

    let legacy = crate::migrate::read(&path).unwrap().unwrap();
    assert_eq!(legacy.service, SERVICE);
    assert_eq!(legacy.incomplete(), Some("whose account it is"));

    match crate::migrate::adopt(&store, &path, NOW).unwrap() {
        Adopted::Kept { why } => assert!(why.contains("whose account it is"), "{why}"),
        other => panic!("expected the file to be kept, got {other:?}"),
    }
    assert!(path.exists());
    assert_eq!(keychain.count(), 0);
}

#[test]
fn a_session_file_spelled_the_other_way_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("collabcraft-session.json");
    let body = format!(r#"{{"base_url":"{SERVICE}/","access_token":"{TOKEN}","sub":"{USER}","name":"Ada"}}"#);
    std::fs::write(&path, body.as_bytes()).unwrap();
    let legacy = crate::migrate::read(&path).unwrap().unwrap();
    assert_eq!(legacy.service, SERVICE, "the trailing slash comes off");
    assert_eq!(legacy.user, USER);
    assert_eq!(legacy.display, "Ada");
    assert_eq!(legacy.token, Secret::new(TOKEN));
}

// ---- the secret itself -----------------------------------------------------

#[test]
fn a_secret_never_prints_itself() {
    let secret = Secret::new(TOKEN);
    let shown = format!("{secret:?}");
    assert!(!shown.contains(TOKEN), "Debug printed the token: {shown}");
    assert_eq!(shown, format!("Secret({} bytes)", TOKEN.len()));
    assert_eq!(secret.len(), TOKEN.len());
}

#[test]
fn an_account_record_prints_no_secret() {
    let account = Account { id: "abc".to_owned(), service: SERVICE.to_owned(), ..Account::default() };
    assert!(!format!("{account:?}").contains(TOKEN));
}
