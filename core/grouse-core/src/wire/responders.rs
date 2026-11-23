// SPDX-License-Identifier: AGPL-3.0-or-later

//! Process-wide response registries: parked server requests (recipe params /
//! elicitation), permission owners keyed by tool call id, and the two listener
//! tables. The parked keys and duplicate rejection rules are moved verbatim.

use super::*;

// ---------------------------------------------------------------------------
// Parked server-request responders (recipe-params / elicitation)
// ---------------------------------------------------------------------------

/// Which parked server request a [`respond_*`](crate::GrouseUnstable) intent
/// answers. The two families are separate namespaces in the parked map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingRequest {
    /// `_goose/unstable/session/recipe/request-params`
    RecipeParams,
    /// `elicitation/create`
    Elicitation,
}

impl PendingRequest {
    fn prefix(self) -> &'static str {
        match self {
            Self::RecipeParams => "recipe",
            Self::Elicitation => "elicit",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::RecipeParams => "recipe/request-params",
            Self::Elicitation => "elicitation/create",
        }
    }
}

struct ParkedRequest {
    /// Which wire asked. Carried to the listener so the UI never assumes the
    /// form on screen belongs to the chat currently displayed.
    owner: SessionOwner,
    responder: Responder<Value>,
}

static PARKED: std::sync::OnceLock<Mutex<HashMap<(PendingRequest, String), ParkedRequest>>> =
    std::sync::OnceLock::new();

fn parked() -> &'static Mutex<HashMap<(PendingRequest, String), ParkedRequest>> {
    PARKED.get_or_init(|| Mutex::new(HashMap::new()))
}
static NEXT_REQUEST_SEQ: AtomicU64 = AtomicU64::new(1);

/// The reply key for one server request. Prefer an id the server actually put
/// in the params; otherwise mint one that is unique across wires and restarts.
/// Either way it is namespaced by the owning wire: two peers can hand out the
/// same server id without one superseding the other's form. Clients treat the
/// result as an opaque string and hand it straight back.
pub(crate) fn request_id_from_params(kind: PendingRequest, params: &Value, owner: &SessionOwner) -> String {
    const ID_KEYS: [&str; 3] = ["requestId", "request_id", "id"];
    let supplied = ID_KEYS
        .iter()
        .find_map(|key| params.get(key))
        .or_else(|| {
            params
                .get("_meta")
                .and_then(|meta| ID_KEYS.iter().find_map(|key| meta.get(key)))
        });
    let wire = if owner.kind == "roam" {
        owner.label.as_str()
    } else {
        "main"
    };
    let base = match supplied {
        Some(Value::String(id)) if !id.is_empty() => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            format!(
                "{}:{}:{}",
                kind.prefix(),
                nanos,
                NEXT_REQUEST_SEQ.fetch_add(1, Ordering::Relaxed)
            )
        }
    };
    format!("{wire}:{base}")
}

/// Owner inferred from a response's params when the calling transport has no
/// owner-aware seam yet (roam peers call the legacy four-argument parker).
/// Roam wire params carry bare ids, so this is only a best-effort label; the
/// reply still routes by the exact request id.
fn owner_from_params(params: &Value) -> SessionOwner {
    let session = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .or_else(|| params.get("session"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(rest) = session.strip_prefix("roam:") {
        let label = rest.split(':').next().unwrap_or("");
        if !label.is_empty() {
            return SessionOwner { kind: "roam".into(), label: label.into() };
        }
    }
    SessionOwner { kind: "main".into(), label: String::new() }
}

/// Answer exactly one parked server request. An unknown or already-used id
/// returns `false` and leaves every other responder untouched.
pub fn answer_request(kind: PendingRequest, request_id: &str, result: Value) -> bool {
    match parked().lock().remove(&(kind, request_id.to_string())) {
        Some(ParkedRequest { responder, .. }) => responder.respond(result).is_ok(),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Permission responders (keyed by tool call, not by whichever peer is active)
// ---------------------------------------------------------------------------

/// How a connection answers one parked `session/request_permission`. The
/// closure captures its owning wire (a `Conn` here, a roam peer there), so the
/// core can route an answer by `tool_call_id` even when the displayed chat is
/// owned by a different connection.
pub(crate) type PermissionAnswer =
    Arc<dyn Fn(&str, PermissionOutcome) -> Result<(), AcpError> + Send + Sync>;
/// How a rejected duplicate tells ITS server there will be no answer. The
/// first registration's responder is never touched.
pub(crate) type PermissionReject = Arc<dyn Fn(AcpError) + Send + Sync>;

/// The SDK responder lives behind a shared slot: the wire's local map and the
/// global owner's answer/duplicate hooks all need to reach the same reply.
pub(crate) type ParkedPermission = Arc<Mutex<Option<Responder<RequestPermissionResponse>>>>;

struct RegisteredPermission {
    owner: String,
    answer: PermissionAnswer,
}

static PERMISSION_OWNERS: std::sync::OnceLock<Mutex<HashMap<String, RegisteredPermission>>> =
    std::sync::OnceLock::new();

fn permission_owners() -> &'static Mutex<HashMap<String, RegisteredPermission>> {
    PERMISSION_OWNERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Advisory alerts (never lifecycle status) from the global responder tables.
/// Installed by every `Conn` with its listener; the callback is cloned out of
/// the lock so a listener that alerts back cannot deadlock us.
static ALERT_HOOK: Mutex<Option<Arc<dyn Fn(String) + Send + Sync>>> = Mutex::new(None);

pub(crate) fn install_alert_hook(hook: Arc<dyn Fn(String) + Send + Sync>) {
    *ALERT_HOOK.lock() = Some(hook);
}

fn alert(message: impl AsRef<str>) {
    let hook = ALERT_HOOK.lock().clone();
    if let Some(hook) = hook {
        hook(message.as_ref().to_string());
    }
}

/// Register the owner of a permission prompt with an explicit wire identity.
/// Returns `false` when a DIFFERENT live request already owns that tool call
/// id: the duplicate's `reject` hook is called (so its server gets an error),
/// an alert is raised, and the first registration is kept. A tool call id is
/// chosen by the server, so two wires can both send one — the UI must never be
/// handed a prompt whose answer would go to the other wire.
pub(crate) fn register_permission_answer_for(
    tool_call_id: String,
    owner: &str,
    answer: PermissionAnswer,
    reject: Option<PermissionReject>,
) -> bool {
    let mut owners = permission_owners().lock();
    if let Some(first) = owners.get(&tool_call_id) {
        let message = format!(
            "duplicate permission request {tool_call_id} from {owner} (already owned by {})",
            first.owner
        );
        drop(owners);
        if let Some(reject) = reject {
            reject(AcpError::internal_error().data(message.clone()));
        }
        alert(message);
        return false;
    }
    owners.insert(tool_call_id, RegisteredPermission { owner: owner.to_string(), answer });
    true
}

/// Legacy two-argument seam (roam peers call it today). Each call gets a
/// distinct owner token, so two wires can never silently overwrite each
/// other's answer closure; the caller should move to
/// [`register_permission_answer_for`] to give its duplicate responder an error
/// and its alert a real label.
pub(crate) fn register_permission_answer(tool_call_id: String, answer: PermissionAnswer) {
    static LEGACY_SEQ: AtomicU64 = AtomicU64::new(1);
    let owner = format!("legacy:{}", LEGACY_SEQ.fetch_add(1, Ordering::Relaxed));
    let _ = register_permission_answer_for(tool_call_id, &owner, answer, None);
}

/// Drop the owner registration (answer already sent, prompt cancelled, or wire
/// closed). Safe to call when no entry exists.
pub(crate) fn unregister_permission_answer(tool_call_id: &str) {
    permission_owners().lock().remove(tool_call_id);
}

/// Drop only OUR entry: a superseded/closed wire must never cancel a live
/// prompt another connection owns just because the tool call ids collide.
pub(crate) fn unregister_permission_answer_owned(tool_call_id: &str, owner: &str) {
    let mut owners = permission_owners().lock();
    if owners
        .get(tool_call_id)
        .map(|entry| entry.owner == owner)
        .unwrap_or(false)
    {
        owners.remove(tool_call_id);
    }
}

/// Answer a permission prompt no matter WHICH connection owns it.
///
/// `Core::respond_permission` used to route by `active_peer()`: a prompt from
/// the main wire could not be answered while a roam chat was displayed, and the
/// answer was silently discarded. The key is the tool call id the UI shows.
pub(crate) fn respond_permission(
    tool_call_id: &str,
    outcome: PermissionOutcome,
) -> Result<(), AcpError> {
    let owner = permission_owners()
        .lock()
        .remove(tool_call_id)
        .ok_or_else(|| AcpError::internal_error().data("no pending permission request"))?;
    (owner.answer)(tool_call_id, outcome)
}

/// A registered server-request listener (the unstable shim registers one at
/// construction so the UI can opt into answering recipe/elicitation requests).
/// While at least one listener is active the spine parks the request and fans
/// it out; with none it auto-answers (desktop parity).
pub struct ServerRequestListenerGuard {
    entry: Arc<ServerRequestListener>,
}

type ServerRequestFn = Arc<dyn Fn(&str, SessionOwner, String, Value) + Send + Sync>;

struct ServerRequestListener {
    active: AtomicBool,
    f: ServerRequestFn,
}

impl Drop for ServerRequestListenerGuard {
    fn drop(&mut self) {
        self.entry.active.store(false, Ordering::SeqCst);
    }
}

/// Register a listener for server→client requests. The spine calls
/// `f(method, owner, request_id, params)` for every parked request: the owner
/// names the asking wire and the id is the ONLY key that answers this exact
/// request. The guard deregisters on drop. `Send + Sync` so it can sit in a
/// uniffi object field.
pub fn register_server_request_listener(
    f: Arc<dyn Fn(&str, SessionOwner, String, Value) + Send + Sync + 'static>,
) -> ServerRequestListenerGuard {
    let entry = Arc::new(ServerRequestListener {
        active: AtomicBool::new(true),
        f,
    });
    SERVER_REQUEST_LISTENERS.lock().push(entry.clone());
    ServerRequestListenerGuard { entry }
}

static SERVER_REQUEST_LISTENERS: Mutex<Vec<Arc<ServerRequestListener>>> = Mutex::new(Vec::new());

fn server_request_listeners() -> Vec<ServerRequestFn> {
    SERVER_REQUEST_LISTENERS
        .lock()
        .iter()
        .filter(|entry| entry.active.load(Ordering::SeqCst))
        .map(|entry| entry.f.clone())
        .collect()
}

/// A registered notification listener (the unstable shim registers one for
/// the goose-custom `_goose/unstable/session/update` notifications:
/// `status_message` / `message_usage`). Same guard pattern as
/// [`register_server_request_listener`].
pub struct NotificationListenerGuard {
    entry: Arc<NotificationListener>,
}

struct NotificationListener {
    active: AtomicBool,
    f: Arc<dyn Fn(&str, Value) + Send + Sync>,
}

impl Drop for NotificationListenerGuard {
    fn drop(&mut self) {
        self.entry.active.store(false, Ordering::SeqCst);
    }
}

/// Register a listener for notifications the spine does not itself dispatch
/// (custom `_goose/...` methods). The spine calls `f(method, params)` for every
/// untyped notification after its own `session/update` dispatch.
pub fn register_notification_listener(
    f: Arc<dyn Fn(&str, Value) + Send + Sync + 'static>,
) -> NotificationListenerGuard {
    let entry = Arc::new(NotificationListener {
        active: AtomicBool::new(true),
        f,
    });
    NOTIFICATION_LISTENERS.lock().push(entry.clone());
    NotificationListenerGuard { entry }
}

static NOTIFICATION_LISTENERS: Mutex<Vec<Arc<NotificationListener>>> = Mutex::new(Vec::new());

pub(super) fn notify_listeners(method: &str, params: Value) {
    for f in NOTIFICATION_LISTENERS
        .lock()
        .iter()
        .filter(|entry| entry.active.load(Ordering::SeqCst))
        .map(|entry| entry.f.clone())
    {
        f(method, params.clone());
    }
}

/// Fan a goose-custom notification out to registered slices. Called by the
/// shared client builder's untyped handler on EVERY transport.
pub(crate) fn dispatch_custom_notification(method: &str, params: Value) {
    notify_listeners(method, params);
}

impl Conn {
    /// Answer this wire's parked prompt without touching the global registry.
    /// The owner closure registered for main-wire prompts calls this, so a
    /// reply cannot remove another wire's registration with the same id.
    pub(crate) fn respond_permission_local(
        &self,
        tool_call_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpError> {
        self.inner.permission_ids.lock().remove(tool_call_id);
        let cell = self
            .inner
            .permission
            .lock()
            .remove(tool_call_id)
            .ok_or_else(|| AcpError::internal_error().data("no pending permission request"))?;
        let responder = cell
            .lock()
            .take()
            .ok_or_else(|| AcpError::internal_error().data("permission already answered"))?;
        responder.respond(crate::session::permission_response(outcome)).map_err(|error| {
            AcpError::internal_error().data(format!("permission respond: {error}"))
        })
    }

    /// Answer a parked `session/request_permission` (the UI's
    /// `respond_permission` intent routes through the global owner registry;
    /// this method is the legacy fallback for a responder with no owner entry).
    pub fn respond_permission(
        &self,
        tool_call_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpError> {
        unregister_permission_answer_owned(tool_call_id, "main");
        self.inner.permission_ids.lock().remove(tool_call_id);
        self.respond_permission_local(tool_call_id, outcome)
    }

    pub(crate) fn on_permission_request(
        &self,
        req: RequestPermissionRequest,
        responder: Responder<RequestPermissionResponse>,
    ) {
        if self.is_closing() {
            let _ = responder.respond_with_error(Self::cancelled_error());
            return;
        }
        let display_id = self.inner.session_id.lock().clone().unwrap_or_default();
        let request = crate::session::permission_request(&req).on_chat(&display_id, "main");
        let tool_call_id = request.tool_call_id.clone();
        let cell: ParkedPermission = Arc::new(Mutex::new(Some(responder)));
        // `Core::respond_permission` used to route by `active_peer()`: a main
        // wire prompt was unanswerable while a roam chat was on screen. The
        // global registry routes by the id the UI actually shows and keeps the
        // owning wire next to the answer closure. A second prompt with the
        // same id from another wire is rejected and its server told so.
        let owner = self.clone();
        let answer: PermissionAnswer =
            Arc::new(move |id, outcome| owner.respond_permission_local(id, outcome));
        let reject: PermissionReject = {
            let cell = cell.clone();
            Arc::new(move |error| {
                if let Some(responder) = cell.lock().take() {
                    let _ = responder.respond_with_error(error);
                }
            })
        };
        if !register_permission_answer_for(tool_call_id.clone(), "main", answer, Some(reject)) {
            return;
        }
        self.inner.permission.lock().insert(tool_call_id.clone(), cell);
        self.inner.permission_ids.lock().insert(tool_call_id);
        self.inner.listener.on_permission_request(request);
    }

    pub(crate) fn on_server_request(&self, method: &str, params: &Value, responder: Responder<Value>) {
        match method {
            "_goose/unstable/session/recipe/request-params" => {
                self.park_and_notify(PendingRequest::RecipeParams, method, params, responder);
            }
            "elicitation/create" => {
                self.park_and_notify(PendingRequest::Elicitation, method, params, responder);
            }
            _ => {
                // Unknown server request -> a real JSON-RPC error, not a
                // silent empty result (desktop serverRequest fallback).
                let _ = responder.respond_with_error(
                    AcpError::method_not_found()
                        .data(format!("not supported by this client: {method}")),
                );
            }
        }
    }

    pub(crate) fn park_and_notify(
        &self,
        kind: PendingRequest,
        method: &str,
        params: &Value,
        responder: Responder<Value>,
    ) {
        park_server_request_on(
            kind,
            method,
            params,
            SessionOwner { kind: "main".into(), label: String::new() },
            responder,
        );
    }

}
/// Park a server request with the wire that raised it and return the reply key
/// handed to the UI. With no listener the request is auto-answered (recipe
/// defaults) or declined — desktop parity — and no key is emitted.
pub(crate) fn park_server_request_on(
    kind: PendingRequest,
    method: &str,
    params: &Value,
    owner: SessionOwner,
    responder: Responder<Value>,
) -> String {
    let listeners = server_request_listeners();
    if listeners.is_empty() {
        match kind {
            PendingRequest::RecipeParams => {
                let values = params
                    .get("parameters")
                    .and_then(Value::as_array)
                    .map(|parameters| {
                        parameters
                            .iter()
                            .filter_map(|p| {
                                let obj = p.as_object()?;
                                let key = obj.get("key")?.as_str()?.to_string();
                                let default = obj.get("default").cloned().unwrap_or(Value::Null);
                                Some((key, default))
                            })
                            .collect::<Map<String, Value>>()
                    })
                    .unwrap_or_default();
                let _ = responder.respond(json!({ "action": "submit", "values": values }));
            }
            PendingRequest::Elicitation => {
                let _ = responder.respond_with_error(
                    AcpError::method_not_found()
                        .data("elicitation/create not supported by this client"),
                );
            }
        }
        return String::new();
    }
    let request_id = request_id_from_params(kind, params, &owner);
    let previous = parked().lock().insert(
        (kind, request_id.clone()),
        ParkedRequest { owner: owner.clone(), responder },
    );
    // The server can repeat a request id. Supersede only the SAME key (family
    // + id); a different id keeps its own responder, so a form answered late
    // can no longer reply to whichever request happened to arrive last.
    if let Some(ParkedRequest { owner: previous_owner, responder: previous_responder }) = previous {
        let _ = previous_responder.respond_with_error(
            AcpError::internal_error()
                .data(format!("superseded by a newer {} request", kind.label())),
        );
        alert(format!(
            "superseded unanswered {} request {request_id} from {}",
            kind.label(),
            previous_owner.kind
        ));
    }
    for f in listeners {
        f(method, owner.clone(), request_id.clone(), params.clone());
    }
    request_id
}

/// Legacy four-argument seam used by roam peers today. Ownership from the bare
/// params is best-effort; the exact `(family, request_id)` key is what keeps a
/// peer from answering or superseding a form from another wire.
pub(crate) fn park_server_request(
    kind: PendingRequest,
    method: &str,
    params: &Value,
    responder: Responder<Value>,
) {
    let owner = owner_from_params(params);
    let _ = park_server_request_on(kind, method, params, owner, responder);
}

