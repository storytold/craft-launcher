//! Talking to the configured deployment, behind a trait, and the sign-out that needs it.
//!
//! RFC-0025 (storytold/craft-contrib#35) decides that the shared account crate names no provider.
//! Its point 3: the provider-neutral parts become the shared secret layer, and the device-flow
//! client "moves out, to live beside the provider adapter it belongs to". The client itself names
//! no provider either: a configurable base URL and the four endpoints below are everything it
//! speaks, and any deployment that serves them answers. The launcher holds the only implementation
//! of that client today, so the contract it implements lives here with it. `craft-suite-account`
//! keeps the account list, the keychain entry and nothing about a provider.
//!
//! The flow is the deployment's own. Nothing here invents it. A desktop application cannot host a
//! redirect worth trusting. It therefore starts a sign-in, sends the user to a browser, and polls:
//!
//! ```text
//! POST <base>/auth/device/start   -> { device_code, verify_url, expires_in, interval }
//! POST <base>/auth/device/poll    -> 200 { token } | 428 authorization_pending | 404 expired
//! GET  <base>/api/v1/me           -> who the token belongs to     (Authorization: Bearer)
//! POST <base>/auth/logout         -> 204, every session for that user ended
//! ```

use craft_suite_account::{Error, Keychain, Secret, Store, TokenStore};

use crate::service_base;

/// Where to open the provider's conversation app with no document in hand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConversationHome {
    /// What the provider calls it (Talk, …).
    pub name: String,
    pub url: String,
}

/// Who a token belongs to, as the deployment reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// The deployment's own identifier: the OIDC subject.
    pub user: String,
    pub display: String,
    pub email: Option<String>,
    /// Whether cloud storage is linked. Without it Open and Save have nowhere to go. Saying so
    /// here is kinder than a failure at the moment of saving.
    pub linked: bool,
    /// The provider's conversation lobby, when the deployment has one. Absent where chat is
    /// unsupported, so a launcher draws no button rather than a dead one.
    pub conversation: Option<ConversationHome>,
}

/// A sign-in the user has to finish in a browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Flow {
    /// Kept by the application and sent back when polling. Never shown to the user.
    pub device_code: String,
    /// Where to send the browser.
    pub verify_url: String,
    /// How long the deployment will wait, in seconds.
    pub expires_in: u64,
    /// How long to leave between polls, in seconds.
    pub interval: u64,
}

/// What a poll found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Polled {
    /// The user has not finished in the browser yet. An ordinary state to loop on.
    Pending,
    Done(Secret),
    /// The deployment gave up waiting. Start another sign-in.
    Expired,
}

/// One suite deployment, speaking the device sign-in protocol above, as the launcher's sign-in
/// needs it. Nothing here names a provider: any deployment that serves the four endpoints answers.
pub trait Service {
    /// Begin a sign-in.
    fn start(&self, base: &str) -> Result<Flow, String>;
    /// Collect the session once the browser is done.
    fn poll(&self, base: &str, device_code: &str) -> Result<Polled, String>;
    /// Who holds this token. `Ok(None)` means the deployment no longer honours it. That is how a
    /// session revoked elsewhere shows up.
    fn identity(&self, base: &str, token: &Secret) -> Result<Option<Identity>, String>;
    /// End the session at the deployment.
    ///
    /// The deployment drops every session it holds for that user. One sign-out therefore covers
    /// the suite and every machine the person left a session on.
    fn revoke(&self, base: &str, token: &Secret) -> Result<(), String>;
}

/// How a sign-out ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignedOut {
    /// The deployment ended the session and the keychain entry is gone.
    Everywhere,
    /// The keychain entry is gone and the deployment could not be told. The session may still be
    /// live there. Worth saying out loud. The user asked to be signed out and half of it happened.
    LocalOnly { why: String },
}

/// Ends `id`'s session at the deployment, then clears it from the store.
///
/// This was `Store::sign_out` in the pre-split crate. It cannot stay there: the store would have
/// to call a provider, which is what RFC-0025 forbids. The order is the same as before. The
/// deployment is told first, while the token can still be read. It drops every session it holds
/// for that user, so one action signs the person out of the whole suite and of any other machine
/// they left a session on.
///
/// A deployment that cannot be reached does not block the local half. Neither does a row whose
/// service or handle failed validation: the network call is refused, but the row is cleared all
/// the same. The user asked to be signed out. Clearing the entry and saying the server was not
/// told beats leaving a readable token in the keychain, whichever of the two is why it was not
/// told.
pub fn sign_out<S: TokenStore>(store: &Store<S>, service: &dyn Service, id: &str) -> Result<SignedOut, Error> {
    let account = store.accounts()?.get(id).cloned().ok_or_else(|| Error::Unknown(id.to_owned()))?;
    // The same gate `account::state_now` applies before an `identity` call: a row whose `service`
    // was redirected, or whose `id` no longer matches its own handle, must not have its token
    // presented anywhere, including here, to the deployment's own logout endpoint. That gate is
    // about the network call alone. The local half still has to go: a row a validation failure
    // refuses to sign out anywhere would be a row nothing can ever remove, and a keychain entry
    // nothing can ever clear, which is worse than the plain refusal it replaced.
    let told = match service_base::validate_account_target(&account) {
        Err(why) => {
            log::warn!("a stored suite account did not pass target validation on sign-out: {why}");
            Some(why)
        }
        // Read through the same keychain the store writes with: one suite service name, one entry
        // per handle, so this is the entry `store.discard` is about to remove. Production builds
        // this with `Store<Keychain>`, where `Keychain.get` and `store`'s own token store are the
        // same backend under the same handle; a test can substitute `S` to cover `discard` below
        // without a real keychain, since that read never goes through `store` either way.
        Ok(()) => match Keychain.get(id) {
            Ok(Some(token)) => service.revoke(&account.service, &token).err(),
            // No token to present, so the deployment cannot be told. Its session may still be
            // live there, and `Everywhere` would claim otherwise. Clearing the row is still the
            // right half to do.
            Ok(None) => Some("there was no stored token to present".to_owned()),
            Err(why) => Some(why),
        },
    };
    store.discard(id)?;
    Ok(match told {
        None => SignedOut::Everywhere,
        Some(why) => SignedOut::LocalOnly { why },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use craft_suite_account::{Account, Accounts, Memory};

    use super::*;

    /// A deployment stand-in that counts how many times it was asked to end a session. The
    /// security property under test is that it is asked zero times.
    #[derive(Default)]
    struct Counting {
        revoke_calls: AtomicUsize,
    }

    impl Service for Counting {
        fn start(&self, _base: &str) -> Result<Flow, String> {
            Err("not used by this test".to_owned())
        }

        fn poll(&self, _base: &str, _device_code: &str) -> Result<Polled, String> {
            Err("not used by this test".to_owned())
        }

        fn identity(&self, _base: &str, _token: &Secret) -> Result<Option<Identity>, String> {
            Err("not used by this test".to_owned())
        }

        fn revoke(&self, _base: &str, _token: &Secret) -> Result<(), String> {
            self.revoke_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Seeds `dir` with a one-row account list and a keychain stand-in entry behind it, bypassing
    /// `Store::sign_in`, which always writes a row whose `id` is the correct handle for its own
    /// `service` and `user`. Both attacks under test need a row `sign_in` would never produce.
    ///
    /// A `Memory` stand-in, not the real `Keychain`: `sign_out`'s own refusal path never reads the
    /// keychain at all, but `store.discard` always does, win or refuse, and a real keychain is not
    /// available in every environment this runs in. `sign_out`'s one hard-coded `Keychain.get` on
    /// the accepted path is unaffected either way, since that read never goes through `store`.
    fn seed(dir: &std::path::Path, account: Account) -> (Store<Memory>, Memory) {
        let tokens = Memory::new();
        let store = Store::at(Some(dir.to_path_buf()), tokens.clone());
        let path = store.path().expect("a store built with a directory has a path");
        let mut list = Accounts { active: Some(account.id.clone()), ..Accounts::default() };
        list.accounts.push(account.clone());
        craft_suite_account::accounts::write(&path, &list).expect("writing the seeded account list");
        tokens.put(&account.id, &Secret::new("a-session-token")).expect("seeding the token");
        (store, tokens)
    }

    /// A row whose `service` is a plain, non-loopback `http://` address must not have its token
    /// presented to the deployment's logout endpoint. The local half still has to run: the row and
    /// its keychain entry are gone afterward, the same as a sign-out `sign_out` could tell the
    /// deployment about.
    #[test]
    fn a_non_loopback_http_service_is_never_told_to_revoke() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let id = craft_suite_account::accounts::handle("http://collab.lan:8080", "u");
        let account = Account {
            id: id.clone(),
            service: "http://collab.lan:8080".to_owned(),
            user: "u".to_owned(),
            display: "Someone".to_owned(),
            ..Account::default()
        };
        let (store, tokens) = seed(dir.path(), account);

        let service = Counting::default();
        let result = sign_out(&store, &service, &id);

        assert!(matches!(result, Ok(SignedOut::LocalOnly { .. })), "{result:?}");
        assert_eq!(service.revoke_calls.load(Ordering::SeqCst), 0, "the token must never reach a service address that failed validation");
        assert!(store.accounts().expect("the account list").get(&id).is_none(), "the row must still be removed locally");
        assert!(!tokens.has(&id), "the keychain stand-in entry must still be cleared locally");
    }

    /// A row whose `service` was edited in place while its `id` was left as it was (so the
    /// handle no longer matches what `accounts::handle(service, user)` computes) must be refused
    /// the same way, not waved through because the address alone looks allowed, and cleared the
    /// same way too.
    #[test]
    fn an_edited_service_that_kept_the_old_id_is_never_told_to_revoke() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // The id a sign-in would have minted for the original, honest service. Kept as-is while
        // `service` below is swapped for a different, equally well-formed address.
        let id = craft_suite_account::accounts::handle("https://good.example.test", "u");
        let account = Account {
            id: id.clone(),
            service: "https://evil.example.test".to_owned(),
            user: "u".to_owned(),
            display: "Someone".to_owned(),
            ..Account::default()
        };
        let (store, tokens) = seed(dir.path(), account);

        let service = Counting::default();
        let result = sign_out(&store, &service, &id);

        assert!(matches!(result, Ok(SignedOut::LocalOnly { .. })), "{result:?}");
        assert_eq!(service.revoke_calls.load(Ordering::SeqCst), 0, "a service edited past its own handle must not receive the token either");
        assert!(store.accounts().expect("the account list").get(&id).is_none(), "the row must still be removed locally");
        assert!(!tokens.has(&id), "the keychain stand-in entry must still be cleared locally");
    }
}
