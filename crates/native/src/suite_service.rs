//! Talking to a CollabCraft deployment, behind a trait, and the sign-out that needs it.
//!
//! RFC-0025 (storytold/craft-contrib#35) decides that the shared account crate names no provider.
//! Its point 3: the provider-neutral parts become the shared secret layer, and the CollabCraft
//! device-flow client "moves out, to live beside the provider adapter it belongs to". The launcher
//! holds the only implementation of that client, so the contract it implements lives here with it.
//! `craft-suite-account` keeps the account list, the keychain entry and nothing about a provider.
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

/// One CollabCraft deployment, as the launcher's sign-in needs it.
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
/// A deployment that cannot be reached does not block the local half. The user asked to be signed
/// out. Clearing the entry and saying the server was not told beats leaving a readable token in
/// the keychain.
pub fn sign_out(store: &Store, service: &dyn Service, id: &str) -> Result<SignedOut, Error> {
    let account = store.accounts()?.get(id).cloned().ok_or_else(|| Error::Unknown(id.to_owned()))?;
    // Read through the same keychain the store writes with: one suite service name, one entry per
    // handle, so this is the entry `store.discard` is about to remove.
    let told = match Keychain.get(id) {
        Ok(Some(token)) => service.revoke(&account.service, &token).err(),
        // No token to present. There is nothing the deployment would accept. Clearing the row is
        // the whole of the job.
        Ok(None) => None,
        Err(why) => Some(why),
    };
    store.discard(id)?;
    Ok(match told {
        None => SignedOut::Everywhere,
        Some(why) => SignedOut::LocalOnly { why },
    })
}
