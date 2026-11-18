//! The suite's shared sign-in, as the launcher drives it.
//!
//! The launcher is the one place somebody signs in to the Craft App Suite. The account list and
//! the keychain entry are [`craft_suite_account`]'s. This module is the half that talks to a
//! CollabCraft deployment, against the contract in [`crate::suite_service`]. It runs the waiting
//! about on a worker thread, so the window never blocks on it.
//!
//! Nothing here is mandatory. With no deployment configured, [`Accounts`] reports
//! [`SuiteAccount::Unconfigured`] and makes no request at all. An application started on a machine
//! where nobody signed in through the launcher behaves exactly as it did before: it finds no shared
//! session and offers its own.
//!
//! No token value is ever logged, formatted into a message, or put on a command line. The one place
//! it goes is an `Authorization` header.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use artcraft_launcher_core::protocol::{SuiteAccount, SuiteIdentity};
use craft_suite_account::{Account, NewAccount, Secret, Shared, Store, TokenStore};
use zeroize::Zeroizing;

use crate::suite_service::{self, ConversationHome, Flow, Identity, Polled, Service, SignedOut};

use crate::http::Http;
use crate::service_base;

/// Clears [`Accounts::busy`] on drop so a panicked worker does not stick the UI.
struct BusyGuard {
    busy: Arc<AtomicBool>,
    live: bool,
}

impl BusyGuard {
    fn new(busy: Arc<AtomicBool>) -> BusyGuard {
        BusyGuard { busy, live: true }
    }

    fn clear(&mut self) {
        if self.live {
            self.busy.store(false, Ordering::SeqCst);
            self.live = false;
        }
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.clear();
    }
}

/// A CollabCraft deployment over the launcher's own HTTPS agent. It then follows the same proxy and
/// the same certificate trust as everything else the launcher fetches.
pub struct CollabCraft {
    http: Http,
}

impl CollabCraft {
    pub fn new(http: Http) -> CollabCraft {
        CollabCraft { http }
    }

    fn url(base: &str, path: &str) -> String {
        format!("{}{path}", base.trim_end_matches('/'))
    }

    /// The `code` field of the deployment's error body. That is how the deployment distinguishes
    /// a state from a failure.
    fn code(body: &str) -> Option<String> {
        let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
        parsed.get("code").and_then(|v| v.as_str()).map(str::to_owned)
    }

    fn refused(status: u16, body: &str) -> String {
        match CollabCraft::code(body) {
            Some(code) => format!("the service refused the request ({code})"),
            None => format!("the service answered HTTP {status}"),
        }
    }
}

impl Service for CollabCraft {
    fn start(&self, base: &str) -> Result<Flow, String> {
        let (status, body) = self.http.post_json(&CollabCraft::url(base, "/auth/device/start"), "null", None)?;
        if status != 200 {
            return Err(CollabCraft::refused(status, &body));
        }
        #[derive(serde::Deserialize)]
        struct Started {
            device_code: String,
            verify_url: String,
            expires_in: u64,
            interval: u64,
        }
        let started: Started = serde_json::from_str(&body).map_err(|e| format!("the service's answer didn't parse ({e})"))?;
        Ok(Flow { device_code: started.device_code, verify_url: started.verify_url, expires_in: started.expires_in, interval: started.interval })
    }

    fn poll(&self, base: &str, device_code: &str) -> Result<Polled, String> {
        // The device code is a one-time handle the deployment issued. It still only ever goes in a
        // request body.
        let body = serde_json::json!({ "device_code": device_code }).to_string();
        let (status, answer) = self.http.post_json(&CollabCraft::url(base, "/auth/device/poll"), &body, None)?;
        // A 200 answer carries the bearer token in plain JSON. Held as `Zeroizing` rather than a
        // bare `String` so that plaintext is overwritten once it is parsed out, the same as
        // `Secret` does for the token after that.
        let answer = Zeroizing::new(answer);
        match status {
            200 => {
                #[derive(serde::Deserialize)]
                struct Token {
                    token: String,
                }
                let token: Token = serde_json::from_str(&answer).map_err(|e| format!("the service's answer didn't parse ({e})"))?;
                Ok(Polled::Done(Secret::new(token.token)))
            }
            // Still waiting on the browser. An ordinary state to loop on.
            428 => Ok(Polled::Pending),
            404 => Ok(Polled::Expired),
            s => Err(CollabCraft::refused(s, &answer)),
        }
    }

    fn identity(&self, base: &str, token: &Secret) -> Result<Option<Identity>, String> {
        let (status, body) = self.http.get_json(&CollabCraft::url(base, "/api/v1/me"), Some(token.expose()))?;
        match status {
            200 => {
                #[derive(serde::Deserialize)]
                struct ConversationHomeWire {
                    name: String,
                    url: String,
                }
                #[derive(serde::Deserialize)]
                struct Me {
                    id: String,
                    display: String,
                    email: Option<String>,
                    provider_linked: bool,
                    #[serde(default)]
                    conversation: Option<ConversationHomeWire>,
                }
                let me: Me = serde_json::from_str(&body).map_err(|e| format!("the service's answer didn't parse ({e})"))?;
                Ok(Some(Identity {
                    user: me.id,
                    display: me.display,
                    email: me.email,
                    linked: me.provider_linked,
                    conversation: me.conversation.map(|c| ConversationHome { name: c.name, url: c.url }),
                }))
            }
            // The deployment no longer honours the token. Nobody is signed in.
            401 | 403 => Ok(None),
            s => Err(CollabCraft::refused(s, &body)),
        }
    }

    fn revoke(&self, base: &str, token: &Secret) -> Result<(), String> {
        let (status, body) = self.http.post_json(&CollabCraft::url(base, "/auth/logout"), "null", Some(token.expose()))?;
        match status {
            // Already gone counts as ended.
            200 | 204 | 401 | 403 => Ok(()),
            s => Err(CollabCraft::refused(s, &body)),
        }
    }
}

/// The launcher's view of the suite sign-in: the store, the deployment, one worker at a time.
pub struct Accounts {
    store: Arc<Store>,
    service: Arc<dyn Service + Send + Sync>,
    /// The configured deployment. Empty means none.
    base: Mutex<String>,
    /// A sign-in or sign-out is in flight. A second request is then ignored. Nothing queues.
    busy: Arc<AtomicBool>,
    /// The last address somebody typed was refused. A sign-in then has nothing it may use: the
    /// address still held is the one before it, and a flow against that one would sign the person
    /// in somewhere they did not ask for.
    refused: Arc<AtomicBool>,
    /// Counts the changes of what this window's session is about: the configured deployment, a
    /// sign-in, a sign-out. A worker reads it when it starts and again before it acts. A worker
    /// holding a stale number is answering about a session that has been replaced, so its answer
    /// is dropped rather than applied to the one that took its place.
    generation: Arc<AtomicU64>,
    emit: Arc<dyn Fn(SuiteAccount) + Send + Sync>,
}

/// What the configured deployment address has to look like. Stated once, because a refused
/// address is reported when it is typed and again when a sign-in is asked for afterwards.
const BASE_RULE: &str = "the suite service must be https:// or http:// on 127.0.0.1, localhost, or [::1] with a port";

/// How often to look again while waiting for a browser, whatever the deployment asked for.
const MIN_POLL: u64 = 1;
const MAX_POLL: u64 = 10;
/// Do not poll longer than a day even if the deployment asks for more.
const MAX_FLOW_SECS: u64 = 86_400;

impl Accounts {
    /// `emit` is called from any thread with each new state.
    pub fn new(http: Http, emit: impl Fn(SuiteAccount) + Send + Sync + 'static) -> Accounts {
        Accounts {
            store: Arc::new(Store::open()),
            service: Arc::new(CollabCraft::new(http)),
            base: Mutex::new(String::new()),
            busy: Arc::new(AtomicBool::new(false)),
            refused: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            emit: Arc::new(emit),
        }
    }

    fn base(&self) -> String {
        self.base.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Points the launcher at a deployment. A change re-checks who is signed in.
    pub fn set_base(&self, base: &str) {
        let base = base.trim().trim_end_matches('/').to_owned();
        let mut held = self.base.lock().unwrap_or_else(PoisonError::into_inner);
        if *held == base {
            return;
        }
        if !service_base::allowed_service_base(&base) {
            self.refused.store(true, Ordering::SeqCst);
            (self.emit)(SuiteAccount::Failed(BASE_RULE.to_owned()));
            return;
        }
        self.refused.store(false, Ordering::SeqCst);
        *held = base;
        drop(held);
        // A sign-in still waiting on a browser was started against the deployment configured
        // then. It must not store a session for that one, nor report one.
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.refresh();
    }

    /// Says who is signed in, asking the deployment whether the session is still good.
    ///
    /// Does nothing while a sign-in is in flight. The store holds no account yet at that point.
    /// Answering would report a sign-out over the top of a browser flow that is still live.
    pub fn refresh(&self) {
        let base = self.base();
        if base.is_empty() {
            (self.emit)(SuiteAccount::Unconfigured);
            return;
        }
        if self.busy.load(Ordering::SeqCst) {
            return;
        }
        let (store, service, emit) = (self.store.clone(), self.service.clone(), self.emit.clone());
        let (generation, issued) = (self.generation.clone(), self.generation.load(Ordering::SeqCst));
        std::thread::spawn(move || {
            let state = state_now(&store, service.as_ref());
            // A sign-out or a sign-in that landed while this was asking has already said what is
            // true. This answer is about the session before it.
            if generation.load(Ordering::SeqCst) != issued {
                return;
            }
            emit(state);
        });
    }

    /// Starts a sign-in. The user finishes it in a browser.
    pub fn sign_in(&self) {
        let base = self.base();
        if base.is_empty() {
            (self.emit)(SuiteAccount::Unconfigured);
            return;
        }
        // The address typed before this was refused, so it was never configured. Starting a flow
        // now would use the address before it, which is not what was asked for.
        if self.refused.load(Ordering::SeqCst) {
            (self.emit)(SuiteAccount::Failed(BASE_RULE.to_owned()));
            return;
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (store, service, emit, busy) = (self.store.clone(), self.service.clone(), self.emit.clone(), self.busy.clone());
        let (generation, issued) = (self.generation.clone(), self.generation.load(Ordering::SeqCst));
        std::thread::spawn(move || {
            let mut guard = BusyGuard::new(busy);
            let state = run_sign_in(&store, service.as_ref(), &base, emit.as_ref(), &generation, issued);
            guard.clear();
            emit(settle(&store, service.as_ref(), state, &generation, issued));
        });
    }

    /// Ends the suite session: at the deployment first, then in the keychain.
    pub fn sign_out(&self) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (store, service, emit, busy) = (self.store.clone(), self.service.clone(), self.emit.clone(), self.busy.clone());
        let generation = self.generation.clone();
        // A sign-out is the end of this window's session whatever else was in flight, so the
        // number moves before the work starts: every worker older than this is now stale.
        generation.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            let mut guard = BusyGuard::new(busy);
            let current = store.accounts().map(|list| list.current().map(|a| a.id.clone()));
            let states: Vec<SuiteAccount> = match current {
                Ok(Some(id)) => match suite_service::sign_out(&store, service.as_ref(), &id) {
                    Ok(SignedOut::Everywhere) => vec![SuiteAccount::SignedOut],
                    // The local half is done either way. The session may still be live at the
                    // deployment. The user therefore needs to hear that it was not told.
                    Ok(SignedOut::LocalOnly { why }) => {
                        vec![SuiteAccount::SignedOut, SuiteAccount::Failed(format!("Signed out here. The service could not be told ({why})."))]
                    }
                    Err(e) => vec![SuiteAccount::Failed(e.to_string())],
                },
                Ok(None) => vec![SuiteAccount::SignedOut],
                Err(e) => vec![SuiteAccount::Failed(e.to_string())],
            };
            guard.clear();
            for state in states {
                emit(state);
            }
        });
    }
}

/// A sign-in, start to finish. It blocks. It therefore runs on a worker.
fn run_sign_in<S: TokenStore>(
    store: &Store<S>,
    service: &(dyn Service + Send + Sync),
    base: &str,
    emit: &(dyn Fn(SuiteAccount) + Send + Sync),
    generation: &AtomicU64,
    issued: u64,
) -> SuiteAccount {
    // Refuse before anything is minted. A machine with no keychain then never ends up holding a
    // session it cannot keep.
    if let Err(e) = store.usable() {
        return SuiteAccount::Failed(e.to_string());
    }
    let flow = match service.start(base) {
        Ok(f) => f,
        Err(why) => return SuiteAccount::Failed(why),
    };
    // The page has to be reported before the waiting starts. The UI opens the browser. Nothing
    // happens at the deployment until the user has been there.
    emit(SuiteAccount::Waiting { verify_url: flow.verify_url.clone() });
    // The deadline is the deployment's, capped so a bad value cannot overflow time math.
    let secs = flow.expires_in.min(MAX_FLOW_SECS);
    let deadline = Instant::now().checked_add(Duration::from_secs(secs)).unwrap_or_else(|| Instant::now() + Duration::from_secs(MAX_FLOW_SECS));
    let gap = Duration::from_secs(flow.interval.clamp(MIN_POLL, MAX_POLL));
    while Instant::now() < deadline {
        std::thread::sleep(gap);
        match service.poll(base, &flow.device_code) {
            Ok(Polled::Pending) => {}
            Ok(Polled::Expired) => return SuiteAccount::Failed("that sign-in expired".to_owned()),
            Ok(Polled::Done(token)) => {
                // The deployment this flow started against may no longer be the configured one.
                // Storing the session would sign the person in to the address they just left. The
                // token is not revoked here, for the reason given on `keep`.
                if generation.load(Ordering::SeqCst) != issued {
                    log::info!("a finished sign-in was dropped: the configured service changed while it waited");
                    return SuiteAccount::Failed("the suite service changed while that sign-in was waiting".to_owned());
                }
                return keep(store, service, base, &token);
            }
            Err(why) => return SuiteAccount::Failed(why),
        }
    }
    SuiteAccount::Failed("the sign-in timed out".to_owned())
}

/// Stores a collected session, after asking the deployment who it belongs to.
///
/// A session whose owner cannot be read is not kept: an account with no subject behind it is
/// worse than no account.
///
/// It is not given back either, and that is deliberate. [`Service::revoke`] is the deployment's
/// logout, and the deployment drops every session it holds for that user, on every machine. Using
/// it to tidy up one failed sign-in would sign the person out of the laptop they are sitting at.
/// The contract offers no way to end one session, so the token is left to expire where it was
/// minted and the gap is logged. `revoke` stays for the sign-out somebody asked for.
fn keep<S: TokenStore>(store: &Store<S>, service: &(dyn Service + Send + Sync), base: &str, token: &Secret) -> SuiteAccount {
    let who = match service.identity(base, token) {
        Ok(Some(who)) => who,
        Ok(None) => return SuiteAccount::Failed("the service did not accept the new session".to_owned()),
        Err(why) => {
            // Not revoked: that call is the deployment's global logout (see above). This token is
            // never stored, so nothing here can use it, and it expires at the deployment.
            log::warn!("a session was minted and then abandoned, since its owner could not be read: {why}");
            return SuiteAccount::Failed(why);
        }
    };
    let new =
        NewAccount { service: base.to_owned(), user: who.user.clone(), display: who.display.clone(), email: who.email.clone(), linked: who.linked };
    match store.sign_in(&new, token, crate::now()) {
        Ok(account) => signed_in(&account, Some(&who)),
        Err(e) => {
            // Not revoked, for the same reason: a failed store must not end this person's sessions
            // everywhere else.
            log::warn!("a session was minted and then abandoned, since it could not be stored: {e}");
            SuiteAccount::Failed(e.to_string())
        }
    }
}

/// What the store says about the session now, with the deployment's own answer where it gives one.
///
/// Read by [`Accounts::refresh`], and by a sign-in whose answer arrived for a session that has
/// since been replaced. In both cases the question is what is true at this moment, never what was
/// true when the work started.
fn state_now<S: TokenStore>(store: &Store<S>, service: &(dyn Service + Send + Sync)) -> SuiteAccount {
    match store.shared() {
        Shared::None => SuiteAccount::SignedOut,
        Shared::Unreadable { account, why } => SuiteAccount::Unreadable { display: account.display, why },
        Shared::Session { account, token } => {
            if let Err(why) = service_base::validate_account_target(&account) {
                log::warn!("a stored suite account did not pass target validation: {why}");
                return SuiteAccount::Unreadable { display: account.display, why };
            }
            match service.identity(&account.service, &token) {
                Ok(Some(who)) => {
                    if let Err(e) = store.touch(&account.id, crate::now()) {
                        log::warn!("couldn't record the account's last use: {e}");
                    }
                    signed_in(&account, Some(&who))
                }
                // The deployment has forgotten the session. This machine should too.
                Ok(None) => {
                    if let Err(e) = store.discard(&account.id) {
                        log::warn!("couldn't clear the stale suite session: {e}");
                    }
                    SuiteAccount::SignedOut
                }
                // Offline, or the deployment is down. Still signed in; the storage state is simply
                // unknown until it answers.
                Err(why) => {
                    log::info!("the suite session could not be checked: {why}");
                    signed_in(&account, None)
                }
            }
        }
    }
}

/// What a finished sign-in should report.
///
/// Its own result, when this window's session is still the one it was started for. The number then
/// moves, because that session is now whatever the sign-in made of it.
///
/// Where the number has already moved, the result is about a session that has been replaced, so it
/// is dropped. What goes out instead is the state as it stands, read afresh. Saying nothing would
/// leave the window on the last thing the flow reported, which is `Waiting`: a screen that offers
/// no sign-in, no sign-out and no way back.
fn settle<S: TokenStore>(
    store: &Store<S>,
    service: &(dyn Service + Send + Sync),
    state: SuiteAccount,
    generation: &AtomicU64,
    issued: u64,
) -> SuiteAccount {
    if generation.load(Ordering::SeqCst) != issued {
        log::info!("a finished sign-in was dropped: this window's session changed while it waited");
        return state_now(store, service);
    }
    generation.fetch_add(1, Ordering::SeqCst);
    state
}

/// The state for an account, with the deployment's own answer where there is one.
fn signed_in(account: &Account, who: Option<&Identity>) -> SuiteAccount {
    SuiteAccount::SignedIn(SuiteIdentity {
        display: who.map_or_else(|| account.display.clone(), |w| w.display.clone()),
        email: who.map_or_else(|| account.email.clone(), |w| w.email.clone()),
        service: account.service.clone(),
        linked: who.map(|w| w.linked),
        conversation: who.and_then(|w| {
            w.conversation.as_ref().map(|c| artcraft_launcher_core::protocol::ConversationHome { name: c.name.clone(), url: c.url.clone() })
        }),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use craft_suite_account::{Memory, Store};

    use super::*;

    /// A deployment that hands out a session at once, and counts what it was asked to revoke.
    struct Immediate {
        /// Bumped by `poll`, so a test can move the generation while a flow is in flight.
        polled: AtomicUsize,
        revoked: AtomicUsize,
        identity: Option<Identity>,
    }

    impl Immediate {
        fn new(identity: Option<Identity>) -> Immediate {
            Immediate { polled: AtomicUsize::new(0), revoked: AtomicUsize::new(0), identity }
        }
    }

    impl Service for Immediate {
        fn start(&self, _base: &str) -> Result<Flow, String> {
            Ok(Flow { device_code: "device".to_owned(), verify_url: "https://example.test/device".to_owned(), expires_in: 60, interval: 0 })
        }

        fn poll(&self, _base: &str, _device_code: &str) -> Result<Polled, String> {
            self.polled.fetch_add(1, Ordering::SeqCst);
            Ok(Polled::Done(Secret::new("a-session-token")))
        }

        fn identity(&self, _base: &str, _token: &Secret) -> Result<Option<Identity>, String> {
            match &self.identity {
                Some(who) => Ok(Some(who.clone())),
                None => Err("the deployment could not say who holds it".to_owned()),
            }
        }

        fn revoke(&self, _base: &str, _token: &Secret) -> Result<(), String> {
            self.revoked.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn store(dir: &std::path::Path) -> Store<Memory> {
        Store::at(Some(dir.to_path_buf()), Memory::default())
    }

    /// Changing the configured deployment while a browser flow is still open must not store the
    /// session the old deployment then hands over, and must not report a sign-in for it. Without
    /// the generation check, `keep` stores it and the window says SignedIn for the address the
    /// person just left.
    #[test]
    fn a_sign_in_that_finishes_after_the_service_changed_is_dropped() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let store = store(dir.path());
        let service = Immediate::new(Some(Identity { user: "u".to_owned(), display: "Someone".to_owned(), ..Identity::default() }));
        let generation = AtomicU64::new(7);
        // The flow was issued at 7; the configured address changed while the browser was open.
        generation.store(8, Ordering::SeqCst);

        let state = run_sign_in(&store, &service, "https://old.example.test", &|_| {}, &generation, 7);

        assert!(matches!(state, SuiteAccount::Failed(_)), "{state:?}");
        assert!(store.accounts().expect("the account list").current().is_none(), "no account is stored for the address that was left");
    }

    /// A sign-in whose answer arrives for a session that has been replaced must still report
    /// something. The window's last word from the flow is `Waiting`, a screen with no sign-in, no
    /// sign-out and no way back, so saying nothing strands whoever is looking at it.
    #[test]
    fn a_dropped_sign_in_reports_the_state_as_it_stands() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let store = store(dir.path());
        let service = Immediate::new(None);
        let generation = AtomicU64::new(9);

        let reported = settle(&store, &service, SuiteAccount::Waiting { verify_url: "https://example.test/device".to_owned() }, &generation, 8);

        assert!(matches!(reported, SuiteAccount::SignedOut), "{reported:?}");
        assert_eq!(generation.load(Ordering::SeqCst), 9, "a dropped answer does not move the session on");
    }

    /// Its own answer goes out when the session is still the one it was started for, and the
    /// number moves, because that session is now whatever the sign-in made of it.
    #[test]
    fn a_sign_in_for_the_session_it_started_for_reports_its_own_answer() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let store = store(dir.path());
        let service = Immediate::new(None);
        let generation = AtomicU64::new(4);

        let reported = settle(&store, &service, SuiteAccount::Failed("that sign-in expired".to_owned()), &generation, 4);

        assert!(matches!(reported, SuiteAccount::Failed(_)), "{reported:?}");
        assert_eq!(generation.load(Ordering::SeqCst), 5);
    }

    /// A sign-in whose owner cannot be read is abandoned, and the token is left alone. Revoking it
    /// would be the deployment's global logout, which ends the person's sessions on every machine
    /// they are signed in on, including the one in front of them.
    #[test]
    fn an_unreadable_owner_abandons_the_session_without_a_global_logout() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let store = store(dir.path());
        let service = Immediate::new(None);

        let state = keep(&store, &service, "https://example.test", &Secret::new("a-session-token"));

        assert!(matches!(state, SuiteAccount::Failed(_)), "{state:?}");
        assert_eq!(service.revoked.load(Ordering::SeqCst), 0, "a failed sign-in must not log the person out everywhere");
        assert!(store.accounts().expect("the account list").current().is_none());
    }

    /// A deployment stand-in whose only job is to count how many times it was asked who a token
    /// belongs to. The security property under test is that it is asked zero times.
    #[derive(Default)]
    struct Counting {
        identity_calls: AtomicUsize,
    }

    impl Service for Counting {
        fn start(&self, _base: &str) -> Result<Flow, String> {
            Err("not used by this test".to_owned())
        }

        fn poll(&self, _base: &str, _device_code: &str) -> Result<Polled, String> {
            Err("not used by this test".to_owned())
        }

        fn identity(&self, _base: &str, _token: &Secret) -> Result<Option<Identity>, String> {
            self.identity_calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        fn revoke(&self, _base: &str, _token: &Secret) -> Result<(), String> {
            Err("not used by this test".to_owned())
        }
    }

    /// Writes `account` as the store's one, active row, with `token` behind it in the keychain
    /// stand-in. Bypasses `Store::sign_in`, which always writes a row whose `id` is the correct
    /// handle for its own `service` and `user`: the two attacks under test, a redirected address
    /// and an edited-in-place `service`, both need a row `sign_in` would never produce.
    fn seed(store: &Store<Memory>, tokens: &Memory, account: Account, token: &Secret) {
        let path = store.path().expect("a store built with a directory has a path");
        let mut list = craft_suite_account::Accounts { active: Some(account.id.clone()), ..craft_suite_account::Accounts::default() };
        list.accounts.push(account.clone());
        craft_suite_account::accounts::write(&path, &list).expect("writing the seeded account list");
        tokens.put(&account.id, token).expect("seeding the token");
    }

    /// A row whose `service` is a plain, non-loopback `http://` address, the shape a migrated
    /// legacy session file or another application's own session could hand this launcher, must
    /// never have its token presented to that address. Without the fix, `state_now` sends it
    /// straight to `identity`.
    #[test]
    fn a_non_loopback_http_service_sends_no_token() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let tokens = Memory::new();
        let store = Store::at(Some(dir.path().to_path_buf()), tokens.clone());
        let service = Counting::default();
        let account = Account {
            id: craft_suite_account::accounts::handle("http://collab.lan:8080", "u"),
            service: "http://collab.lan:8080".to_owned(),
            user: "u".to_owned(),
            display: "Someone".to_owned(),
            ..Account::default()
        };
        seed(&store, &tokens, account, &Secret::new("a-session-token"));

        let state = state_now(&store, &service);

        assert!(matches!(state, SuiteAccount::Unreadable { .. }), "{state:?}");
        assert_eq!(service.identity_calls.load(Ordering::SeqCst), 0, "the token must never reach a service address that failed validation");
    }

    /// A row whose `service` was edited in place while its `id` was left as it was (so the
    /// handle no longer matches what `accounts::handle(service, user)` computes) points the next
    /// refresh at a different deployment than the one that minted the session. This must be
    /// refused the same way a bad address is, not waved through because the address alone looks
    /// allowed.
    #[test]
    fn an_edited_service_that_kept_the_old_id_sends_no_token() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let tokens = Memory::new();
        let store = Store::at(Some(dir.path().to_path_buf()), tokens.clone());
        let service = Counting::default();
        // The id a sign-in would have minted for the original, honest service. Kept as-is while
        // `service` below is swapped for a different, equally well-formed address.
        let original_id = craft_suite_account::accounts::handle("https://good.example.test", "u");
        let account = Account {
            id: original_id,
            service: "https://evil.example.test".to_owned(),
            user: "u".to_owned(),
            display: "Someone".to_owned(),
            ..Account::default()
        };
        seed(&store, &tokens, account, &Secret::new("a-session-token"));

        let state = state_now(&store, &service);

        assert!(matches!(state, SuiteAccount::Unreadable { .. }), "{state:?}");
        assert_eq!(service.identity_calls.load(Ordering::SeqCst), 0, "a service edited past its own handle must not receive the token either");
    }
}
