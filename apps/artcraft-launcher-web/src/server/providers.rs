//! Same-origin protocol v1 bridge for an already authenticated Launcher editor frame.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use js_sys::{Array, ArrayBuffer, Function, JsString, Object, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{Event, Window};

pub(super) use crate::provider_protocol::Editor;
use crate::provider_protocol::{EventKind, MAX_FILENAME, MAX_ID, OpenOutcome, SaveOutcome, decimal_revision, valid_filename, valid_id, valid_text};

const MAX_BYTES: usize = 80 * 1024 * 1024;
const MAX_RESOURCES: usize = 100;
const MAX_EVENTS: usize = 64;

pub(super) struct Registration {
    pub id: String,
    pub label: String,
    pub namespace: String,
}

/// The opaque connection is also a generation token for asynchronous list requests.
#[derive(Clone, Debug)]
pub(super) struct Connection(Rc<String>);

impl Connection {
    pub(super) fn id(&self) -> &str {
        &self.0
    }
}

/// Only an event from this session grants an operation reply.
#[derive(Clone, Debug)]
pub(super) struct Request {
    id: String,
    connection: Connection,
}

impl Request {
    pub(super) fn id(&self) -> &str {
        &self.id
    }
}

pub(super) struct Resource {
    pub id: String,
    pub filename: String,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveIntent {
    Save,
    SaveAs,
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveTarget {
    Provider,
    Device,
}

#[derive(Clone, Copy)]
pub(super) enum SaveChoice {
    Provider,
    Device,
    Cancel,
}

impl SaveChoice {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Device => "device",
            Self::Cancel => "cancel",
        }
    }
}

pub(super) struct SaveDialog {
    pub request: Request,
    pub filename: String,
}

pub(super) struct OpenRequest {
    pub request: Request,
    pub resource_id: String,
}

pub(super) struct SaveCapture {
    pub request: Request,
    pub document_id: String,
    pub filename: String,
    /// For Save As and Save a Copy, the capture always needs a new resource.
    pub resource_id: Option<String>,
    pub target: SaveTarget,
    pub bytes: Uint8Array,
}

pub(super) enum SaveResult<'a> {
    /// Call only after the backend durably commits the bytes of this capture.
    Saved {
        resource_id: &'a str,
    },
    Downloaded,
    Cancelled,
}

pub(super) enum ProviderEvent {
    Connected(Connection),
    ChooseSave(SaveDialog),
    Open(OpenRequest),
    Save(SaveCapture),
    Opened { request: Request, resource_id: String, document_id: String },
    OpenFailed { request: Request },
    OpenCancelled { request: Request },
    SaveUnchanged,
    SaveFailed,
    BridgeError,
}

/// One authenticated account and one same-origin editor frame. Keep the session alive while mounted.
pub(super) struct ProviderSession {
    state: Rc<State>,
    listener: Closure<dyn FnMut(Event)>,
}

impl ProviderSession {
    /// Subscribe before registering. Direct discovery also handles a ready event sent before attach.
    pub(super) fn attach(
        window: Window,
        editor: Editor,
        registration: Registration,
        on_event: impl Fn(ProviderEvent) + 'static,
    ) -> Result<Self, JsValue> {
        // The document getter rejects a cross-origin WindowProxy, including sandboxed opaque origins.
        if !property(window.as_ref(), "document")?.is_object() {
            return Err(error("The editor must be in a same-origin frame."));
        }
        if !valid_id(editor, &registration.id) || !valid_id(editor, &registration.namespace) || !valid_text(&registration.label, 100, false) {
            return Err(error("Invalid file provider identity."));
        }
        let state = Rc::new(State {
            window,
            editor,
            registration,
            on_event: Box::new(on_event),
            connection: RefCell::new(None),
            pending: RefCell::new(HashMap::new()),
            queue: RefCell::new(EventQueue::default()),
            scheduled: Cell::new(false),
            stopped: Cell::new(false),
        });
        let weak = Rc::downgrade(&state);
        let listener = Closure::<dyn FnMut(Event)>::new(move |event| {
            let Some(state) = weak.upgrade() else { return };
            if state.receive(event).is_err() {
                let _ = state.enqueue(ProviderEvent::BridgeError);
            }
        });
        state.window.add_event_listener_with_callback(editor.names().1, listener.as_ref().unchecked_ref())?;
        let session = Self { state, listener };
        session.state.discover()?;
        Ok(session)
    }

    pub(super) fn connection(&self) -> Option<Connection> {
        self.state.current().ok().map(|connected| connected.connection)
    }

    /// Apply Recents only to the connection that initiated the list fetch.
    pub(super) fn set_resources(&self, connection: &Connection, resources: &[Resource]) -> Result<(), JsValue> {
        let connected = self.state.for_connection(connection)?;
        if resources.len() > MAX_RESOURCES {
            return Err(error("Too many provider resources."));
        }
        let rows = Array::new();
        let empty = Array::new();
        let mut ids = HashSet::new();
        for row in resources {
            if !valid_id(self.state.editor, &row.id)
                || !valid_filename(self.state.editor, &row.filename)
                || !valid_text(&row.updated_at, 100, true)
                || !ids.insert(&row.id)
            {
                return Err(error("Invalid or repeated provider resource."));
            }
            // The SDK also casts each entry to Object in its own realm.
            let entry = Reflect::construct(&connected.object_constructor, &empty)?;
            set(&entry, "id", &JsValue::from_str(&row.id))?;
            set(&entry, "name", &JsValue::from_str(&row.filename))?;
            set(&entry, "updatedAt", &JsValue::from_str(&row.updated_at))?;
            rows.push(&entry);
        }
        function(&connected.api, "setResources")?.call2(&connected.api, &JsValue::from_str(connection.id()), rows.as_ref())?;
        Ok(())
    }

    /// Start the normal editor open flow. Respond to the resulting Open event.
    pub(super) fn request_open(&self, resource_id: &str) -> Result<String, JsValue> {
        if !valid_id(self.state.editor, resource_id) {
            return Err(error("Invalid provider resource ID."));
        }
        let connected = self.state.current()?;
        let id = function(&connected.api, "requestOpen")?.call1(&connected.api, &JsValue::from_str(resource_id))?;
        parse_id(self.state.editor, id)
    }

    /// WordCraft and GridCraft handle Device internally. PhotoCraft and CADCraft issue a Device capture.
    pub(super) fn resolve_save(&self, request: &Request, choice: SaveChoice, filename: &str) -> Result<(), JsValue> {
        let connected = self.state.for_request(request, |pending| matches!(pending, Pending::Chooser))?;
        let filename = if matches!(choice, SaveChoice::Cancel) {
            ""
        } else if valid_filename(self.state.editor, filename) {
            filename
        } else {
            return Err(error("Invalid provider filename."));
        };
        function(&connected.api, "resolveSaveDialog")?.call3(
            &connected.api,
            &JsValue::from_str(request.id()),
            &JsValue::from_str(choice.as_str()),
            &JsValue::from_str(filename),
        )?;
        self.state.pending.borrow_mut().remove(request.id());
        Ok(())
    }

    /// Supply native bytes. An Opened event confirms the editor actually admitted the document.
    pub(super) fn open_resource(&self, request: &Request, filename: &str, bytes: &Uint8Array) -> Result<(), JsValue> {
        let connected = self.state.for_request(request, Pending::can_answer_open)?;
        if !valid_filename(self.state.editor, filename) || bytes.length() == 0 || bytes.length() as usize > MAX_BYTES {
            return Err(error("Invalid native document name or size."));
        }
        // The editor checks instanceof Uint8Array in its own realm.
        let args = Array::new();
        args.push(&JsValue::from_f64(f64::from(bytes.length())));
        let array = editor_byte_array(Reflect::construct(&connected.byte_constructor, &args)?, &connected.byte_prototype)?;
        if array.length() != bytes.length() {
            return Err(error("The editor returned an invalid byte buffer."));
        }
        array.set(bytes, 0);
        function(&connected.api, "openResource")?.call3(
            &connected.api,
            &JsValue::from_str(request.id()),
            &JsValue::from_str(filename),
            array.as_ref(),
        )?;
        self.state.mark_open_answered(request);
        Ok(())
    }

    /// A blank message cancels a read. WordCraft and GridCraft report this as an empty openFailed.
    pub(super) fn cancel_open(&self, request: &Request, message: &str) -> Result<(), JsValue> {
        let connected = self.state.for_request(request, Pending::can_answer_open)?;
        if !valid_text(message, 1024, true) {
            return Err(error("Invalid open failure message."));
        }
        function(&connected.api, "cancelOpen")?.call2(&connected.api, &JsValue::from_str(request.id()), &JsValue::from_str(message))?;
        self.state.mark_open_answered(request);
        Ok(())
    }

    /// Reply to the exact live capture owned by this connection.
    pub(super) fn save_result(&self, request: &Request, result: SaveResult<'_>) -> Result<(), JsValue> {
        let (status, resource, required_target) = match result {
            SaveResult::Saved { resource_id } if valid_id(self.state.editor, resource_id) => ("saved", resource_id, Some(SaveTarget::Provider)),
            SaveResult::Saved { .. } => return Err(error("Invalid saved resource ID.")),
            SaveResult::Downloaded => ("download", "", Some(SaveTarget::Device)),
            SaveResult::Cancelled => ("cancel", "", None),
        };
        let connected = self
            .state
            .for_request(request, |pending| matches!(pending, Pending::Save(target) if required_target.is_none_or(|required| required == *target)))?;
        function(&connected.api, "saveResult")?.call3(
            &connected.api,
            &JsValue::from_str(request.id()),
            &JsValue::from_str(status),
            &JsValue::from_str(resource),
        )?;
        self.state.pending.borrow_mut().remove(request.id());
        Ok(())
    }

    /// Stop new hosted operations while retaining the editor and its local documents.
    pub(super) fn suspend(&self) -> Result<(), JsValue> {
        let connected = self.state.current()?;
        function(&connected.api, "setEnabled")?.call1(&connected.api, &JsValue::FALSE)?;
        let pending: Vec<_> = self
            .state
            .pending
            .borrow()
            .iter()
            .filter_map(|(id, item)| {
                let action = match item {
                    Pending::Chooser => Suspension::Chooser,
                    Pending::Open { answered: false, .. } => Suspension::Open,
                    Pending::Open { answered: true, .. } => return None,
                    Pending::Save(_) => Suspension::Save,
                };
                Some((Request { id: id.clone(), connection: connected.connection.clone() }, action))
            })
            .collect();
        for (request, action) in pending {
            let _ = match action {
                Suspension::Chooser => self.resolve_save(&request, SaveChoice::Cancel, ""),
                Suspension::Open => self.cancel_open(&request, ""),
                Suspension::Save => self.save_result(&request, SaveResult::Cancelled),
            };
        }
        Ok(())
    }

    /// Call outside the editor's dispatch batch. If the SDK reports pending work, keep the
    /// session to settle it or discard its frame before mounting another account.
    pub(super) fn disconnect(&self) -> Result<(), JsValue> {
        self.state.unregister()?;
        self.stop_listening();
        Ok(())
    }

    fn stop_listening(&self) {
        if !self.state.stopped.replace(true) {
            self.state.pending.borrow_mut().clear();
            self.state.queue.replace(EventQueue::default());
            let _ = self.state.window.remove_event_listener_with_callback(self.state.editor.names().1, self.listener.as_ref().unchecked_ref());
        }
    }
}

impl Drop for ProviderSession {
    fn drop(&mut self) {
        if !self.state.stopped.get() {
            let _ = self.state.unregister();
            self.stop_listening();
        }
    }
}

#[derive(Clone)]
struct Connected {
    api: JsValue,
    connection: Connection,
    object_constructor: Function,
    byte_constructor: Function,
    byte_prototype: JsValue,
}

enum Suspension {
    Chooser,
    Open,
    Save,
}

enum Pending {
    Chooser,
    Open { resource_id: String, answered: bool },
    Save(SaveTarget),
}

impl Pending {
    fn can_answer_open(&self) -> bool {
        matches!(self, Self::Open { answered: false, .. })
    }
}

#[derive(Default)]
struct EventQueue {
    events: Vec<ProviderEvent>,
    bytes: usize,
}

struct State {
    window: Window,
    editor: Editor,
    registration: Registration,
    on_event: Box<dyn Fn(ProviderEvent)>,
    connection: RefCell<Option<Connected>>,
    pending: RefCell<HashMap<String, Pending>>,
    queue: RefCell<EventQueue>,
    scheduled: Cell<bool>,
    stopped: Cell<bool>,
}

impl State {
    fn discover(self: &Rc<Self>) -> Result<(), JsValue> {
        if self.stopped.get() || self.connection.borrow().is_some() {
            return Ok(());
        }
        let api = property(self.window.as_ref(), self.editor.names().0)?;
        if api.is_undefined() {
            return Ok(());
        }
        if !api.is_object()
            || property(&api, "protocolVersion")?.as_f64() != Some(1.0)
            || bounded_string(property(&api, "app")?, 16)? != self.editor.id()
        {
            return Err(error("Unsupported editor file provider API."));
        }
        let object_constructor = function(self.window.as_ref(), "Object")?;
        let byte_constructor = function(self.window.as_ref(), "Uint8Array")?;
        let byte_prototype = property(byte_constructor.as_ref(), "prototype")?;
        let result = function(&api, "registerProvider")?.call3(
            &api,
            &JsValue::from_str(&self.registration.id),
            &JsValue::from_str(&self.registration.label),
            &JsValue::from_str(&self.registration.namespace),
        )?;
        let connection = Connection(Rc::new(parse_id(self.editor, result)?));
        self.connection.replace(Some(Connected { api, connection: connection.clone(), object_constructor, byte_constructor, byte_prototype }));
        self.enqueue(ProviderEvent::Connected(connection))
    }

    fn current(&self) -> Result<Connected, JsValue> {
        let connection = self.connection.borrow().clone().ok_or_else(|| error("Editor provider is disconnected."))?;
        if self.stopped.get() || !Object::is(&property(self.window.as_ref(), self.editor.names().0)?, &connection.api) {
            return Err(error("The editor frame changed."));
        }
        Ok(connection)
    }

    fn for_connection(&self, connection: &Connection) -> Result<Connected, JsValue> {
        let current = self.current()?;
        if !Rc::ptr_eq(&current.connection.0, &connection.0) {
            return Err(error("Stale editor provider connection."));
        }
        Ok(current)
    }

    fn for_request(&self, request: &Request, matches: impl FnOnce(&Pending) -> bool) -> Result<Connected, JsValue> {
        let current = self.for_connection(&request.connection)?;
        if !self.pending.borrow().get(request.id()).is_some_and(matches) {
            return Err(error("Unknown or completed editor provider request."));
        }
        Ok(current)
    }

    fn unregister(&self) -> Result<(), JsValue> {
        if let Ok(connected) = self.current() {
            let result = function(&connected.api, "unregisterProvider")?.call0(&connected.api)?;
            if result.as_bool() != Some(true) {
                return Err(error("The editor refused to unregister its provider."));
            }
        }
        self.connection.replace(None);
        Ok(())
    }

    fn mark_open_answered(&self, request: &Request) {
        if let Some(Pending::Open { answered, .. }) = self.pending.borrow_mut().get_mut(request.id()) {
            *answered = true;
        }
    }

    fn remember(&self, request: &Request, pending: Pending) -> Result<(), JsValue> {
        let mut requests = self.pending.borrow_mut();
        if requests.len() >= MAX_EVENTS || requests.contains_key(request.id()) {
            return Err(error("Too many or repeated editor provider requests."));
        }
        requests.insert(request.id.clone(), pending);
        Ok(())
    }

    fn receive(self: &Rc<Self>, event: Event) -> Result<(), JsValue> {
        if self.stopped.get() || !event.target().is_some_and(|target| Object::is(target.as_ref(), self.window.as_ref())) {
            return Ok(());
        }
        // A CustomEvent constructed in the child has a different prototype from the parent.
        let detail = property(event.as_ref(), "detail")?;
        if !detail.is_object() {
            return Err(error("Invalid provider event detail."));
        }
        let kind = bounded_string(property(&detail, "type")?, 32)?;
        if kind == "ready" {
            if property(&detail, "protocolVersion")?.as_f64() != Some(1.0) {
                return Err(error("Unsupported editor file provider protocol."));
            }
            return self.discover();
        }
        let Ok(connected) = self.current() else { return Ok(()) };
        if read_id(self.editor, &detail, "connectionId")? != connected.connection.id()
            || read_id(self.editor, &detail, "providerId")? != self.registration.id
            || read_id(self.editor, &detail, "namespace")? != self.registration.namespace
        {
            return Ok(());
        }
        if self.queue.borrow().events.len() >= MAX_EVENTS {
            return Err(error("Too many queued editor provider events."));
        }
        if let Some(event) = self.parse_event(&kind, &detail, &connected)? {
            self.enqueue(event)?;
        }
        Ok(())
    }

    fn parse_event(&self, kind: &str, detail: &JsValue, connected: &Connected) -> Result<Option<ProviderEvent>, JsValue> {
        let Some(kind) = EventKind::parse(self.editor, kind) else { return Ok(None) };
        let request = Request { id: read_id(self.editor, detail, "requestId")?, connection: connected.connection.clone() };
        let event = match kind {
            EventKind::ChooseSave => {
                let dialog = parse_dialog(self.editor, detail, request)?;
                self.remember(&dialog.request, Pending::Chooser)?;
                ProviderEvent::ChooseSave(dialog)
            }
            EventKind::Save => {
                let capture = self.parse_capture(detail, request, &connected.byte_prototype)?;
                self.remember(&capture.request, Pending::Save(capture.target))?;
                ProviderEvent::Save(capture)
            }
            EventKind::Open => self.parse_open(detail, request)?,
            EventKind::OpenFinished(outcome) => return self.parse_open_terminal(outcome, detail, request),
            EventKind::SaveUnchanged => {
                read_id(self.editor, detail, "resourceId")?;
                read_id(self.editor, detail, "documentId")?;
                revision(self.editor, property(detail, "revision")?)?;
                read_filename(self.editor, detail)?;
                ProviderEvent::SaveUnchanged
            }
            EventKind::SaveFinished(outcome) => {
                let _ = optional_id(self.editor, detail, "resourceId")?;
                let _ = optional_message(detail)?;
                let mut pending = self.pending.borrow_mut();
                if matches!(pending.get(request.id()), Some(Pending::Save(_) | Pending::Chooser)) {
                    pending.remove(request.id());
                }
                return Ok((outcome == SaveOutcome::Failed).then_some(ProviderEvent::SaveFailed));
            }
        };
        Ok(Some(event))
    }

    fn parse_open(&self, detail: &JsValue, request: Request) -> Result<ProviderEvent, JsValue> {
        let resource_id = read_id(self.editor, detail, "resourceId")?;
        let filename = optional_string(detail, "filename", MAX_FILENAME)?;
        if filename.as_deref().is_some_and(|name| !valid_filename(self.editor, name)) {
            return Err(error("Invalid provider open filename."));
        }
        self.remember(&request, Pending::Open { resource_id: resource_id.clone(), answered: false })?;
        Ok(ProviderEvent::Open(OpenRequest { request, resource_id }))
    }

    fn parse_open_terminal(&self, outcome: OpenOutcome, detail: &JsValue, request: Request) -> Result<Option<ProviderEvent>, JsValue> {
        let resource_id = read_id(self.editor, detail, "resourceId")?;
        if !matches!(self.pending.borrow().get(request.id()), Some(Pending::Open { resource_id: expected, .. }) if expected == &resource_id) {
            return Ok(None);
        }
        let request_id = request.id.clone();
        let event = match outcome {
            OpenOutcome::Opened => ProviderEvent::Opened { request, resource_id, document_id: read_id(self.editor, detail, "documentId")? },
            OpenOutcome::Cancelled => ProviderEvent::OpenCancelled { request },
            OpenOutcome::Failed => {
                let _ = optional_message(detail)?;
                ProviderEvent::OpenFailed { request }
            }
        };
        self.pending.borrow_mut().remove(&request_id);
        Ok(Some(event))
    }

    fn parse_capture(&self, detail: &JsValue, request: Request, byte_prototype: &JsValue) -> Result<SaveCapture, JsValue> {
        let document_id = read_id(self.editor, detail, "documentId")?;
        revision(self.editor, property(detail, "revision")?)?;
        let filename = read_filename(self.editor, detail)?;
        let mut resource_id = optional_id(self.editor, detail, "resourceId")?;
        let destination = bounded_string(property(detail, "destination")?, 8)?;
        let target = match destination.as_str() {
            "provider" => SaveTarget::Provider,
            "device" if !self.editor.editor_download() => SaveTarget::Device,
            _ => return Err(error("Invalid provider capture destination.")),
        };
        // WordCraft and GridCraft v1 send saveAs on the dialog and omit resourceId
        // on Save As captures. Their captures do not have an intent field.
        if !self.editor.editor_download() && parse_intent(property(detail, "intent")?)? != SaveIntent::Save {
            resource_id = None;
        }
        let declared = property(detail, "bytesLength")?;
        let array = editor_byte_array(property(detail, "bytes")?, byte_prototype)?;
        let length = array.length() as usize;
        if length == 0 || length > MAX_BYTES.saturating_sub(self.queue.borrow().bytes) {
            return Err(error("Provider capture exceeds the pending byte limit."));
        }
        if !declared.is_undefined() && declared.as_f64() != Some(length as f64) {
            return Err(error("Provider capture length does not match its bytes."));
        }
        // Copy synchronously into the host realm while dispatch owns the editor's array.
        let bytes = Uint8Array::new_with_length(array.length());
        bytes.set(&array, 0);
        Ok(SaveCapture { request, document_id, filename, resource_id, target, bytes })
    }

    fn enqueue(self: &Rc<Self>, event: ProviderEvent) -> Result<(), JsValue> {
        let mut queue = self.queue.borrow_mut();
        if queue.events.len() >= MAX_EVENTS {
            return Err(error("Too many queued editor provider events."));
        }
        if let ProviderEvent::Save(capture) = &event {
            queue.bytes += capture.bytes.length() as usize;
        }
        queue.events.push(event);
        drop(queue);
        if !self.scheduled.replace(true) {
            let weak = Rc::downgrade(self);
            wasm_bindgen_futures::spawn_local(async move {
                let Some(state) = weak.upgrade() else { return };
                state.scheduled.set(false);
                let events = std::mem::take(&mut *state.queue.borrow_mut()).events;
                // Run host callbacks after the SDK's entire synchronous publication batch.
                for event in events {
                    if state.stopped.get() {
                        break;
                    }
                    if matches!(event, ProviderEvent::BridgeError) || state.current().is_ok() {
                        (state.on_event)(event);
                    }
                }
            });
        }
        Ok(())
    }
}

fn editor_byte_array(value: JsValue, prototype: &JsValue) -> Result<Uint8Array, JsValue> {
    if !ArrayBuffer::is_view(&value) || !Object::is(Reflect::get_prototype_of(&value)?.as_ref(), prototype) {
        return Err(error("Expected a Uint8Array from the editor frame."));
    }
    Ok(value.unchecked_into())
}

fn parse_dialog(editor: Editor, detail: &JsValue, request: Request) -> Result<SaveDialog, JsValue> {
    if editor.editor_download() {
        if property(detail, "saveAs")?.as_bool().is_none() {
            return Err(error("Invalid editor save intent."));
        }
    } else {
        parse_intent(property(detail, "intent")?)?;
    }
    let raw_revision = property(detail, "revision")?;
    if editor != Editor::Photo || !raw_revision.is_undefined() {
        revision(editor, raw_revision)?;
    }
    let document_id = optional_id(editor, detail, "documentId")?;
    if editor != Editor::Photo && document_id.is_none() {
        return Err(error("Missing editor document ID."));
    }
    let _ = optional_id(editor, detail, "resourceId")?;
    Ok(SaveDialog { request, filename: read_filename(editor, detail)? })
}

fn parse_intent(value: JsValue) -> Result<SaveIntent, JsValue> {
    match bounded_string(value, 8)?.as_str() {
        "save" => Ok(SaveIntent::Save),
        "saveAs" => Ok(SaveIntent::SaveAs),
        "copy" => Ok(SaveIntent::Copy),
        _ => Err(error("Invalid editor save intent.")),
    }
}

fn revision(editor: Editor, value: JsValue) -> Result<u64, JsValue> {
    if editor.editor_download() {
        let number = value.as_f64().ok_or_else(|| error("Invalid numeric editor revision."))?;
        if (0.0..=9_007_199_254_740_991.0).contains(&number) && number.fract() == 0.0 {
            return Ok(number as u64);
        }
    } else if let Some(number) = decimal_revision(&bounded_string(value, 20)?) {
        return Ok(number);
    }
    Err(error("Invalid editor revision."))
}

fn read_filename(editor: Editor, object: &JsValue) -> Result<String, JsValue> {
    let field = if editor.editor_download() { "name" } else { "filename" };
    let filename = bounded_string(property(object, field)?, MAX_FILENAME)?;
    if !valid_filename(editor, &filename) {
        return Err(error("Invalid provider filename."));
    }
    Ok(filename)
}

fn bounded_string(value: JsValue, max: usize) -> Result<String, JsValue> {
    let text = read_text(value, max)?;
    if text.chars().any(char::is_control) {
        return Err(error("Invalid provider text."));
    }
    Ok(text)
}

fn read_text(value: JsValue, max: usize) -> Result<String, JsValue> {
    let js = value.dyn_into::<JsString>().map_err(|_| error("Expected provider text."))?;
    if js.length() as usize > max {
        return Err(error("Provider text exceeds its limit."));
    }
    let text = js.as_string().ok_or_else(|| error("Invalid provider text."))?;
    if text.len() > max {
        return Err(error("Provider text exceeds its limit."));
    }
    Ok(text)
}

fn optional_message(detail: &JsValue) -> Result<Option<String>, JsValue> {
    let value = property(detail, "error")?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    let message = read_text(value, 1024)?.replace(char::is_control, " ");
    Ok((!message.trim().is_empty()).then_some(message))
}

fn optional_string(object: &JsValue, key: &str, max: usize) -> Result<Option<String>, JsValue> {
    let value = property(object, key)?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    bounded_string(value, max).map(|value| (!value.is_empty()).then_some(value))
}

fn parse_id(editor: Editor, value: JsValue) -> Result<String, JsValue> {
    let id = bounded_string(value, MAX_ID)?;
    if !valid_id(editor, &id) {
        return Err(error("Invalid provider identifier."));
    }
    Ok(id)
}

fn read_id(editor: Editor, object: &JsValue, key: &str) -> Result<String, JsValue> {
    parse_id(editor, property(object, key)?)
}

fn optional_id(editor: Editor, object: &JsValue, key: &str) -> Result<Option<String>, JsValue> {
    let id = optional_string(object, key, MAX_ID)?;
    if id.as_deref().is_some_and(|id| !valid_id(editor, id)) {
        return Err(error("Invalid provider identifier."));
    }
    Ok(id)
}

fn property(object: &JsValue, key: &str) -> Result<JsValue, JsValue> {
    Reflect::get(object, &JsValue::from_str(key))
}

fn set(object: &JsValue, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if !Reflect::set(object, &JsValue::from_str(key), value)? {
        return Err(error("Unable to set provider resource property."));
    }
    Ok(())
}

fn function(object: &JsValue, key: &str) -> Result<Function, JsValue> {
    property(object, key)?.dyn_into().map_err(|_| error("Missing editor provider method."))
}

fn error(message: &str) -> JsValue {
    JsValue::from_str(message)
}
