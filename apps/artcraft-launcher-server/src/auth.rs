use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, Request, Response, header};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AccessTokenHash, AsyncHttpClient, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet, EndpointSet, IssuerUrl,
    Nonce, OAuth2TokenResponse as _, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse as _,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use url::Url;
use uuid::Uuid;

use crate::config::{AuthSettings, bounded_text, read_secret};
use crate::https::Https;

pub(crate) const SESSION_COOKIE: &str = "__Host-artcraft_session";
pub(crate) const LOGIN_COOKIE: &str = "__Host-artcraft_login";
pub(crate) const CALLBACK: &str = "/api/server/v1/auth/callback";

const LOGIN_TTL: Duration = Duration::from_secs(300);
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_LOGINS: usize = 256;
const MAX_SESSIONS: usize = 1024;
const MAX_OIDC_RESPONSE: usize = 1024 * 1024;

type DiscoveredClient = CoreClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointMaybeSet, EndpointMaybeSet>;

pub(crate) struct Auth {
    provider: DiscoveredClient,
    http: Https,
    settings: AuthSettings,
    logins: Mutex<HashMap<String, Login>>,
    sessions: Mutex<HashMap<String, SessionEntry>>,
}

struct Login {
    expires: Instant,
    csrf: CsrfToken,
    nonce: Nonce,
    verifier: PkceCodeVerifier,
    return_path: String,
}

struct SessionEntry {
    session: Session,
    last_active: Instant,
    expires: Instant,
}

#[derive(Clone, Serialize)]
pub(crate) struct Session {
    #[serde(rename = "namespace")]
    pub(crate) account: String,
    pub(crate) label: String,
    pub(crate) admin: bool,
    pub(crate) csrf: String,
}

pub(crate) struct CallbackQuery {
    pub(crate) code: Option<String>,
    pub(crate) state: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthError {
    Denied,
    Invalid,
    Unavailable,
}

impl Auth {
    pub(crate) async fn initialize(origin: &Url, settings: AuthSettings) -> Result<Self, String> {
        let secret = read_secret(&settings.client_secret_file)?;
        let http = Https::new(Duration::from_secs(15));
        let issuer = IssuerUrl::new(settings.issuer.to_string()).map_err(|_| String::from("Invalid OIDC issuer"))?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &http).await.map_err(|_| String::from("OIDC discovery failed"))?;
        if metadata.authorization_endpoint().url().scheme() != "https" || !metadata.token_endpoint().is_some_and(|url| url.url().scheme() == "https")
        {
            return Err(String::from("The OIDC endpoints must use HTTPS"));
        }
        let callback = origin.join(CALLBACK).map_err(|_| String::from("Invalid OIDC redirect origin"))?;
        let redirect = RedirectUrl::new(callback.to_string()).map_err(|_| String::from("Invalid OIDC redirect URL"))?;
        let provider = CoreClient::from_provider_metadata(metadata, ClientId::new(settings.client_id.clone()), Some(ClientSecret::new(secret)))
            .set_redirect_uri(redirect);
        Ok(Self { provider, http, settings, logins: Mutex::new(HashMap::new()), sessions: Mutex::new(HashMap::new()) })
    }

    pub(crate) fn begin(&self, previous: Option<&str>, return_path: String) -> Result<(Url, String), AuthError> {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf, nonce) = self
            .provider
            .authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random)
            .add_scope(Scope::new(String::from("email")))
            .set_pkce_challenge(challenge)
            .url();
        let now = Instant::now();
        let mut logins = self.logins.lock().map_err(|_| AuthError::Unavailable)?;
        logins.retain(|_, entry| entry.expires > now);
        if let Some(previous) = previous {
            logins.remove(previous);
        }
        if logins.len() >= MAX_LOGINS {
            return Err(AuthError::Unavailable);
        }
        let token = opaque_token();
        logins.insert(token.clone(), Login { expires: now + LOGIN_TTL, csrf, nonce, verifier, return_path });
        Ok((url, cookie(LOGIN_COOKIE, &token, LOGIN_TTL.as_secs())))
    }

    pub(crate) async fn complete(&self, binding: &str, previous: Option<&str>, query: CallbackQuery) -> Result<(String, String), AuthError> {
        let code = query.code.filter(|code| bounded_text(code, 4096));
        let state = query.state.filter(|state| bounded_text(state, 256));
        let (Some(code), Some(state)) = (code, state) else {
            return Err(AuthError::Invalid);
        };
        let login = self.take_login(binding, &state)?;
        let return_path = login.return_path.clone();
        let (subject, label) = self.verify_identity(code, login).await?;
        if !self.settings.allows(&subject) {
            return Err(AuthError::Denied);
        }
        let session =
            Session { account: account_id(&self.settings.issuer, &subject), label, admin: self.settings.is_admin(&subject), csrf: opaque_token() };
        let token = self.create_session(session, previous)?;
        Ok((cookie(SESSION_COOKIE, &token, SESSION_TTL.as_secs()), return_path))
    }

    fn take_login(&self, binding: &str, state: &str) -> Result<Login, AuthError> {
        let mut logins = self.logins.lock().map_err(|_| AuthError::Unavailable)?;
        let now = Instant::now();
        logins.retain(|_, entry| entry.expires > now);
        let entry = logins.get(binding).ok_or(AuthError::Denied)?;
        if !equal_secret(entry.csrf.secret(), state) {
            return Err(AuthError::Denied);
        }
        logins.remove(binding).ok_or(AuthError::Denied)
    }

    async fn verify_identity(&self, code: String, login: Login) -> Result<(String, String), AuthError> {
        let token = self
            .provider
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|_| AuthError::Unavailable)?
            .set_pkce_verifier(login.verifier)
            .request_async(&self.http)
            .await
            .map_err(|_| AuthError::Denied)?;
        let id = token.id_token().ok_or(AuthError::Denied)?;
        let verifier = self.provider.id_token_verifier();
        let claims = id.claims(&verifier, &login.nonce).map_err(|_| AuthError::Denied)?;
        if let Some(hash) = claims.access_token_hash() {
            let actual = AccessTokenHash::from_token(
                token.access_token(),
                id.signing_alg().map_err(|_| AuthError::Denied)?,
                id.signing_key(&verifier).map_err(|_| AuthError::Denied)?,
            )
            .map_err(|_| AuthError::Denied)?;
            if &actual != hash {
                return Err(AuthError::Denied);
            }
        }
        let subject = claims.subject().as_str();
        if !bounded_text(subject, 255) {
            return Err(AuthError::Denied);
        }
        let label = claims
            .email()
            .filter(|email| claims.email_verified() == Some(true) && bounded_text(email.as_str(), 254))
            .map_or_else(String::new, |email| email.as_str().to_owned());
        Ok((subject.to_owned(), label))
    }

    fn create_session(&self, session: Session, previous: Option<&str>) -> Result<String, AuthError> {
        let now = Instant::now();
        let mut sessions = self.sessions.lock().map_err(|_| AuthError::Unavailable)?;
        sessions.retain(|_, entry| entry.live(now, self.settings.idle_timeout()));
        if let Some(previous) = previous {
            sessions.remove(previous);
        }
        if sessions.len() >= MAX_SESSIONS {
            return Err(AuthError::Unavailable);
        }
        let token = opaque_token();
        sessions.insert(token.clone(), SessionEntry { session, last_active: now, expires: now + SESSION_TTL });
        Ok(token)
    }

    pub(crate) fn session(&self, token: &str, activity: Option<&str>) -> Result<Session, AuthError> {
        let mut sessions = self.sessions.lock().map_err(|_| AuthError::Unavailable)?;
        let now = Instant::now();
        let entry = sessions.get_mut(token).filter(|entry| entry.live(now, self.settings.idle_timeout())).ok_or(AuthError::Denied)?;
        if let Some(csrf) = activity {
            if !equal_secret(&entry.session.csrf, csrf) {
                return Err(AuthError::Denied);
            }
            entry.last_active = now;
        }
        Ok(entry.session.clone())
    }

    pub(crate) fn logout(&self, token: &str) -> Result<(), AuthError> {
        self.sessions.lock().map_err(|_| AuthError::Unavailable)?.remove(token);
        Ok(())
    }
}

impl SessionEntry {
    fn live(&self, now: Instant, idle: Duration) -> bool {
        now < self.expires && now.duration_since(self.last_active) < idle
    }
}

pub(crate) fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut token = None;
    for line in headers.get_all(header::COOKIE) {
        for field in line.to_str().ok()?.split(';') {
            if let Some((key, value)) = field.trim().split_once('=')
                && key == name
            {
                if token.is_some() || !valid_token(value) {
                    return None;
                }
                token = Some(value.to_owned());
            }
        }
    }
    token
}

fn valid_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn clear_cookie(name: &str) -> String {
    cookie(name, "", 0)
}

fn cookie(name: &str, value: &str, max_age: u64) -> String {
    format!("{name}={value}; Max-Age={max_age}; Path=/; Secure; HttpOnly; SameSite=Lax")
}

fn opaque_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn equal_secret(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

fn account_id(issuer: &Url, subject: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"artcraft-server-account-v1\0");
    digest.update(issuer.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(subject.as_bytes());
    format!("{:x}", digest.finalize())
}

impl<'a> AsyncHttpClient<'a> for Https {
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Vec<u8>>, io::Error>> + Send + 'a>>;

    fn call(&'a self, request: Request<Vec<u8>>) -> Self::Future {
        Box::pin(self.send(request, MAX_OIDC_RESPONSE))
    }
}

#[cfg(test)]
mod tests {
    use super::{SESSION_COOKIE, Session, SessionEntry, account_id, clear_cookie, opaque_token, read_cookie};
    use axum::http::{HeaderMap, HeaderValue, header};
    use std::time::{Duration, Instant};
    use url::Url;

    #[test]
    fn cookie_requires_one_well_formed_session_token() {
        let token = opaque_token();
        let mut headers = HeaderMap::new();
        let valid = HeaderValue::from_str(&format!("theme=dark; {SESSION_COOKIE}={token}")).expect("test header");
        headers.insert(header::COOKIE, valid.clone());
        assert_eq!(read_cookie(&headers, SESSION_COOKIE), Some(token));
        headers.append(header::COOKIE, valid);
        assert_eq!(read_cookie(&headers, SESSION_COOKIE), None);
        assert!(clear_cookie(SESSION_COOKIE).contains("Max-Age=0; Path=/; Secure; HttpOnly"));
    }

    #[test]
    fn account_identity_depends_on_issuer_and_subject() {
        let a = Url::parse("https://a.example").expect("test issuer");
        let b = Url::parse("https://b.example").expect("test issuer");
        assert_ne!(account_id(&a, "user"), account_id(&b, "user"));
        assert_ne!(account_id(&a, "user"), account_id(&a, "other"));
        assert_eq!(account_id(&a, "user"), account_id(&a, "user"));
    }

    #[test]
    fn activity_renews_idle_expiry_but_not_absolute_expiry() {
        let now = Instant::now();
        let idle = Duration::from_secs(60);
        let mut entry = SessionEntry {
            session: Session { account: String::new(), label: String::new(), admin: false, csrf: String::new() },
            last_active: now,
            expires: now + idle * 2,
        };
        assert!(entry.live(now + idle / 2, idle));
        assert!(!entry.live(now + idle, idle));
        entry.last_active = now + idle / 2;
        assert!(entry.live(now + idle, idle));
        entry.last_active = now + idle * 2;
        assert!(!entry.live(entry.last_active, idle));
    }
}
