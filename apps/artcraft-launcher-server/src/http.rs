mod release_routes;

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream;
use serde::Deserialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower_http::services::ServeDir;

use crate::auth::{Auth, AuthError, CallbackQuery, LOGIN_COOKIE, SESSION_COOKIE, Session, clear_cookie, read_cookie};
use crate::config::Settings;
use crate::releases::ReleaseService;
use crate::store::{Project, Save, Store, StoreError, StoredFile};

const MAX_UPLOAD_BYTES: usize = 80 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 4096;
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(90);
const TRANSFER_CHUNK: usize = 64 * 1024;
const MAX_TRANSFERS: usize = 4;

#[derive(Clone)]
pub(crate) struct App {
    pub(crate) auth: Arc<Auth>,
    pub(crate) origin: String,
    store: Store,
    releases: ReleaseService,
    worker_secret: Arc<str>,
    transfers: Arc<Semaphore>,
}

impl App {
    pub(crate) fn new(settings: &Settings, auth: Auth, store: Store, releases: ReleaseService, worker_secret: String) -> Self {
        Self {
            auth: Arc::new(auth),
            origin: settings.origin.origin().ascii_serialization(),
            store,
            releases,
            worker_secret: Arc::from(worker_secret),
            transfers: Arc::new(Semaphore::new(MAX_TRANSFERS)),
        }
    }

    pub(crate) fn authenticate(&self, headers: &HeaderMap) -> Result<Session, Error> {
        self.auth.session(&session_id(headers)?, None).map_err(Error::from)
    }

    pub(crate) fn authorize_change(&self, headers: &HeaderMap) -> Result<Session, Error> {
        if one_header(headers, header::ORIGIN.as_str()) != Some(self.origin.as_str()) {
            return Err(Error::forbidden());
        }
        let csrf = one_header(headers, "x-artcraft-csrf").ok_or_else(Error::forbidden)?;
        self.auth.session(&session_id(headers)?, Some(csrf)).map_err(Error::from)
    }

    fn transfer(&self) -> Result<OwnedSemaphorePermit, Error> {
        Arc::clone(&self.transfers).try_acquire_owned().map_err(|_| Error::new(StatusCode::SERVICE_UNAVAILABLE, "transfer_busy"))
    }
}

pub(crate) fn router(settings: &Settings, app: App) -> Router {
    let api = Router::new()
        .route("/info", get(info))
        .route("/session", get(session))
        .route("/session/activity", post(activity))
        .route("/session/logout", post(logout))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/files", get(list).post(save))
        .route("/files/{id}/content", get(content))
        .route("/files/{id}/download", get(download))
        .merge(release_routes::api())
        .fallback(|| async { Error::new(StatusCode::NOT_FOUND, "not_found") })
        .layer(from_fn(no_cache));
    Router::new()
        .nest("/api/server/v1", api)
        .merge(release_routes::assets())
        .fallback_service(ServeDir::new(&settings.launcher_root))
        .layer(from_fn(response_headers))
        .with_state(app)
}

async fn response_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("frame-ancestors 'self'; object-src 'none'; base-uri 'self'"));
    response
}

async fn no_cache(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
}

async fn info() -> Json<serde_json::Value> {
    Json(serde_json::json!({"mode": "server", "protocol": 1}))
}

async fn session(State(app): State<App>, headers: HeaderMap) -> Result<Json<Session>, Error> {
    app.authenticate(&headers).map(Json)
}

async fn activity(State(app): State<App>, headers: HeaderMap) -> Result<StatusCode, Error> {
    app.authorize_change(&headers)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> Result<Response, Error> {
    app.authorize_change(&headers)?;
    app.auth.logout(&session_id(&headers)?)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    add_cookie(&mut response, clear_cookie(SESSION_COOKIE))?;
    Ok(response)
}

fn return_path(editor: Option<&str>, project: Option<&str>) -> String {
    let Some(editor) = editor.filter(|id| matches!(*id, "photocraft" | "wordcraft" | "gridcraft" | "cadcraft")) else {
        return String::from("/");
    };
    let mut path = format!("/?editor={editor}");
    if let Some(project) = project.and_then(|id| uuid::Uuid::parse_str(id).ok()) {
        path.push_str("&project=");
        path.push_str(&project.to_string());
    }
    path
}

async fn login(State(app): State<App>, headers: HeaderMap, RawQuery(raw): RawQuery) -> Result<Response, Error> {
    let [editor, project] = query_values(raw.as_deref(), ["editor", "project"], false)?;
    let binding = read_cookie(&headers, LOGIN_COOKIE);
    let (url, cookie) = app.auth.begin(binding.as_deref(), return_path(editor.as_deref(), project.as_deref()))?;
    let mut response = redirect(url.as_str())?;
    add_cookie(&mut response, cookie)?;
    Ok(response)
}

async fn callback(State(app): State<App>, headers: HeaderMap, RawQuery(raw): RawQuery) -> Result<Response, Error> {
    let binding = read_cookie(&headers, LOGIN_COOKIE).ok_or_else(Error::forbidden)?;
    let previous = read_cookie(&headers, SESSION_COOKIE);
    let [code, state] = query_values(raw.as_deref(), ["code", "state"], true)?;
    let query = CallbackQuery { code, state };
    let (cookie, return_path) = app.auth.complete(&binding, previous.as_deref(), query).await?;
    let mut response = redirect(&return_path)?;
    add_cookie(&mut response, clear_cookie(LOGIN_COOKIE))?;
    add_cookie(&mut response, cookie)?;
    Ok(response)
}

fn redirect(to: &str) -> Result<Response, Error> {
    let mut response = StatusCode::SEE_OTHER.into_response();
    let location = HeaderValue::from_str(to).map_err(|_| Error::internal())?;
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

fn add_cookie(response: &mut Response, cookie: String) -> Result<(), Error> {
    let value = HeaderValue::from_str(&cookie).map_err(|_| Error::internal())?;
    response.headers_mut().append(header::SET_COOKIE, value);
    Ok(())
}

fn session_id(headers: &HeaderMap) -> Result<String, Error> {
    read_cookie(headers, SESSION_COOKIE).ok_or_else(Error::unauthorized)
}

pub(crate) fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let all = headers.get_all(name);
    let mut values = all.iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

async fn list(State(app): State<App>, headers: HeaderMap, RawQuery(raw): RawQuery) -> Result<Json<Vec<Project>>, Error> {
    let session = app.authenticate(&headers)?;
    let [editor] = query_values(raw.as_deref(), ["app"], false)?;
    let permit = app.transfer()?;
    blocking(move || {
        let _permit = permit;
        app.store.list(&session.account, editor.as_deref())
    })
    .await
    .map(Json)
}

async fn content(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, RawQuery(raw): RawQuery) -> Result<Response, Error> {
    deliver(app, headers, id, requested_version(raw.as_deref())?, false).await
}

async fn download(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>, RawQuery(raw): RawQuery) -> Result<Response, Error> {
    deliver(app, headers, id, requested_version(raw.as_deref())?, true).await
}

fn requested_version(raw: Option<&str>) -> Result<Option<u64>, Error> {
    let [value] = query_values(raw, ["version"], false)?;
    value.map(|v| v.parse::<u64>().map_err(|_| Error::invalid())).transpose()
}

fn query_values<const N: usize>(raw: Option<&str>, keys: [&str; N], allow_unknown: bool) -> Result<[Option<String>; N], Error> {
    let mut values = std::array::from_fn(|_| None);
    let Some(raw) = raw else { return Ok(values) };
    if raw.len() > 8192 {
        return Err(Error::invalid());
    }
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        let Some(index) = keys.iter().position(|expected| *expected == key) else {
            if allow_unknown {
                continue;
            }
            return Err(Error::invalid());
        };
        let slot = values.get_mut(index).ok_or_else(Error::internal)?;
        if slot.is_some() {
            return Err(Error::invalid());
        }
        *slot = Some(value.into_owned());
    }
    Ok(values)
}

async fn deliver(app: App, headers: HeaderMap, id: String, version: Option<u64>, attachment: bool) -> Result<Response, Error> {
    let session = app.authenticate(&headers)?;
    let permit = app.transfer()?;
    let (file, permit) = blocking(move || app.store.read(&session.account, &id, version).map(|file| (file, permit))).await?;
    download_response(file, permit, attachment)
}

fn download_response(file: StoredFile, permit: OwnedSemaphorePermit, attachment: bool) -> Result<Response, Error> {
    let length = HeaderValue::from_str(&file.bytes.len().to_string()).map_err(|_| Error::internal())?;
    let bytes = Bytes::from(file.bytes);
    let body = Body::from_stream(stream::unfold((bytes, permit), |(mut bytes, permit)| async move {
        if bytes.is_empty() {
            return None;
        }
        let next = bytes.split_to(TRANSFER_CHUNK.min(bytes.len()));
        Some((Ok::<_, std::convert::Infallible>(next), (bytes, permit)))
    }));
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_LENGTH, length);
    if attachment {
        headers.insert(header::CONTENT_DISPOSITION, attachment_header(&file.project.name)?);
    }
    Ok(response)
}

fn attachment_header(name: &str) -> Result<HeaderValue, Error> {
    use std::fmt::Write as _;

    let mut value = String::from("attachment; filename=\"project\"; filename*=UTF-8''");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
            value.push(char::from(byte));
        } else {
            write!(value, "%{byte:02X}").map_err(|_| Error::internal())?;
        }
    }
    HeaderValue::from_str(&value).map_err(|_| Error::internal())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveMetadata {
    app: String,
    name: String,
    operation: String,
    project_id: Option<String>,
    expected_version: Option<u64>,
}

async fn save(State(app): State<App>, headers: HeaderMap, body: Body) -> Result<Json<Project>, Error> {
    let session = app.authorize_change(&headers)?;
    if one_header(&headers, header::CONTENT_TYPE.as_str()) != Some("application/octet-stream") {
        return Err(Error::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "invalid_content_type"));
    }
    let permit = app.transfer()?;
    let limit = MAX_UPLOAD_BYTES + MAX_METADATA_BYTES + 4;
    let body = tokio::time::timeout(UPLOAD_TIMEOUT, to_bytes(body, limit))
        .await
        .map_err(|_| Error::new(StatusCode::REQUEST_TIMEOUT, "upload_timeout"))?
        .map_err(|_| Error::new(StatusCode::PAYLOAD_TOO_LARGE, "invalid_upload_body"))?;
    let (metadata, bytes) = parse_save(&body)?;
    let bytes = body.slice_ref(bytes);
    // Recheck admission after receiving the bytes. An expired or replaced session cannot
    // turn an upload that was still in transit into a newly committed version.
    let current = app.authorize_change(&headers)?;
    if current.account != session.account {
        return Err(Error::forbidden());
    }
    blocking(move || {
        let _permit = permit;
        app.store.save(
            &current.account,
            Save {
                app: &metadata.app,
                name: &metadata.name,
                operation: &metadata.operation,
                project_id: metadata.project_id.as_deref(),
                expected_version: metadata.expected_version,
                bytes: &bytes,
            },
        )
    })
    .await
    .map(Json)
}

fn parse_save(body: &[u8]) -> Result<(SaveMetadata, &[u8]), Error> {
    let (length, remaining) = body.split_first_chunk::<4>().ok_or_else(Error::invalid)?;
    let length = u32::from_be_bytes(*length) as usize;
    if length == 0 || length > MAX_METADATA_BYTES {
        return Err(Error::invalid());
    }
    let (metadata, bytes) = remaining.split_at_checked(length).ok_or_else(Error::invalid)?;
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD_BYTES {
        return Err(Error::invalid());
    }
    let metadata = serde_json::from_slice(metadata).map_err(|_| Error::invalid())?;
    Ok((metadata, bytes))
}

async fn blocking<T: Send + 'static>(task: impl FnOnce() -> Result<T, StoreError> + Send + 'static) -> Result<T, Error> {
    tokio::task::spawn_blocking(task).await.map_err(|_| Error::internal())?.map_err(Error::from)
}

pub(crate) struct Error {
    status: StatusCode,
    code: &'static str,
}

impl Error {
    pub(crate) fn new(status: StatusCode, code: &'static str) -> Self {
        Self { status, code }
    }

    pub(crate) fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "login_required")
    }

    pub(crate) fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden")
    }

    pub(crate) fn invalid() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request")
    }

    pub(crate) fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "server_error")
    }
}

impl From<AuthError> for Error {
    fn from(value: AuthError) -> Self {
        match value {
            AuthError::Denied => Self::unauthorized(),
            AuthError::Invalid => Self::invalid(),
            AuthError::Unavailable => Self::new(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable"),
        }
    }
}

impl From<StoreError> for Error {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::InvalidInput(_) => Self::invalid(),
            StoreError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found"),
            StoreError::VersionConflict { .. } => Self::new(StatusCode::CONFLICT, "version_conflict"),
            StoreError::OperationConflict => Self::new(StatusCode::CONFLICT, "operation_conflict"),
            StoreError::FileTooLarge => Self::new(StatusCode::PAYLOAD_TOO_LARGE, "file_too_large"),
            StoreError::QuotaExceeded => Self::new(StatusCode::INSUFFICIENT_STORAGE, "quota_exceeded"),
            StoreError::Busy => Self::new(StatusCode::SERVICE_UNAVAILABLE, "store_busy"),
            StoreError::InvalidLimits | StoreError::Unavailable | StoreError::UnsupportedSchema(_) | StoreError::Database(_) => Self::internal(),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({"error": self.code}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_METADATA_BYTES, attachment_header, one_header, parse_save};
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn save_framing_preserves_utf8_and_native_bytes() {
        let metadata = r#"{"app":"wordcraft","operation":"op","name":"Résumé.docx"}"#.as_bytes();
        let mut body = (metadata.len() as u32).to_be_bytes().to_vec();
        body.extend_from_slice(metadata);
        body.extend_from_slice(&[0, 128, 255]);
        let (metadata, bytes) = parse_save(&body).ok().expect("valid frame");
        assert_eq!(metadata.name, "Résumé.docx");
        assert_eq!(bytes, &[0, 128, 255]);
        assert!(parse_save(&body[..3]).is_err());
        assert!(parse_save(&body[..body.len() - 3]).is_err());
        assert!(parse_save(&((MAX_METADATA_BYTES + 1) as u32).to_be_bytes()).is_err());
    }

    #[test]
    fn mutation_headers_reject_duplicate_origins() {
        let mut headers = HeaderMap::new();
        let value = HeaderValue::from_static("https://example.com");
        headers.append(header::ORIGIN, value.clone());
        assert_eq!(one_header(&headers, "origin"), Some("https://example.com"));
        headers.append(header::ORIGIN, value);
        assert_eq!(one_header(&headers, "origin"), None);
    }

    #[test]
    fn download_names_encode_spaces_and_unicode() {
        let header = attachment_header("é space.docx").ok().expect("valid attachment");
        assert!(header.to_str().expect("ASCII header").ends_with("%C3%A9%20space.docx"));
    }
}
