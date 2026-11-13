//! The optional server workspace and shared editor host.

mod api;
mod panel;
mod providers;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use artcraft_launcher_ui_egui::i18n;
use js_sys::{Array, Uint8Array};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Blob, Document, Event, EventTarget, HtmlAnchorElement, HtmlElement, HtmlIFrameElement, HtmlInputElement, Window};

use api::{AppId, Failure, Project, Session};
use panel::Panel;
use providers::{
    Connection, OpenRequest, ProviderEvent, ProviderSession, Registration, Request, Resource, SaveCapture, SaveChoice, SaveDialog, SaveResult,
    SaveTarget,
};

const MAX_CHOOSERS: usize = 16;
const MAX_ACTIVE_SAVES: u8 = 2;
const ACTIVITY_INTERVAL_MS: f64 = 30_000.0;
const URL_LIFETIME_MS: i32 = 60_000;

thread_local! {
    static WORKSPACE: RefCell<Option<Rc<Workspace>>> = const { RefCell::new(None) };
}

pub(crate) fn editor_url(app: &str) -> Option<String> {
    WORKSPACE.with_borrow(|current| {
        let workspace = current.as_ref()?;
        let app = AppId::from_slug(app)?;
        if !workspace.editors.borrow().iter().any(|editor| editor.app == app) {
            return None;
        }
        let origin = workspace.window.location().origin().ok()?;
        Some(format!("{origin}{}", editor_link(app, None)))
    })
}

/// Returns true when the URL selected a hosted editor and owns the page.
pub(crate) async fn mount(document: &Document) -> bool {
    if !api::enabled().await {
        return false;
    }
    let Some(window) = web_sys::window() else { return false };
    match Workspace::mount(window, document.clone()).await {
        Ok(workspace) => {
            let editor = workspace.route.is_some();
            WORKSPACE.set(Some(workspace));
            editor
        }
        Err(_) => {
            log::warn!("The hosted workspace could not be initialized");
            false
        }
    }
}

#[derive(Clone)]
struct EditorRoute {
    app: AppId,
    project: Option<String>,
}

fn editor_route(window: &Window) -> Option<EditorRoute> {
    let query = web_sys::UrlSearchParams::new_with_str(&window.location().search().ok()?).ok()?;
    let app = AppId::from_slug(&query.get("editor")?)?;
    let project = query.get("project").filter(|value| api::resource_id(value));
    Some(EditorRoute { app, project })
}

fn editor_link(app: AppId, project: Option<&str>) -> String {
    let mut url = format!("/?editor={}", app.slug());
    if let Some(project) = project.filter(|id| api::resource_id(id)) {
        url.push_str("&project=");
        url.push_str(project);
    }
    url
}

struct Listener {
    target: EventTarget,
    event: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}

impl Listener {
    fn add(target: &EventTarget, event: &'static str, callback: impl FnMut(Event) + 'static) -> Result<Self, JsValue> {
        let callback = Closure::<dyn FnMut(Event)>::new(callback);
        target.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref())?;
        Ok(Self { target: target.clone(), event, callback })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(self.event, self.callback.as_ref().unchecked_ref());
    }
}

struct Workspace {
    window: Window,
    document: Document,
    panel: Panel,
    route: Option<EditorRoute>,
    session: RefCell<Option<Session>>,
    editors: RefCell<Vec<api::Editor>>,
    projects: RefCell<Vec<Project>>,
    jobs: RefCell<Vec<api::Release>>,
    generation: Cell<u64>,
    last_activity: Cell<f64>,
    refresh_generation: Cell<u64>,
    recents_generation: Cell<u64>,
    view: Cell<View>,
    status: Cell<Option<&'static str>>,
    listeners: RefCell<Vec<Listener>>,
    editor: RefCell<EditorState>,
}

#[derive(Default)]
struct EditorState {
    frame: Option<HtmlIFrameElement>,
    provider: Option<ProviderSession>,
    frame_listeners: Vec<Listener>,
    recents: Vec<Project>,
    opening: HashMap<String, Project>,
    document_versions: HashMap<String, Project>,
    choosers: VecDeque<SaveDialog>,
    requested_project: Option<String>,
    active_saves: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Files,
    Account,
    Updates,
}

impl Workspace {
    async fn mount(window: Window, document: Document) -> Result<Rc<Self>, JsValue> {
        let route = editor_route(&window);
        let panel = Panel::mount(&document, &window)?;
        let state = Rc::new(Self {
            window,
            document,
            panel,
            route,
            session: RefCell::new(None),
            editors: RefCell::new(Vec::new()),
            projects: RefCell::new(Vec::new()),
            jobs: RefCell::new(Vec::new()),
            generation: Cell::new(0),
            last_activity: Cell::new(0.0),
            refresh_generation: Cell::new(0),
            recents_generation: Cell::new(0),
            view: Cell::new(View::Files),
            status: Cell::new(None),
            listeners: RefCell::new(Vec::new()),
            editor: RefCell::new(EditorState::default()),
        });
        state.listen()?;
        match api::session().await {
            Ok(session) => {
                state.session.replace(Some(session));
                match api::editors().await {
                    Ok(editors) => {
                        state.editors.replace(editors);
                        state.mount_editor()?;
                    }
                    Err(error) => state.on_error(error),
                }
                state.refresh();
            }
            Err(Failure::Http(401)) => {}
            Err(error) => state.on_error(error),
        }
        state.render()?;
        if state.route.is_some() && state.editor.borrow().frame.is_none() {
            state.panel.open();
        }
        Ok(state)
    }

    fn listen(self: &Rc<Self>) -> Result<(), JsValue> {
        let weak = Rc::downgrade(self);
        let click = Listener::add(self.panel.body().as_ref(), "click", move |event| {
            let Some(state) = weak.upgrade() else { return };
            if let Err(error) = state.on_click(event) {
                state.report_error(&error);
            }
        })?;
        let mut listeners = self.listeners.borrow_mut();
        listeners.push(click);
        listeners.extend(self.activity_listeners(self.document.as_ref())?);
        let weak = Rc::downgrade(self);
        listeners.push(Listener::add(self.window.as_ref(), "focus", move |_| {
            if let Some(state) = weak.upgrade() {
                state.verify_session();
            }
        })?);
        Ok(())
    }

    fn activity_listeners(self: &Rc<Self>, target: &EventTarget) -> Result<[Listener; 2], JsValue> {
        let make = |event| {
            let weak = Rc::downgrade(self);
            Listener::add(target, event, move |input| {
                if input.is_trusted()
                    && let Some(state) = weak.upgrade()
                {
                    state.activity();
                }
            })
        };
        Ok([make("pointerdown")?, make("keydown")?])
    }

    fn mount_editor(self: &Rc<Self>) -> Result<(), JsValue> {
        let Some(route) = &self.route else { return Ok(()) };
        let editor = self.editors.borrow().iter().find(|entry| entry.app == route.app).cloned();
        let Some(editor) = editor else {
            self.status.set(Some("Not available"));
            return Ok(());
        };
        let frame: HtmlIFrameElement = self.document.create_element("iframe")?.dyn_into()?;
        frame.set_title(route.app.name());
        frame.set_attribute("style", "position:fixed;inset:0;width:100%;height:100%;border:0;z-index:1")?;
        let weak = Rc::downgrade(self);
        self.listeners.borrow_mut().push(Listener::add(frame.as_ref(), "load", move |_| {
            let Some(state) = weak.upgrade() else { return };
            if let Err(error) = state.attach_editor() {
                state.report_error(&error);
            }
        })?);
        frame.set_src(&editor.url());
        self.editor.borrow_mut().requested_project.clone_from(&route.project);
        self.editor.borrow_mut().frame = Some(frame.clone());
        self.document.body().ok_or(JsValue::NULL)?.append_child(frame.as_ref())?;
        Ok(())
    }

    fn attach_editor(self: &Rc<Self>) -> Result<(), JsValue> {
        let Some(route) = &self.route else { return Ok(()) };
        let Some(session) = self.session.borrow().clone() else { return Ok(()) };
        let mut editor = self.editor.borrow_mut();
        let frame = editor.frame.as_ref().ok_or(JsValue::NULL)?;
        let window = frame.content_window().ok_or(JsValue::NULL)?;
        let document = frame.content_document().ok_or(JsValue::NULL)?;
        editor.provider = None;
        editor.frame_listeners.clear();
        editor.document_versions.clear();
        editor.opening.clear();
        editor.recents.clear();
        editor.choosers.clear();
        editor.active_saves = 0;
        self.next_generation();
        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        let app = providers::Editor::from_id(route.app.slug()).ok_or(JsValue::NULL)?;
        editor.provider = Some(ProviderSession::attach(
            window,
            app,
            Registration { id: String::from("artcraft-launcher"), label: String::from("ArtCraft Launcher"), namespace: session.namespace },
            move |event| {
                if let Some(state) = weak.upgrade()
                    && state.current(generation)
                {
                    state.provider_event(event);
                }
            },
        )?);
        editor.frame_listeners.extend(self.activity_listeners(document.as_ref())?);
        Ok(())
    }

    fn next_generation(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    fn current(&self, generation: u64) -> bool {
        self.generation.get() == generation && self.session.borrow().is_some()
    }

    fn verify_session(self: &Rc<Self>) {
        if self.session.borrow().is_none() {
            return;
        }
        let state = Rc::clone(self);
        let generation = self.generation.get();
        spawn_local(async move {
            match api::session().await {
                Ok(session) if state.current(generation) => {
                    if state.session.borrow().as_ref().is_some_and(|previous| previous.namespace != session.namespace) {
                        state.expire();
                    }
                }
                Err(Failure::Http(401)) if state.current(generation) => state.expire(),
                _ => {}
            }
        });
    }

    fn activity(self: &Rc<Self>) {
        let Some(session) = self.session.borrow().clone() else { return };
        let now = js_sys::Date::now();
        if now - self.last_activity.get() < ACTIVITY_INTERVAL_MS {
            return;
        }
        self.last_activity.set(now);
        let state = Rc::clone(self);
        let generation = self.generation.get();
        spawn_local(async move {
            if let Err(Failure::Http(401)) = api::activity(&session).await
                && state.current(generation)
            {
                state.expire();
            }
        });
    }

    fn expire(&self) {
        self.session.replace(None);
        self.next_generation();
        self.projects.borrow_mut().clear();
        self.jobs.borrow_mut().clear();
        self.editors.borrow_mut().clear();
        self.view.set(View::Account);
        let mut editor = self.editor.borrow_mut();
        // The frame is retained so the user can recover unsaved local edits. It cannot reconnect
        // as another user without a page navigation and a newly initialized editor.
        if let Some(provider) = &editor.provider {
            let _ = provider.suspend();
            let _ = provider.disconnect();
        }
        editor.choosers.clear();
        editor.opening.clear();
        editor.recents.clear();
        editor.document_versions.clear();
        drop(editor);
        self.render_safely();
        self.panel.open();
    }

    fn refresh(self: &Rc<Self>) {
        if self.session.borrow().is_none() {
            return;
        }
        let state = Rc::clone(self);
        let generation = self.generation.get();
        let refresh = self.refresh_generation.get().wrapping_add(1);
        self.refresh_generation.set(refresh);
        spawn_local(async move {
            let result = api::projects(None).await;
            if !state.current(generation) || state.refresh_generation.get() != refresh {
                return;
            }
            match result {
                Ok(projects) => {
                    state.projects.replace(projects);
                    state.render_safely();
                }
                Err(error) => state.on_error(error),
            }
        });
    }

    fn provider_event(self: &Rc<Self>, event: ProviderEvent) {
        match event {
            ProviderEvent::Connected(connection) => {
                self.refresh();
                self.refresh_recents(connection);
                self.render_safely();
            }
            ProviderEvent::ChooseSave(dialog) => self.enqueue_chooser(dialog),
            ProviderEvent::Open(open) => self.read_project(open),
            ProviderEvent::Save(capture) => self.write_project(capture),
            ProviderEvent::Opened { request, resource_id, document_id } => {
                let mut editor = self.editor.borrow_mut();
                if let Some(project) = editor.opening.remove(request.id())
                    && project.id == resource_id
                {
                    editor.document_versions.insert(document_id, project);
                }
            }
            ProviderEvent::OpenFailed { request } => {
                self.editor.borrow_mut().opening.remove(request.id());
                self.status.set(Some("Not available"));
                self.render_safely();
                self.panel.open();
            }
            ProviderEvent::OpenCancelled { request } => {
                self.editor.borrow_mut().opening.remove(request.id());
            }
            ProviderEvent::SaveUnchanged => self.status.set(None),
            ProviderEvent::SaveFailed => {
                self.status.set(Some("Could not save the file"));
                self.render_safely();
            }
            ProviderEvent::BridgeError => self.report_error(&JsValue::NULL),
        }
    }

    fn refresh_recents(self: &Rc<Self>, connection: Connection) {
        let Some(route) = &self.route else { return };
        let app = route.app;
        let state = Rc::clone(self);
        let generation = self.generation.get();
        let refresh = self.recents_generation.get().wrapping_add(1);
        self.recents_generation.set(refresh);
        spawn_local(async move {
            let result = api::projects(Some(app)).await;
            if !state.current(generation) || state.recents_generation.get() != refresh {
                return;
            }
            match result {
                Ok(projects) => state.install_recents(connection, projects),
                Err(error) => state.on_error(error),
            }
        });
    }

    fn install_recents(&self, connection: Connection, projects: Vec<Project>) {
        let mut editor = self.editor.borrow_mut();
        let Some(provider) = &editor.provider else { return };
        let resources: Vec<_> = projects
            .iter()
            .map(|project| Resource {
                id: project.id.clone(),
                filename: project.name.clone(),
                updated_at: jiff::Timestamp::from_second(project.updated_at).map_or_else(|_| String::new(), |date| date.to_string()),
            })
            .collect();
        if provider.set_resources(&connection, &resources).is_err() {
            return;
        }
        editor.recents = projects;
        if let Some(id) = editor.requested_project.take() {
            let result = editor.provider.as_ref().ok_or(JsValue::NULL).and_then(|provider| provider.request_open(&id));
            if let Err(error) = result {
                drop(editor);
                self.report_error(&error);
            }
        }
    }

    fn enqueue_chooser(&self, dialog: SaveDialog) {
        let mut editor = self.editor.borrow_mut();
        if editor.choosers.len() >= MAX_CHOOSERS {
            if let Some(provider) = &editor.provider {
                let _ = provider.resolve_save(&dialog.request, SaveChoice::Cancel, "");
            }
            return;
        }
        editor.choosers.push_back(dialog);
        drop(editor);
        self.render_safely();
        self.panel.open();
    }

    fn resolve_chooser(&self, choice: SaveChoice) -> Result<(), JsValue> {
        let mut editor = self.editor.borrow_mut();
        let Some(dialog) = editor.choosers.front() else { return Ok(()) };
        let filename = self.input("craft-cloud-save-name").map_or_else(|| dialog.filename.clone(), |input| input.value());
        let Some(provider) = &editor.provider else { return Ok(()) };
        provider.resolve_save(&dialog.request, choice, &filename)?;
        editor.choosers.pop_front();
        drop(editor);
        self.status.set(None);
        self.render()
    }

    fn read_project(self: &Rc<Self>, open: OpenRequest) {
        let project = self.editor.borrow().recents.iter().find(|project| project.id == open.resource_id).cloned();
        let Some(project) = project else {
            self.cancel_open(&open.request);
            return;
        };
        let state = Rc::clone(self);
        let generation = self.generation.get();
        spawn_local(async move {
            let result = api::content(&project).await;
            if !state.current(generation) {
                return;
            }
            match result {
                Ok(bytes) => state.deliver_project(open, project, bytes),
                Err(error) => {
                    state.cancel_open(&open.request);
                    state.on_error(error);
                }
            }
        });
    }

    fn deliver_project(&self, open: OpenRequest, project: Project, bytes: Uint8Array) {
        let mut editor = self.editor.borrow_mut();
        editor.opening.insert(open.request.id().to_owned(), project.clone());
        let result = editor.provider.as_ref().ok_or(JsValue::NULL).and_then(|provider| provider.open_resource(&open.request, &project.name, &bytes));
        if let Err(error) = result {
            editor.opening.remove(open.request.id());
            drop(editor);
            self.cancel_open(&open.request);
            self.report_error(&error);
        }
    }

    fn cancel_open(&self, request: &Request) {
        if let Some(provider) = &self.editor.borrow().provider {
            let _ = provider.cancel_open(request, label("Not available"));
        }
    }

    fn write_project(self: &Rc<Self>, capture: SaveCapture) {
        if capture.target == SaveTarget::Device {
            self.download_capture(capture);
            return;
        }
        let (Some(route), Some(session)) = (&self.route, self.session.borrow().clone()) else {
            self.fail_save(&capture.request);
            return;
        };
        let mut editor = self.editor.borrow_mut();
        let base = capture
            .resource_id
            .as_ref()
            .and_then(|id| editor.document_versions.get(&capture.document_id).filter(|project| &project.id == id).cloned());
        let unsupported = capture.resource_id.is_some() && base.is_none();
        if unsupported || editor.active_saves >= MAX_ACTIVE_SAVES || capture.request.id().len() > 128 {
            drop(editor);
            self.fail_save(&capture.request);
            self.status.set(Some(if unsupported { "Save a copy to continue" } else { "Could not save the file" }));
            self.render_safely();
            return;
        }
        editor.active_saves += 1;
        drop(editor);
        let generation = self.generation.get();
        let app = route.app;
        let state = Rc::clone(self);
        spawn_local(async move {
            let SaveCapture { request, filename, document_id, bytes: data, .. } = capture;
            let metadata = api::Save {
                app,
                operation: request.id(),
                name: &filename,
                project_id: base.as_ref().map(|project| project.id.as_str()),
                expected_version: base.as_ref().map(|project| project.version),
            };
            let mut result = api::save(&session, &metadata, &data).await;
            if result.as_ref().err() == Some(&Failure::Transport) && state.current(generation) {
                result = api::save(&session, &metadata, &data).await;
            }
            if state.current(generation) {
                state.complete_save(request, document_id, result);
            }
        });
    }

    fn complete_save(self: &Rc<Self>, request: Request, document_id: String, result: Result<Project, Failure>) {
        let mut editor = self.editor.borrow_mut();
        editor.active_saves = editor.active_saves.saturating_sub(1);
        let project = match result {
            Ok(project) => project,
            Err(error) => {
                drop(editor);
                self.fail_save(&request);
                self.on_error(error);
                self.panel.open();
                return;
            }
        };
        let Some(provider) = &editor.provider else { return };
        let accepted = provider.save_result(&request, SaveResult::Saved { resource_id: &project.id }).is_ok();
        let connection = provider.connection();
        if accepted {
            editor.document_versions.insert(document_id, project);
        }
        drop(editor);
        if !accepted {
            self.status.set(Some("Could not confirm the save"));
        } else {
            self.status.set(Some("Saved"));
        }
        self.refresh();
        if let Some(connection) = connection {
            self.refresh_recents(connection);
        }
    }

    fn fail_save(&self, request: &Request) {
        if let Some(provider) = &self.editor.borrow().provider {
            // The SDK cancellation releases the request and leaves the editor revision dirty.
            // A new Save can capture the user's current revision.
            let _ = provider.save_result(request, SaveResult::Cancelled);
        }
    }

    fn download_capture(&self, capture: SaveCapture) {
        if let Err(error) = self.download_bytes(&capture.filename, &capture.bytes) {
            self.fail_save(&capture.request);
            self.report_error(&error);
            return;
        }
        if let Some(provider) = &self.editor.borrow().provider {
            let _ = provider.save_result(&capture.request, SaveResult::Downloaded);
        }
    }

    fn download_bytes(&self, filename: &str, bytes: &Uint8Array) -> Result<(), JsValue> {
        let parts = Array::new();
        parts.push(bytes);
        let blob = Blob::new_with_u8_array_sequence(&parts)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;
        let result = self.click_download(filename, &url);
        let cleanup_url = url.clone();
        let cleanup = Closure::once_into_js(move || {
            let _ = web_sys::Url::revoke_object_url(&cleanup_url);
        });
        if self.window.set_timeout_with_callback_and_timeout_and_arguments_0(cleanup.unchecked_ref(), URL_LIFETIME_MS).is_err() {
            let _ = web_sys::Url::revoke_object_url(&url);
        }
        result
    }

    fn click_download(&self, filename: &str, url: &str) -> Result<(), JsValue> {
        let anchor: HtmlAnchorElement = self.document.create_element("a")?.dyn_into()?;
        anchor.set_href(url);
        anchor.set_download(filename);
        self.document.body().ok_or(JsValue::NULL)?.append_child(anchor.as_ref())?;
        anchor.click();
        anchor.remove();
        Ok(())
    }

    fn on_error(&self, error: Failure) {
        if error == Failure::Http(401) {
            self.expire();
            return;
        }
        self.status.set(Some(match error {
            Failure::Http(409) => "A newer version was saved",
            Failure::Http(507) => "Cloud storage is full",
            Failure::Transport => "Offline",
            _ => "Not available",
        }));
        self.render_safely();
    }

    fn report_error(&self, _error: &JsValue) {
        self.status.set(Some("Not available"));
        self.render_safely();
    }

    fn render_safely(&self) {
        if self.render().is_err() {
            log::warn!("The hosted workspace panel could not be rendered");
        }
    }

    fn render(&self) -> Result<(), JsValue> {
        let container = self.node("div", "craft-cloud-view", None)?;
        if let Some(status) = self.status.get() {
            let line = self.node("p", "craft-cloud-status", Some(label(status)))?;
            line.set_attribute("role", "status")?;
            container.append_child(line.as_ref())?;
        }
        let chooser = self.editor.borrow();
        if let Some(dialog) = chooser.choosers.front() {
            self.render_chooser(&container, dialog)?;
        } else if let Some(session) = self.session.borrow().as_ref() {
            self.render_session(&container, session)?;
        } else {
            let login = self.route.as_ref().map_or_else(
                || String::from(api::LOGIN),
                |route| {
                    let target = editor_link(route.app, route.project.as_deref());
                    format!("{}{}", api::LOGIN, target.trim_start_matches('/'))
                },
            );
            self.append_link(&container, "Sign in", &login, false)?;
        }
        self.panel.body().set_text_content(None);
        self.panel.body().append_child(container.as_ref())?;
        Ok(())
    }

    fn render_chooser(&self, parent: &HtmlElement, dialog: &SaveDialog) -> Result<(), JsValue> {
        let field = self.node("label", "craft-cloud-field", Some(label("Name")))?;
        field.set_attribute("for", "craft-cloud-save-name")?;
        parent.append_child(field.as_ref())?;
        let input: HtmlInputElement = self.document.create_element("input")?.dyn_into()?;
        input.set_id("craft-cloud-save-name");
        input.set_max_length(255);
        input.set_attribute("data-request", dialog.request.id())?;
        let previous =
            self.input("craft-cloud-save-name").filter(|input| input.get_attribute("data-request").as_deref() == Some(dialog.request.id()));
        input.set_value(&previous.map_or_else(|| dialog.filename.clone(), |input| input.value()));
        parent.append_child(input.as_ref())?;
        let actions = self.node("div", "craft-cloud-actions", None)?;
        self.append_button(&actions, "Save to cloud", "save-provider", None)?;
        self.append_button(&actions, "Save to device", "save-device", None)?;
        self.append_button(&actions, "Cancel", "save-cancel", None)?;
        parent.append_child(actions.as_ref())?;
        Ok(())
    }

    fn render_session(&self, parent: &HtmlElement, session: &Session) -> Result<(), JsValue> {
        let tabs = self.node("nav", "craft-cloud-actions", None)?;
        self.append_button(&tabs, "Files", "files", None)?;
        self.append_button(&tabs, "Account", "account", None)?;
        if session.admin {
            self.append_button(&tabs, "Updates", "updates", None)?;
        }
        parent.append_child(tabs.as_ref())?;
        match self.view.get() {
            View::Files => self.render_projects(parent),
            View::Account => {
                let account = self.node("p", "craft-cloud-account", Some(&session.label))?;
                parent.append_child(account.as_ref())?;
                self.append_button(parent, "Sign out", "logout", None)?;
                Ok(())
            }
            View::Updates => self.render_updates(parent),
        }
    }

    fn render_projects(&self, parent: &HtmlElement) -> Result<(), JsValue> {
        let rows = self.node("ul", "craft-cloud-files", None)?;
        for project in &*self.projects.borrow() {
            let row = self.node("li", "craft-cloud-file", None)?;
            let title = self.node("span", "craft-cloud-file-name", Some(&project.name))?;
            let app = self.node("span", "craft-cloud-file-app", Some(project.app.name()))?;
            row.append_child(title.as_ref())?;
            row.append_child(app.as_ref())?;
            if self.route.as_ref().is_some_and(|route| route.app == project.app)
                && self.editor.borrow().provider.as_ref().and_then(ProviderSession::connection).is_some()
            {
                self.append_button(&row, "Open", "open-project", Some(&project.id))?;
            } else if self.editors.borrow().iter().any(|entry| entry.app == project.app)
                && self.route.as_ref().is_none_or(|route| route.app != project.app)
            {
                self.append_link(&row, "Open", &editor_link(project.app, Some(&project.id)), true)?;
            } else {
                let unavailable = self.node("span", "craft-cloud-file-app", Some(label("Not available")))?;
                row.append_child(unavailable.as_ref())?;
            }
            self.append_link(&row, "Download", &project.download_url(), false)?;
            rows.append_child(row.as_ref())?;
        }
        if self.projects.borrow().is_empty() {
            let empty = self.node("p", "craft-cloud-empty", Some(label("No files yet")))?;
            parent.append_child(empty.as_ref())?;
        }
        parent.append_child(rows.as_ref())?;
        Ok(())
    }

    fn render_updates(&self, parent: &HtmlElement) -> Result<(), JsValue> {
        self.append_button(parent, "Check for updates", "refresh-updates", None)?;
        let fields = self.node("div", "craft-cloud-update-fields", None)?;
        let app: web_sys::HtmlSelectElement = self.document.create_element("select")?.dyn_into()?;
        app.set_id("craft-cloud-update-app");
        app.set_attribute("aria-label", label("App"))?;
        for id in [AppId::Photo, AppId::Word, AppId::Grid, AppId::Cad] {
            let option = self.node("option", "", Some(id.name()))?;
            option.set_attribute("value", id.slug())?;
            app.append_child(option.as_ref())?;
        }
        fields.append_child(app.as_ref())?;
        self.update_field(&fields, "Source commit", "craft-cloud-update-commit")?;
        self.update_field(&fields, "Source tree", "craft-cloud-update-tree")?;
        self.append_button(&fields, "Prepare version", "queue-update", None)?;
        parent.append_child(fields.as_ref())?;
        let jobs = self.node("ul", "craft-cloud-files", None)?;
        for job in &*self.jobs.borrow() {
            let row = self.node("li", "craft-cloud-file", None)?;
            let title = format!("{} · {}", job.app.name(), release_state(&job.state));
            row.append_child(self.node("span", "craft-cloud-file-name", Some(&title))?.as_ref())?;
            let source = self.node("code", "craft-cloud-file-app", job.source_commit.get(..12))?;
            source.set_attribute("title", &job.source_tree)?;
            row.append_child(source.as_ref())?;
            self.render_release_actions(&row, job)?;
            jobs.append_child(row.as_ref())?;
        }
        parent.append_child(jobs.as_ref())?;
        Ok(())
    }

    fn update_field(&self, parent: &HtmlElement, text: &str, id: &str) -> Result<(), JsValue> {
        let label_node = self.node("label", "craft-cloud-field", Some(label(text)))?;
        label_node.set_attribute("for", id)?;
        let input: HtmlInputElement = self.document.create_element("input")?.dyn_into()?;
        input.set_id(id);
        input.set_max_length(40);
        input.set_attribute("spellcheck", "false")?;
        if let Some(previous) = self.input(id) {
            input.set_value(&previous.value());
        }
        parent.append_child(label_node.as_ref())?;
        parent.append_child(input.as_ref())?;
        Ok(())
    }

    fn render_release_actions(&self, row: &HtmlElement, job: &api::Release) -> Result<(), JsValue> {
        if matches!(job.state.as_str(), "queued" | "claimed") {
            self.append_button(row, "Cancel", "cancel-update", Some(&job.id))?;
        }
        if job.state == "ready" {
            let active = self.editors.borrow().iter().any(|editor| editor.app == job.app && editor.build == job.id);
            if active && job.published {
                self.append_button(row, "Roll back", "rollback-update", Some(&job.id))?;
            } else {
                self.append_button(row, "Activate", "activate-update", Some(&job.id))?;
            }
        }
        Ok(())
    }

    fn refresh_updates(self: &Rc<Self>) {
        let state = Rc::clone(self);
        let generation = self.generation.get();
        spawn_local(async move {
            let result = api::releases().await;
            if !state.current(generation) {
                return;
            }
            match result {
                Ok(jobs) => {
                    state.jobs.replace(jobs);
                    state.render_safely();
                }
                Err(error) => state.on_error(error),
            }
        });
    }

    fn queue_update(self: &Rc<Self>) -> Result<(), JsValue> {
        let app = self
            .document
            .get_element_by_id("craft-cloud-update-app")
            .and_then(|element| element.dyn_into::<web_sys::HtmlSelectElement>().ok())
            .and_then(|input| AppId::from_slug(&input.value()))
            .ok_or(JsValue::NULL)?;
        let source = |id| {
            self.document
                .get_element_by_id(id)
                .and_then(|element| element.dyn_into::<HtmlInputElement>().ok())
                .map(|input| input.value().trim().to_ascii_lowercase())
                .filter(|value| value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or(JsValue::NULL)
        };
        let commit = source("craft-cloud-update-commit")?;
        let tree = source("craft-cloud-update-tree")?;
        self.run_update(move |session| async move { api::queue(&session, &api::Source { app, source_commit: &commit, source_tree: &tree }).await });
        Ok(())
    }

    fn release_action(self: &Rc<Self>, action: &str, id: Option<String>) -> Result<(), JsValue> {
        let id = id.ok_or(JsValue::NULL)?;
        let job = self.jobs.borrow().iter().find(|job| job.id == id).cloned().ok_or(JsValue::NULL)?;
        let operation = match action {
            "activate-update" => Some(api::ReleaseAction::Activate),
            "rollback-update" => Some(api::ReleaseAction::Rollback),
            _ => None,
        };
        self.run_update(move |session| async move {
            if let Some(operation) = operation {
                api::select_release(&session, job.app, &job.id, operation).await
            } else {
                api::cancel(&session, &job.id).await
            }
        });
        Ok(())
    }

    fn run_update<F, Fut>(self: &Rc<Self>, action: F)
    where
        F: FnOnce(Session) -> Fut + 'static,
        Fut: std::future::Future<Output = Result<api::Release, Failure>> + 'static,
    {
        let Some(session) = self.session.borrow().as_ref().filter(|session| session.admin).cloned() else { return };
        let state = Rc::clone(self);
        let generation = self.generation.get();
        spawn_local(async move {
            let result = action(session).await;
            if !state.current(generation) {
                return;
            }
            if let Err(error) = result {
                state.on_error(error);
                return;
            }
            if let Ok(editors) = api::editors().await
                && state.current(generation)
            {
                state.editors.replace(editors);
                state.status.set(None);
                state.refresh_updates();
            }
        });
    }

    fn input(&self, id: &str) -> Option<HtmlInputElement> {
        self.document.get_element_by_id(id)?.dyn_into().ok()
    }

    fn node(&self, tag: &str, class: &str, text: Option<&str>) -> Result<HtmlElement, JsValue> {
        let node: HtmlElement = self.document.create_element(tag)?.dyn_into()?;
        node.set_class_name(class);
        node.set_text_content(text);
        Ok(node)
    }

    fn append_button(&self, parent: &HtmlElement, text: &str, action: &str, id: Option<&str>) -> Result<(), JsValue> {
        let button = self.node("button", "craft-cloud-button", Some(label(text)))?;
        button.set_attribute("type", "button")?;
        button.set_attribute("data-workspace-action", action)?;
        if let Some(id) = id {
            button.set_attribute("data-id", id)?;
        }
        parent.append_child(button.as_ref())?;
        Ok(())
    }

    fn append_link(&self, parent: &HtmlElement, text: &str, href: &str, new_tab: bool) -> Result<(), JsValue> {
        let anchor: HtmlAnchorElement = self.node("a", "craft-cloud-button", Some(label(text)))?.dyn_into()?;
        anchor.set_href(href);
        if new_tab {
            anchor.set_target("_blank");
            anchor.set_rel("noopener");
        }
        parent.append_child(anchor.as_ref())?;
        Ok(())
    }

    fn on_click(self: &Rc<Self>, event: Event) -> Result<(), JsValue> {
        let target = event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok());
        let Some(target) = target.and_then(|target| target.closest("[data-workspace-action]").ok().flatten()) else { return Ok(()) };
        let Some(action) = target.get_attribute("data-workspace-action") else { return Ok(()) };
        match action.as_str() {
            "files" => self.select_view(View::Files),
            "account" => self.select_view(View::Account),
            "updates" => {
                self.select_view(View::Updates);
                self.refresh_updates();
            }
            "refresh-updates" => self.refresh_updates(),
            "queue-update" => self.queue_update()?,
            "activate-update" | "rollback-update" | "cancel-update" => {
                self.release_action(&action, target.get_attribute("data-id"))?;
            }
            "logout" => self.logout(),
            "save-provider" => self.resolve_chooser(SaveChoice::Provider)?,
            "save-device" => self.resolve_chooser(SaveChoice::Device)?,
            "save-cancel" => self.resolve_chooser(SaveChoice::Cancel)?,
            "open-project" => {
                if let Some(id) = target.get_attribute("data-id")
                    && let Some(provider) = &self.editor.borrow().provider
                {
                    provider.request_open(&id)?;
                    self.panel.close();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn select_view(&self, view: View) {
        self.view.set(view);
        self.render_safely();
    }

    fn logout(self: &Rc<Self>) {
        let Some(session) = self.session.borrow().clone() else { return };
        let state = Rc::clone(self);
        let generation = self.generation.get();
        spawn_local(async move {
            let result = api::logout(&session).await;
            if state.current(generation) {
                match result {
                    Ok(()) | Err(Failure::Http(401)) => state.expire(),
                    Err(error) => state.on_error(error),
                }
            }
        });
    }
}

fn label(key: &str) -> &str {
    i18n::tr(i18n::system_lang(), key)
}

fn release_state(state: &str) -> &str {
    label(match state {
        "queued" => "Queued",
        "claimed" => "Preparing",
        "ready" => "Ready",
        "cancelled" => "Cancelled",
        "failed" => "Failed",
        "interrupted" => "Interrupted",
        _ => "Not available",
    })
}
