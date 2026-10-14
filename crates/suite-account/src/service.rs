//! Talking to a CollabCraft deployment, behind a trait.
//!
//! This crate carries no HTTP client. The store is the part every Craft App needs. An application
//! taking the store should not have to take a transport it already has. The launcher implements
//! [`Service`] over the agent it already uses. A test implements it with canned answers.
//!
//! The flow is the service's own. Nothing here invents it. A desktop application cannot host a
//! redirect worth trusting. It therefore starts a sign-in, sends the user to a browser, and polls:
//!
//! ```text
//! POST <base>/auth/device/start   -> { device_code, verify_url, expires_in, interval }
//! POST <base>/auth/device/poll    -> 200 { token } | 428 authorization_pending | 404 expired
//! GET  <base>/api/v1/me           -> who the token belongs to     (Authorization: Bearer)
//! POST <base>/auth/logout         -> 204, every session for that user ended
//! ```

use crate::secret::Secret;

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

/// One CollabCraft deployment, as the account store needs it.
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
    /// The deployment drops every session it holds for that user. One sign-out therefore covers the
    /// suite and every machine the person left a session on.
    fn revoke(&self, base: &str, token: &Secret) -> Result<(), String>;
}
