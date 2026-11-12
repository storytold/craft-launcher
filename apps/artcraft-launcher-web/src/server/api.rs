//! Same-origin transport for the optional hosted workspace.
use js_sys::{Array, Reflect, Uint8Array};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, Request, RequestInit, RequestMode, RequestRedirect, Response};

pub(super) const LOGIN: &str = "/api/server/v1/auth/login";
const PREFIX: &str = "/api/server/v1";
const FILE_LIMIT: usize = 80 * 1024 * 1024;
const JSON_LIMIT: usize = 256 * 1024;
const REQUEST_TIMEOUT_MS: u32 = 20_000;
const TRANSFER_TIMEOUT_MS: u32 = 100_000;
const MAX_TRANSFER_TIMEOUT_MS: u32 = 260_000;
const TRANSFER_OVERHEAD_MS: u32 = 20_000;

fn transfer_timeout_ms(size: u32) -> u32 {
    // Match the server's four-minute body budget at the file limit and leave time for its reply.
    let scaled_ms = size.div_ceil(1024) * 3;
    (TRANSFER_OVERHEAD_MS + scaled_ms).clamp(TRANSFER_TIMEOUT_MS, MAX_TRANSFER_TIMEOUT_MS)
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Session {
    pub(super) namespace: String,
    pub(super) label: String,
    pub(super) admin: bool,
    csrf: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) enum AppId {
    #[serde(rename = "photocraft")]
    Photo,
    #[serde(rename = "wordcraft")]
    Word,
    #[serde(rename = "gridcraft")]
    Grid,
    #[serde(rename = "cadcraft")]
    Cad,
}

impl AppId {
    pub(super) fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "photocraft" => Some(Self::Photo),
            "wordcraft" => Some(Self::Word),
            "gridcraft" => Some(Self::Grid),
            "cadcraft" => Some(Self::Cad),
            _ => None,
        }
    }

    pub(super) fn slug(self) -> &'static str {
        match self {
            Self::Photo => "photocraft",
            Self::Word => "wordcraft",
            Self::Grid => "gridcraft",
            Self::Cad => "cadcraft",
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Photo => "PhotoCraft",
            Self::Word => "WordCraft",
            Self::Grid => "GridCraft",
            Self::Cad => "CADCraft",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Project {
    pub(super) id: String,
    pub(super) app: AppId,
    pub(super) name: String,
    pub(super) version: u64,
    pub(super) size: u64,
    pub(super) sha256: String,
    pub(super) updated_at: i64,
}

impl Project {
    fn validate(&self) -> Result<(), Failure> {
        let valid = resource_id(&self.id)
            && !self.name.is_empty()
            && self.name.len() <= 512
            && !self.name.chars().any(char::is_control)
            && !self.name.contains(['/', '\\'])
            && self.version != 0
            && (1..=FILE_LIMIT as u64).contains(&self.size)
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit());
        if valid { Ok(()) } else { Err(Failure::InvalidResponse) }
    }

    pub(super) fn download_url(&self) -> String {
        format!("{PREFIX}/files/{}/download?version={}", self.id, self.version)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Editor {
    pub(super) app: AppId,
    pub(super) build: String,
}

impl Editor {
    pub(super) fn url(&self) -> String {
        format!("/editors/{}/{}/", self.app.slug(), self.build)
    }
}

#[derive(Serialize)]
pub(super) struct Save<'a> {
    pub(super) app: AppId,
    pub(super) operation: &'a str,
    pub(super) name: &'a str,
    pub(super) project_id: Option<&'a str>,
    pub(super) expected_version: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Http(u16),
    Transport,
    InvalidRequest,
    InvalidResponse,
}

#[derive(Deserialize)]
struct Info {
    mode: String,
    protocol: u8,
}

pub(super) async fn enabled() -> bool {
    get::<Info>("/info").await.is_ok_and(|info| info.mode == "server" && info.protocol == 1)
}

pub(super) async fn session() -> Result<Session, Failure> {
    let session: Session = get("/session").await?;
    if session.csrf.len() != 64 || !session.csrf.bytes().all(|b| b.is_ascii_hexdigit()) || session.label.len() > 254 {
        return Err(Failure::InvalidResponse);
    }
    if session.namespace.len() != 64 || !session.namespace.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Failure::InvalidResponse);
    }
    Ok(session)
}

pub(super) async fn activity(session: &Session) -> Result<(), Failure> {
    change_empty("/session/activity", session).await
}

pub(super) async fn logout(session: &Session) -> Result<(), Failure> {
    change_empty("/session/logout", session).await
}

pub(super) async fn projects(app: Option<AppId>) -> Result<Vec<Project>, Failure> {
    let path = app.map_or_else(|| String::from("/files"), |app| format!("/files?app={}", app.slug()));
    let rows: Vec<Project> = get(&path).await?;
    if rows.len() > 100 {
        return Err(Failure::InvalidResponse);
    }
    for row in &rows {
        row.validate()?;
        if app.is_some_and(|app| app != row.app) {
            return Err(Failure::InvalidResponse);
        }
    }
    Ok(rows)
}

pub(super) async fn editors() -> Result<Vec<Editor>, Failure> {
    let editors: Vec<Editor> = get("/editors").await?;
    if editors.len() > 4 {
        return Err(Failure::InvalidResponse);
    }
    for editor in &editors {
        if editor.build.is_empty() || editor.build.len() > 128 || !editor.build.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Failure::InvalidResponse);
        }
    }
    Ok(editors)
}

pub(super) async fn content(project: &Project) -> Result<Uint8Array, Failure> {
    project.validate()?;
    let path = format!("/files/{}/content?version={}", project.id, project.version);
    let response = request("GET", &path, None, None, transfer_timeout_ms(project.size as u32)).await?;
    let announced = response.headers().get("content-length").map_err(|_| Failure::InvalidResponse)?.and_then(|length| length.parse::<u64>().ok());
    if announced != Some(project.size) {
        return Err(Failure::InvalidResponse);
    }
    let output = Uint8Array::new_with_length(project.size as u32);
    let mut offset = 0;
    read(response, project.size as usize, |chunk| {
        output.set(&chunk, offset);
        offset += chunk.length();
    })
    .await?;
    if u64::from(offset) != project.size {
        return Err(Failure::InvalidResponse);
    }
    Ok(output)
}

pub(super) async fn save(session: &Session, metadata: &Save<'_>, bytes: &Uint8Array) -> Result<Project, Failure> {
    if !(1..=FILE_LIMIT).contains(&(bytes.length() as usize)) {
        return Err(Failure::InvalidRequest);
    }
    let json = serde_json::to_vec(metadata).map_err(|_| Failure::InvalidRequest)?;
    if json.is_empty() || json.len() > 4096 {
        return Err(Failure::InvalidRequest);
    }
    let parts = Array::new();
    parts.push(&Uint8Array::from((json.len() as u32).to_be_bytes().as_slice()));
    parts.push(&Uint8Array::from(json.as_slice()));
    parts.push(bytes);
    let body = Blob::new_with_u8_array_sequence(&parts).map_err(|_| Failure::InvalidRequest)?;
    let response =
        request("POST", "/files", Some(session), Some(("application/octet-stream", body.as_ref())), transfer_timeout_ms(bytes.length())).await?;
    let project: Project = decode(response).await?;
    project.validate()?;
    if project.app != metadata.app
        || project.name != metadata.name
        || project.size != u64::from(bytes.length())
        || metadata.project_id.is_some_and(|id| id != project.id)
    {
        return Err(Failure::InvalidResponse);
    }
    Ok(project)
}

pub(super) fn resource_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| if matches!(i, 8 | 13 | 18 | 23) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}

pub(super) async fn get<T: DeserializeOwned>(path: &str) -> Result<T, Failure> {
    decode(request("GET", path, None, None, REQUEST_TIMEOUT_MS).await?).await
}

pub(super) async fn change<T: DeserializeOwned>(path: &str, session: &Session, payload: &impl Serialize) -> Result<T, Failure> {
    let json = serde_json::to_string(payload).map_err(|_| Failure::InvalidRequest)?;
    let json = JsValue::from(json);
    let response = request("POST", path, Some(session), Some(("application/json", &json)), REQUEST_TIMEOUT_MS).await?;
    decode(response).await
}

async fn change_empty(path: &str, session: &Session) -> Result<(), Failure> {
    let response = request("POST", path, Some(session), None, REQUEST_TIMEOUT_MS).await?;
    if response.status() != 204 {
        return Err(Failure::InvalidResponse);
    }
    Ok(())
}

async fn request(method: &str, path: &str, session: Option<&Session>, body: Option<(&str, &JsValue)>, timeout: u32) -> Result<Response, Failure> {
    let window = web_sys::window().ok_or(Failure::Transport)?;
    let init = RequestInit::new();
    init.set_method(method);
    init.set_mode(RequestMode::SameOrigin);
    init.set_redirect(RequestRedirect::Error);
    init.set_cache(web_sys::RequestCache::NoStore);
    init.set_signal(Some(&web_sys::AbortSignal::timeout_with_u32(timeout)));
    if let Some((_, body)) = body {
        init.set_body(body);
    }
    let request = Request::new_with_str_and_init(&format!("{PREFIX}{path}"), &init).map_err(|_| Failure::InvalidRequest)?;
    if let Some(session) = session {
        request.headers().set("x-artcraft-csrf", &session.csrf).map_err(|_| Failure::InvalidRequest)?;
    }
    if let Some((content_type, _)) = body {
        request.headers().set("content-type", content_type).map_err(|_| Failure::InvalidRequest)?;
    }
    let response: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| Failure::Transport)?
        .dyn_into()
        .map_err(|_| Failure::InvalidResponse)?;
    if !response.ok() {
        return Err(Failure::Http(response.status()));
    }
    Ok(response)
}

async fn decode<T: DeserializeOwned>(response: Response) -> Result<T, Failure> {
    let mut bytes = Vec::new();
    read(response, JSON_LIMIT, |chunk| bytes.extend_from_slice(&chunk.to_vec())).await?;
    serde_json::from_slice(&bytes).map_err(|_| Failure::InvalidResponse)
}

async fn read(response: Response, limit: usize, mut consume: impl FnMut(Uint8Array)) -> Result<(), Failure> {
    let reader: web_sys::ReadableStreamDefaultReader =
        response.body().ok_or(Failure::InvalidResponse)?.get_reader().dyn_into().map_err(|_| Failure::InvalidResponse)?;
    let mut remaining = limit;
    loop {
        let result = JsFuture::from(reader.read()).await.map_err(|_| Failure::Transport)?;
        if Reflect::get(&result, &JsValue::from_str("done")).ok().and_then(|value| value.as_bool()) == Some(true) {
            return Ok(());
        }
        let data: Uint8Array = Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|_| Failure::InvalidResponse)?
            .dyn_into()
            .map_err(|_| Failure::InvalidResponse)?;
        let Some(next) = remaining.checked_sub(data.length() as usize) else {
            let _ = reader.cancel();
            return Err(Failure::InvalidResponse);
        };
        remaining = next;
        consume(data);
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Release {
    pub(super) id: String,
    pub(super) app: AppId,
    pub(super) source_commit: String,
    pub(super) source_tree: String,
    pub(super) state: String,
    pub(super) published: bool,
}

pub(super) async fn releases() -> Result<Vec<Release>, Failure> {
    let releases: Vec<Release> = get("/admin/releases").await?;
    if releases.len() > 256 || releases.iter().any(|release| !release_id(&release.id)) {
        return Err(Failure::InvalidResponse);
    }
    Ok(releases)
}

#[derive(Serialize)]
pub(super) struct Source<'a> {
    pub(super) app: AppId,
    pub(super) source_commit: &'a str,
    pub(super) source_tree: &'a str,
}

pub(super) async fn queue(session: &Session, source: &Source<'_>) -> Result<Release, Failure> {
    change("/admin/releases", session, source).await
}

pub(super) async fn cancel(session: &Session, id: &str) -> Result<Release, Failure> {
    if !release_id(id) {
        return Err(Failure::InvalidRequest);
    }
    let response = request("POST", &format!("/admin/releases/{id}/cancel"), Some(session), None, REQUEST_TIMEOUT_MS).await?;
    decode(response).await
}

#[derive(Clone, Copy)]
pub(super) enum ReleaseAction {
    Activate,
    Rollback,
}

pub(super) async fn select_release(session: &Session, app: AppId, build: &str, action: ReleaseAction) -> Result<Release, Failure> {
    if !release_id(build) {
        return Err(Failure::InvalidRequest);
    }
    let action = match action {
        ReleaseAction::Activate => "activate",
        ReleaseAction::Rollback => "rollback",
    };
    let path = format!("/admin/editors/{}/{action}", app.slug());
    change(&path, session, &serde_json::json!({"build": build})).await
}

pub(super) fn release_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
