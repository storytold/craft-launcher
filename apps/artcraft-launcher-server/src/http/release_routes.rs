use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use subtle::ConstantTimeEq as _;
use tower::ServiceExt as _;
use tower_http::services::ServeFile;

use super::{App, Error, OwnedSemaphorePermit, UPLOAD_TIMEOUT, one_header, query_values};
use crate::releases::{self, Claim, Manifest, Release, ReleaseError, ReleaseRequest};

const APPS: [&str; 4] = ["photocraft", "wordcraft", "gridcraft", "cadcraft"];
const WORKER: &str = "paired-pc";
const MANIFEST_LIMIT: usize = 2 * 1024 * 1024;
const COMMAND_LIMIT: usize = 4096;

pub(super) fn api() -> Router<App> {
    Router::new()
        .route("/editors", get(active_editors))
        .route("/admin/releases", get(admin_list).post(admin_queue))
        .route("/admin/releases/{id}/cancel", post(admin_cancel))
        .route("/admin/editors/{app}/activate", post(admin_activate))
        .route("/admin/editors/{app}/rollback", post(admin_rollback))
        .route("/worker/claim", post(worker_claim))
        .route("/worker/{id}/renew", post(worker_renew))
        .route("/worker/{id}/manifest", post(worker_manifest))
        .route("/worker/{id}/file", put(worker_file))
        .route("/worker/{id}/finalize", post(worker_finalize))
        .route("/worker/{id}/fail", post(worker_fail))
}

pub(super) fn assets() -> Router<App> {
    Router::new().route("/editors/{app}/{build}/", get(editor_index)).route("/editors/{app}/{build}/{*path}", get(editor_asset))
}

#[derive(Serialize)]
struct ActiveEditor {
    app: &'static str,
    build: String,
}

async fn active_editors(State(app): State<App>, headers: HeaderMap) -> Result<Json<Vec<ActiveEditor>>, Error> {
    app.authenticate(&headers)?;
    run(move || {
        let mut editors = Vec::new();
        for id in APPS {
            if let Some(release) = app.releases.active(id)? {
                editors.push(ActiveEditor { app: id, build: release.id });
            }
        }
        Ok(Json(editors))
    })
    .await
}

async fn editor_index(State(app): State<App>, Path((editor, build)): Path<(String, String)>, request: Request) -> Result<Response, Error> {
    serve_asset(app, editor, build, String::from("index.html"), request).await
}

async fn editor_asset(
    State(app): State<App>,
    Path((editor, build, path)): Path<(String, String, String)>,
    request: Request,
) -> Result<Response, Error> {
    serve_asset(app, editor, build, path, request).await
}

async fn serve_asset(app: App, editor: String, build: String, path: String, request: Request) -> Result<Response, Error> {
    let session = app.authenticate(request.headers())?;
    let path = run(move || app.releases.asset_path(&editor, &build, &path, session.admin)).await?;
    let file = match ServeFile::new(path).oneshot(request).await {
        Ok(file) => file,
        Err(never) => match never {},
    };
    let mut response = file.into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    Ok(response)
}

async fn admin_list(State(app): State<App>, headers: HeaderMap) -> Result<Json<Vec<Release>>, Error> {
    if !app.authenticate(&headers)?.admin {
        return Err(Error::forbidden());
    }
    run(move || app.releases.list()).await.map(Json)
}

async fn admin_queue(State(app): State<App>, headers: HeaderMap, body: Body) -> Result<Json<Release>, Error> {
    admin_change(&app, &headers)?;
    let source: ReleaseRequest = json(body, &headers, COMMAND_LIMIT).await?;
    admin_change(&app, &headers)?;
    run(move || app.releases.queue(source)).await.map(Json)
}

async fn admin_cancel(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Result<Json<Release>, Error> {
    admin_change(&app, &headers)?;
    run(move || app.releases.cancel(&id)).await.map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseSelection {
    build: String,
}

async fn admin_activate(State(app): State<App>, Path(editor): Path<String>, headers: HeaderMap, body: Body) -> Result<Json<Release>, Error> {
    admin_change(&app, &headers)?;
    let selection: ReleaseSelection = json(body, &headers, COMMAND_LIMIT).await?;
    admin_change(&app, &headers)?;
    run(move || app.releases.activate(&editor, &selection.build)).await.map(Json)
}

async fn admin_rollback(State(app): State<App>, Path(editor): Path<String>, headers: HeaderMap, body: Body) -> Result<Json<Release>, Error> {
    admin_change(&app, &headers)?;
    let selection: ReleaseSelection = json(body, &headers, COMMAND_LIMIT).await?;
    admin_change(&app, &headers)?;
    run(move || app.releases.rollback(&editor, &selection.build)).await.map(Json)
}

fn admin_change(app: &App, headers: &HeaderMap) -> Result<(), Error> {
    if !app.authorize_change(headers)?.admin {
        return Err(Error::forbidden());
    }
    Ok(())
}

#[derive(Serialize)]
struct ClaimResponse {
    claim: Option<Claim>,
}

async fn worker_claim(State(app): State<App>, headers: HeaderMap) -> Result<Json<ClaimResponse>, Error> {
    worker_auth(&app, &headers)?;
    run(move || app.releases.claim(WORKER).map(|claim| Json(ClaimResponse { claim }))).await
}

#[derive(Serialize)]
struct RenewResponse {
    expires_at: i64,
}

async fn worker_renew(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Result<Json<RenewResponse>, Error> {
    let claim = worker_claim_id(&app, &headers)?;
    run(move || app.releases.renew(&id, WORKER, &claim).map(|expires_at| Json(RenewResponse { expires_at }))).await
}

async fn worker_manifest(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap, body: Body) -> Result<StatusCode, Error> {
    let claim = worker_claim_id(&app, &headers)?;
    let permit = app.transfer()?;
    let manifest: Manifest = json(body, &headers, MANIFEST_LIMIT).await?;
    run(move || {
        let _permit = permit;
        app.releases.submit_manifest(&id, WORKER, &claim, manifest)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn worker_file(
    State(app): State<App>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, Error> {
    let claim = worker_claim_id(&app, &headers)?;
    let permit = app.transfer()?;
    let [path] = query_values(raw.as_deref(), ["path"], false)?;
    let path = path.ok_or_else(Error::invalid)?;
    let bytes = read(body, &headers, "application/octet-stream", releases::MAX_FILE_BYTES).await?;
    write_file(app, id, path, claim, permit, bytes).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn write_file(app: App, id: String, path: String, claim: String, permit: OwnedSemaphorePermit, bytes: Bytes) -> Result<(), Error> {
    run(move || {
        let _permit = permit;
        app.releases.upload(&id, WORKER, &claim, &path, &bytes)
    })
    .await
}

async fn worker_finalize(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Result<Json<Release>, Error> {
    let claim = worker_claim_id(&app, &headers)?;
    let permit = app.transfer()?;
    run(move || {
        let _permit = permit;
        app.releases.finalize(&id, WORKER, &claim)
    })
    .await
    .map(Json)
}

async fn worker_fail(State(app): State<App>, Path(id): Path<String>, headers: HeaderMap) -> Result<Json<Release>, Error> {
    let claim = worker_claim_id(&app, &headers)?;
    run(move || app.releases.fail(&id, WORKER, &claim)).await.map(Json)
}

fn worker_auth(app: &App, headers: &HeaderMap) -> Result<(), Error> {
    let supplied =
        one_header(headers, header::AUTHORIZATION.as_str()).and_then(|value| value.strip_prefix("Bearer ")).ok_or_else(Error::unauthorized)?;
    if !bool::from(app.worker_secret.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(Error::unauthorized());
    }
    Ok(())
}

fn worker_claim_id(app: &App, headers: &HeaderMap) -> Result<String, Error> {
    worker_auth(app, headers)?;
    one_header(headers, "x-artcraft-claim").filter(|id| !id.is_empty() && id.len() <= 128).map(str::to_owned).ok_or_else(Error::invalid)
}

async fn json<T: DeserializeOwned>(body: Body, headers: &HeaderMap, limit: usize) -> Result<T, Error> {
    let bytes = read(body, headers, "application/json", limit).await?;
    serde_json::from_slice(&bytes).map_err(|_| Error::invalid())
}

async fn read(body: Body, headers: &HeaderMap, kind: &str, limit: usize) -> Result<Bytes, Error> {
    if one_header(headers, header::CONTENT_TYPE.as_str()) != Some(kind) {
        return Err(Error::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "invalid_content_type"));
    }
    tokio::time::timeout(UPLOAD_TIMEOUT, to_bytes(body, limit))
        .await
        .map_err(|_| Error::new(StatusCode::REQUEST_TIMEOUT, "upload_timeout"))?
        .map_err(|_| Error::new(StatusCode::PAYLOAD_TOO_LARGE, "invalid_upload_body"))
}

async fn run<T: Send + 'static>(task: impl FnOnce() -> Result<T, ReleaseError> + Send + 'static) -> Result<T, Error> {
    tokio::task::spawn_blocking(task).await.map_err(|_| Error::internal())?.map_err(Error::from)
}

impl From<ReleaseError> for Error {
    fn from(value: ReleaseError) -> Self {
        match value {
            ReleaseError::Invalid => Self::invalid(),
            ReleaseError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found"),
            ReleaseError::Conflict => Self::new(StatusCode::CONFLICT, "release_conflict"),
            ReleaseError::Incomplete => Self::new(StatusCode::CONFLICT, "incomplete_release"),
            ReleaseError::Limit => Self::new(StatusCode::INSUFFICIENT_STORAGE, "release_limit"),
            ReleaseError::Database(_) | ReleaseError::Filesystem(_) | ReleaseError::Json(_) => Self::internal(),
        }
    }
}
