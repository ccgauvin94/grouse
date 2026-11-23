// SPDX-License-Identifier: AGPL-3.0-or-later

//! Transport-independent session semantics: one place for the ACP parameters,
//! capability gates, wire-payload translation, and the SDK client wiring that
//! both the WebSocket spine and the roam peer use.
//!
//! Nothing here knows whether a session arrived over a WebSocket or an iroh
//! byte stream. A transport is only: where bytes come from, how a session id is
//! namespaced, and when the link is ready (see [`ChatEndpoint`]). Everything
//! else — `initialize` capabilities, `session/new|load|prompt` params,
//! `session/list` parsing, tool/chunk translation — lives here so a protocol
//! change cannot land in one transport and be forgotten in the other.
//!
//! The implementation is split into `params`, `translate`, `replay`, `dispatch`
//! and `hub` submodules, all re-exported here so the `session::*` paths used by
//! the rest of the core (and by the shared test module below) stay unchanged.

#![allow(clippy::type_complexity)] // The SDK handler table is intentionally a set of boxed closures.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};

use agent_client_protocol::schema::v1::{
    ClientCapabilities, ElicitationCapabilities, ElicitationFormCapabilities,
    FileSystemCapabilities, Implementation, PermissionOptionKind, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome, SessionInfo,
    ToolCall, ToolCallStatus,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{
    on_receive_notification, on_receive_request, Agent, Client, ConnectTo, Error as AcpError,
    Responder,
};
use futures::future::BoxFuture;
use serde_json::{json, Map, Value};

use crate::{
    ConfigChoice, ConfigOption, PermissionOutcome, PermissionRequest, Prompt, PromptBlock,
    SessionSummary, ToolCallKind,
};

// The session layer is split by concern; these re-exports keep the historical
// `session::*` paths (used across the core and the tests) intact.
mod dispatch;
mod hub;
mod params;
mod replay;
mod translate;

pub(crate) use dispatch::*;
pub(crate) use hub::*;
pub(crate) use params::*;
pub(crate) use replay::*;
pub(crate) use translate::*;

// ---------------------------------------------------------------------------
// Session id namespaces
// ---------------------------------------------------------------------------

/// The prefix that namespaces a peer's sessions (`CONTRACT §6`). The UI only
/// ever sees display ids; wire ids exist only inside an endpoint.
pub(crate) const ROAM_PREFIX: &str = "roam:";

/// Display id for a session owned by `label` (`None` = the main connection).
pub(crate) fn display_id(label: Option<&str>, wire: &str) -> String {
    match label {
        Some(label) => format!("{ROAM_PREFIX}{label}:{wire}"),
        None => wire.to_string(),
    }
}

/// Announce one successful session bind on any transport (audit §3).
///
/// The peer/spine bind sites hold a listener but not a `Core`, so this is the
/// shared exit for `on_session_opened`: call it only AFTER the store is painted
/// and BEFORE Ready, with the DISPLAY id (prefix included for roam). A rejected
/// load or a bind-only load must stay silent — that is the whole point of the
/// event, so the client no longer infers open success from Ready/watchdogs.
pub(crate) fn emit_session_opened(listener: &dyn crate::CoreListener, display_id: &str) {
    listener.on_session_opened(display_id.to_string());
}

// ---------------------------------------------------------------------------
// The SDK client: one handler table, whatever the transport
// ---------------------------------------------------------------------------

/// The handlers every connection must register. Both transports build the
/// same client from these, so a custom notification or server request can
/// never be handled on one wire and dropped on the other.
#[derive(Clone)]
pub(crate) struct ClientHandlers {
    pub notification: std::sync::Arc<
        dyn Fn(agent_client_protocol::schema::v1::SessionNotification) + Send + Sync,
    >,
    pub custom_notification: std::sync::Arc<dyn Fn(&str, Value) + Send + Sync>,
    pub permission: std::sync::Arc<
        dyn Fn(RequestPermissionRequest, Responder<RequestPermissionResponse>) + Send + Sync,
    >,
    pub request: std::sync::Arc<dyn Fn(&str, &Value, Responder<Value>) + Send + Sync>,
}

/// Run a connection with the shared handler table. `foreground` runs the
/// handshake and then serves until the link ends; it is the only
/// transport-specific part of the client.
#[allow(clippy::redundant_closure)] // `connect_with` needs an `AsyncFnOnce`, not a plain fn item
pub(crate) async fn connect_client<T, F>(
    transport: T,
    handlers: ClientHandlers,
    foreground: F,
) -> Result<(), AcpError>
where
    T: ConnectTo<Client> + 'static,
    F: FnOnce(
            agent_client_protocol::ConnectionTo<Agent>,
        ) -> BoxFuture<'static, Result<(), AcpError>>
        + Send
        + 'static,
{
    let ClientHandlers {
        notification,
        custom_notification,
        permission,
        request,
    } = handlers;
    Client
        .builder()
        .name("grouse")
        // Typed handler claims `session/update`; the untyped one below claims
        // every other notification (handlers are tried in registration order).
        .on_receive_notification(
            async move |notif, _cx| {
                notification(notif);
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_notification(
            async move |notif: agent_client_protocol::UntypedMessage, _cx| {
                custom_notification(&notif.method, notif.params.clone());
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: RequestPermissionRequest, responder, _cx| {
                permission(req, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: agent_client_protocol::UntypedMessage, responder, _cx| {
                request(&req.method, &req.params, responder);
                Ok(())
            },
            on_receive_request!(),
        )
        // The SDK's `connect_with` requires an `AsyncFnOnce`; a plain fn item
        // returning a boxed future does not satisfy it.
        .connect_with(transport, move |cx| foreground(cx))
        .await
}

#[allow(clippy::items_after_test_module)] // kept next to the helpers it exercises
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConnectionStatus, CoreListener, PermissionRequest, ProjectSummary, SessionSummary,
        TranscriptOp,
    };
    use futures::future::BoxFuture;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn a_load_the_host_never_answers_fails_instead_of_pinning_the_chat() {
        // A `session/load` with no reply used to wait forever: the await holds the
        // store's load slot, so the chat could not be opened again, and the client
        // got no event to explain the spinner. It must fail, say why, and let go.
        use futures::future::BoxFuture;
        struct Hang;
        impl ChatEndpoint for Hang {
            fn request(&self, _m: &str, _p: Value) -> BoxFuture<'static, Result<Value, AcpError>> {
                Box::pin(futures::future::pending())
            }
            fn notify(&self, _m: &str, _p: Value) -> Result<(), AcpError> {
                Ok(())
            }
            fn wire_id(&self, display: &str) -> String {
                display.to_string()
            }
            fn accepts_prompt(&self) -> bool {
                true
            }
            fn label(&self) -> Option<String> {
                None
            }
        }
        LOAD_TIMEOUT_SECS.store(1, Ordering::SeqCst);
        let error = crate::roam::runtime()
            .block_on(load_request(&Hang, "s1", "/work", Some(80)))
            .err();
        LOAD_TIMEOUT_SECS.store(600, Ordering::SeqCst);
        let error = error.expect("a load with no answer is an error, not a wait");
        assert!(
            format!("{error:?}").contains("gave no answer"),
            "{error:?}"
        );

        // A WHOLE-session load (a resync's rebuild, or the merge-gap fallback) has
        // its own, shorter bound: it holds the chat's paint while it waits, so it
        // must give up far sooner than a first open of a long chat may.
        assert!(
            REBUILD_TIMEOUT_SECS.load(Ordering::SeqCst) < LOAD_TIMEOUT_SECS.load(Ordering::SeqCst),
            "a rebuild must not wait as long as a first open"
        );
        REBUILD_TIMEOUT_SECS.store(1, Ordering::SeqCst);
        let error = crate::roam::runtime()
            .block_on(load_request(&Hang, "s1", "/work", None))
            .err();
        REBUILD_TIMEOUT_SECS.store(120, Ordering::SeqCst);
        assert!(
            format!("{:?}", error.expect("a whole-session load must fail too"))
                .contains("gave no answer")
        );
    }

    /// A session with a TURN IN FLIGHT is opened: the store is the newest copy.
    ///
    /// goose persists a message when it finishes, so the server's replay cannot
    /// contain the reply being streamed — and the replay's supersede/`reset`
    /// deletes the rows the store holds. Measured 2026-11-21: switching away
    /// mid-reply and back left the prompt with no reply, or a reply that started
    /// a paragraph in. A bind-only plan (the one an in-flight open now uses) must
    /// paint NOTHING, ask the wire for ONE message, and never probe the server:
    /// a verification that downgraded it to a merge would anchor on the live rows
    /// and truncate them.
    #[test]
    fn a_bind_only_load_keeps_the_live_rows_and_asks_for_one_message() {
        use futures::future::BoxFuture;
        use std::sync::Mutex as StdMutex;

        struct Host {
            store: Arc<crate::transcript::TranscriptStore>,
        }
        impl DispatchHost for Host {
            fn display_id(&self, wire: &str) -> String {
                wire.to_string()
            }
            fn store(&self, _k: &str) -> Arc<crate::transcript::TranscriptStore> {
                self.store.clone()
            }
            fn touched(&self, _k: String, _t: String, _u: String) {}
            fn active_run(&self, _k: String, _r: String) {}
            fn turn_lost(&self, _k: &str) {}
            fn config(&self, _k: &str, _o: Vec<ConfigOption>) {}
            fn usage(&self, _k: &str, _u: i64, _s: i64, _c: f64, _cur: String) {}
            fn commands(&self, _k: &str, _n: Vec<String>) {}
            fn permission(&self, _k: &str, _r: PermissionRequest) {}
            fn begin_replay(&self, _k: &str, _s: bool) {}
            fn end_replay(&self, _k: &str) {}
            fn replaying(&self, _k: &str) -> bool {
                false
            }
            fn replay_tick(&self, _k: &str, _m: &str) {}
            fn paging(&self, _k: &str) -> bool {
                false
            }
            fn replay_backlog(&self, _k: &str, _s: usize, _t: usize) {}
            fn accepts_chat(&self, _k: &str) -> bool {
                true
            }
            fn background(&self, _k: &str) {}
            fn endpoint_ready(&self, _l: Option<&str>) {}
            fn cache_session(&self, _k: &str) {}
            fn session_stamp(&self, _k: &str, _u: &str) {}
            fn listed_stamp(&self, _k: &str) -> String {
                String::new()
            }
            fn is_incomplete(&self, _k: &str) -> bool {
                true // a turn is running, which is what put us on this path
            }
            fn turn_in_flight(&self, _k: &str) -> bool {
                true
            }
            fn clear_incomplete(&self, _k: &str) {}
            fn mark_incomplete(&self, _k: &str) {}
            fn tool_run(&self, _k: &str, _r: &str) {}
            fn step_closed(&self, _k: &str) {}
            fn stamp_probe(&self, _k: &str, _p: bool) {}
            fn is_summarizer(&self, _k: &str) -> bool {
                false
            }
            fn summarizer_chunk(&self, _k: &str, _t: &str) -> bool {
                false
            }
            fn take_summarizer_reply(&self, _k: &str) -> String {
                String::new()
            }
        }

        /// The wire: a `session/load` ONLY (a probe would fail this assert), which
        /// replays the last turn as the server has persisted it — the prompt and
        /// the finished part, with no in-flight reply. The `replayTail` it was
        /// asked for is recorded so the test can see what a bind-only plan owes.
        struct Server(StdMutex<Option<u64>>);
        impl ChatEndpoint for Server {
            fn request(
                &self,
                method: &str,
                params: Value,
            ) -> BoxFuture<'static, Result<Value, AcpError>> {
                assert_eq!(method, "session/load", "a bind-only load must not probe");
                *self.0.lock().unwrap() = params
                    .pointer("/_meta/replayTail")
                    .and_then(Value::as_u64);
                Box::pin(async { Ok(json!({ "ok": true })) })
            }
            fn notify(&self, _m: &str, _p: Value) -> Result<(), AcpError> {
                Ok(())
            }
            fn wire_id(&self, display: &str) -> String {
                display.to_string()
            }
            fn accepts_prompt(&self) -> bool {
                true
            }
            fn label(&self) -> Option<String> {
                None
            }
        }

        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(NullListener)));
        store.set_session("roam:Phaethon:s1");
        store.append_chunk("user", "a question", Some("m1"), false);
        store.append_chunk("agent", "the reply so far", Some("m2"), false);
        let before = store.rich_transcript();
        let host = Host {
            store: store.clone(),
        };
        let server = Server(StdMutex::new(None));

        crate::roam::runtime()
            .block_on(load_with_plan(
                &server,
                &host,
                &store,
                "roam:Phaethon:s1",
                "/work",
                &PaintPlan::bind_only(),
            ))
            .expect("a bind-only load still has to answer");

        assert_eq!(
            Some(1),
            *server.0.lock().unwrap(),
            "a bind-only load asks the wire for ONE message, not the chat"
        );
        let after: Vec<(String, String)> = store
            .rich_transcript()
            .iter()
            .map(|i| (i.id.clone(), i.text.clone()))
            .collect();
        let expected: Vec<(String, String)> = before
            .iter()
            .map(|i| (i.id.clone(), i.text.clone()))
            .collect();
        assert_eq!(
            expected, after,
            "the store is the newest copy of a session with a turn in flight — it must \
             come back exactly as it was"
        );
    }

    /// A painted cache, a tail that cannot anchor, and a rebuild that fails.
    ///
    /// The gap path clears the store before rebuilding it from the whole session. That
    /// rebuild is capped (`REBUILD_TIMEOUT_SECS`) and a roam link can drop mid-stream, so
    /// it can fail — and the store has already been cleared by then. The release then
    /// painted whatever prefix had arrived, so the chat came back showing an OLDER turn
    /// with nothing reported. It must keep the transcript the client was looking at, and
    /// mark the session incomplete so the hole cannot survive as a "fresh" cache.
    #[test]
    fn a_failed_gap_rebuild_keeps_the_painted_transcript() {
        use futures::future::BoxFuture;

        struct Host {
            store: Arc<crate::transcript::TranscriptStore>,
            incomplete: StdMutex<Vec<String>>,
            events: StdMutex<Vec<String>>,
        }
        impl DispatchHost for Host {
            fn display_id(&self, wire: &str) -> String {
                wire.to_string()
            }
            fn store(&self, _key: &str) -> Arc<crate::transcript::TranscriptStore> {
                self.store.clone()
            }
            fn touched(&self, _k: String, _t: String, _u: String) {}
            fn active_run(&self, _k: String, _r: String) {}
            fn turn_lost(&self, _k: &str) {}
            fn config(&self, _k: &str, _o: Vec<ConfigOption>) {}
            fn usage(&self, _k: &str, _u: i64, _s: i64, _c: f64, _cur: String) {}
            fn commands(&self, _k: &str, _n: Vec<String>) {}
            fn permission(&self, _k: &str, _r: PermissionRequest) {}
            fn begin_replay(&self, key: &str, suppress: bool) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("begin {key} suppress={suppress}"));
            }
            fn end_replay(&self, key: &str) {
                self.events.lock().unwrap().push(format!("end {key}"));
            }
            fn replaying(&self, _k: &str) -> bool {
                false
            }
            fn replay_tick(&self, _k: &str, _m: &str) {}
            fn paging(&self, _k: &str) -> bool {
                false
            }
            fn replay_backlog(&self, _k: &str, _s: usize, _t: usize) {}
            fn accepts_chat(&self, _k: &str) -> bool {
                true
            }
            fn background(&self, _k: &str) {}
            fn endpoint_ready(&self, _l: Option<&str>) {}
            fn cache_session(&self, _k: &str) {}
            fn session_stamp(&self, _k: &str, _u: &str) {}
            fn listed_stamp(&self, _k: &str) -> String {
                String::new()
            }
            fn is_incomplete(&self, _k: &str) -> bool {
                false
            }
            fn mark_incomplete(&self, key: &str) {
                self.incomplete.lock().unwrap().push(key.to_string());
            }
            fn turn_in_flight(&self, k: &str) -> bool {
                self.events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|e| e == &format!("in_flight {k}"))
            }
            fn clear_incomplete(&self, key: &str) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("settled {key}"));
            }
            fn tool_run(&self, _k: &str, _r: &str) {}
            fn step_closed(&self, _k: &str) {}
            fn stamp_probe(&self, _k: &str, _p: bool) {}
            fn is_summarizer(&self, _k: &str) -> bool {
                false
            }
            fn summarizer_chunk(&self, _k: &str, _t: &str) -> bool {
                false
            }
            fn take_summarizer_reply(&self, _k: &str) -> String {
                String::new()
            }
        }

        /// The server: a bounded tail that streams one row the paint cannot name (so
        /// the merge finds no anchor), and a whole-session rebuild that dies.
        struct Server(Arc<crate::transcript::TranscriptStore>);
        impl ChatEndpoint for Server {
            fn request(
                &self,
                method: &str,
                params: Value,
            ) -> BoxFuture<'static, Result<Value, AcpError>> {
                assert_eq!(method, "session/load", "only a load is expected here");
                let full = params
                    .pointer("/_meta/replayTail")
                    .and_then(Value::as_u64)
                    .is_none();
                if full {
                    return Box::pin(async {
                        Err(AcpError::internal_error().data("link died mid-rebuild"))
                    });
                }
                self.0
                    .append_chunk("agent", "a row the paint cannot name", Some("z9"), false);
                Box::pin(async { Ok(json!({ "ok": true })) })
            }
            fn notify(&self, _m: &str, _p: Value) -> Result<(), AcpError> {
                Ok(())
            }
            fn wire_id(&self, display: &str) -> String {
                display.to_string()
            }
            fn accepts_prompt(&self) -> bool {
                true
            }
            fn label(&self) -> Option<String> {
                None
            }
        }

        // What the cache would have painted: an older turn this client had seen.
        let seed = crate::transcript::TranscriptStore::new(Box::new(NullListener));
        seed.set_session("roam:Phaethon:s1");
        seed.append_chunk("user", "an older question", Some("m1"), false);
        seed.append_chunk("agent", "an older answer", Some("m2"), false);
        let painted = seed.rich_transcript();

        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(NullListener)));
        store.set_session("roam:Phaethon:s1");
        let host = Host {
            store: store.clone(),
            incomplete: StdMutex::new(Vec::new()),
            events: StdMutex::new(Vec::new()),
        };
        let plan = PaintPlan {
            suppress: false,
            merge: true,
            keep: false,
            tail: REPLAY_TAIL,
            items: Some(painted.clone()),
            complete: true,
        };
        let server = Server(store.clone());
        crate::roam::runtime()
            .block_on(load_with_plan(
                &server,
                &host,
                &store,
                "roam:Phaethon:s1",
                "/work",
                &plan,
            ))
            .expect("the open itself succeeded; only the rebuild failed");

        let kept = store.rich_transcript();
        // Compare identity and content, not `seq`: that is a position stamp the store
        // assigns itself, and the point here is WHICH rows survived.
        let shape = |items: &[crate::Item]| -> Vec<(String, crate::ItemKind, String)> {
            items
                .iter()
                .map(|i| (i.id.clone(), i.kind, i.text.clone()))
                .collect()
        };
        assert_eq!(
            shape(&kept),
            shape(&painted),
            "the rows the client was reading are what survives — not a prefix of the rebuild"
        );
        assert_eq!(
            host.incomplete.lock().unwrap().as_slice(),
            ["roam:Phaethon:s1"],
            "a transcript with a hole in it must never read back as current"
        );
        // The bar belongs to the whole load: it must not finish before the rebuild it caused.
        let events = host.events.lock().unwrap().clone();
        assert_eq!(
            events.iter().filter(|e| e.starts_with("begin")).count(),
            1,
            "{events:?}"
        );
        assert_eq!(
            events.last().map(String::as_str),
            Some("end roam:Phaethon:s1"),
            "the replay ends only after the rebuild: {events:?}"
        );
    }

    /// A recording host for the paint/verify regression tests. `listed` is the
    /// stale baseline a fresh-cache plan is built from; `stamps` records the
    /// baseline a verification had to correct.
    struct TestHost {
        store: Arc<crate::transcript::TranscriptStore>,
        listed: StdMutex<String>,
        stamps: StdMutex<Vec<String>>,
        events: StdMutex<Vec<String>>,
        incomplete: StdMutex<Vec<String>>,
    }

    impl TestHost {
        fn new(store: Arc<crate::transcript::TranscriptStore>) -> Arc<Self> {
            Arc::new(Self {
                store,
                listed: StdMutex::new("listed@t1".into()),
                stamps: StdMutex::new(Vec::new()),
                events: StdMutex::new(Vec::new()),
                incomplete: StdMutex::new(Vec::new()),
            })
        }
    }

    impl DispatchHost for TestHost {
        fn display_id(&self, wire: &str) -> String {
            wire.to_string()
        }
        fn store(&self, _key: &str) -> Arc<crate::transcript::TranscriptStore> {
            self.store.clone()
        }
        fn touched(&self, _key: String, _title: String, _updated: String) {}
        fn active_run(&self, _key: String, _run: String) {}
        fn turn_lost(&self, _key: &str) {}
        fn config(&self, _key: &str, _options: Vec<ConfigOption>) {}
        fn usage(&self, _key: &str, _u: i64, _s: i64, _c: f64, _cur: String) {}
        fn commands(&self, _key: &str, _names: Vec<String>) {}
        fn permission(&self, _key: &str, _request: PermissionRequest) {}
        fn begin_replay(&self, key: &str, suppress: bool) {
            self.events
                .lock()
                .unwrap()
                .push(format!("begin {key} suppress={suppress}"));
        }
        fn end_replay(&self, key: &str) {
            self.events.lock().unwrap().push(format!("end {key}"));
        }
        fn replaying(&self, _key: &str) -> bool {
            false
        }
        fn replay_tick(&self, _key: &str, _id: &str) {}
        fn paging(&self, _key: &str) -> bool {
            false
        }
        fn replay_backlog(&self, _key: &str, _skipped: usize, _tail: usize) {}
        fn accepts_chat(&self, _key: &str) -> bool {
            true
        }
        fn background(&self, _key: &str) {}
        fn endpoint_ready(&self, _label: Option<&str>) {}
        fn cache_session(&self, _key: &str) {}
        fn session_stamp(&self, _key: &str, updated: &str) {
            self.stamps.lock().unwrap().push(updated.to_string());
        }
        fn listed_stamp(&self, _key: &str) -> String {
            self.listed.lock().unwrap().clone()
        }
        fn is_incomplete(&self, _key: &str) -> bool {
            false
        }
        fn turn_in_flight(&self, _key: &str) -> bool {
            false
        }
        fn clear_incomplete(&self, _key: &str) {}
        fn mark_incomplete(&self, key: &str) {
            self.incomplete.lock().unwrap().push(key.to_string());
        }
        fn tool_run(&self, _key: &str, _run: &str) {}
        fn step_closed(&self, _key: &str) {}
        fn stamp_probe(&self, _key: &str, _persist: bool) {}
        fn is_summarizer(&self, _key: &str) -> bool {
            false
        }
        fn summarizer_chunk(&self, _key: &str, _text: &str) -> bool {
            false
        }
        fn take_summarizer_reply(&self, _key: &str) -> String {
            String::new()
        }
    }

    /// The probe says another client moved the session; the bounded replay
    /// repeats the painted tail (`m2`, `m3`) and adds its row (`m4`).
    struct StaleServer {
        store: Arc<crate::transcript::TranscriptStore>,
        tails: StdMutex<Vec<Option<u64>>>,
    }

    impl ChatEndpoint for StaleServer {
        fn request(
            &self,
            method: &str,
            params: Value,
        ) -> BoxFuture<'static, Result<Value, AcpError>> {
            if method == "_goose/unstable/session/info" {
                return Box::pin(async { Ok(json!({ "session": { "updatedAt": "listed@t2" } })) });
            }
            assert_eq!(method, "session/load");
            self.tails.lock().unwrap().push(
                params
                    .pointer("/_meta/replayTail")
                    .and_then(Value::as_u64),
            );
            let store = self.store.clone();
            Box::pin(async move {
                for (id, text) in [
                    ("m2", "older answer"),
                    ("m3", "newest from us"),
                    ("m4", "written by another client"),
                ] {
                    store.append_chunk("agent", text, Some(id), false);
                }
                Ok(json!({ "ok": true }))
            })
        }
        fn notify(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
            Ok(())
        }
        fn wire_id(&self, display: &str) -> String {
            display.to_string()
        }
        fn accepts_prompt(&self) -> bool {
            true
        }
        fn label(&self) -> Option<String> {
            None
        }
    }

    fn shape(items: &[crate::Item]) -> Vec<(String, crate::ItemKind, String)> {
        items
            .iter()
            .map(|i| (i.id.clone(), i.kind, i.text.clone()))
            .collect()
    }

    fn seed_painted(store: &crate::transcript::TranscriptStore) -> Vec<crate::Item> {
        store.set_session("s1");
        store.append_chunk("user", "older question", Some("m1"), false);
        store.append_chunk("agent", "older answer", Some("m2"), false);
        store.append_chunk("agent", "newest from us", Some("m3"), false);
        store.rich_transcript()
    }

    #[test]
    fn a_suppression_downgraded_by_verification_merges_the_paint() {
        // Paint-then-verify used to call `replace_rich` (no merge gate), then
        // downgrade to a merge: `end_merge()` returned false and the overlap
        // was APPENDED — the newest turn appeared twice. Verify before painting
        // so the rows are armed as a merge base from the start.
        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(NullListener)));
        let painted = seed_painted(&store);
        let host = TestHost::new(store.clone());
        let server = StaleServer {
            store: store.clone(),
            tails: StdMutex::new(Vec::new()),
        };
        let plan = PaintPlan {
            suppress: true, // the stale plan's verdict
            merge: false,
            keep: false,
            tail: 1,
            items: Some(painted.clone()),
            complete: true,
        };
        crate::roam::runtime()
            .block_on(load_with_plan(&server, host.as_ref(), &store, "s1", "/w", &plan))
            .expect("the load answered");

        assert_eq!(
            server.tails.lock().unwrap().as_slice(),
            [Some(REPLAY_TAIL as u64)],
            "a downgraded suppression owes the overlap tail, not the 1-message bind"
        );
        assert_eq!(
            host.stamps.lock().unwrap().as_slice(),
            ["listed@t2"],
            "the server's stamp must replace the stale baseline"
        );
        assert_eq!(
            shape(&store.rich_transcript()),
            vec![
                ("m1".into(), crate::ItemKind::User, "older question".into()),
                ("m2".into(), crate::ItemKind::Agent, "older answer".into()),
                ("m3".into(), crate::ItemKind::Agent, "newest from us".into()),
                (
                    "m4".into(),
                    crate::ItemKind::Agent,
                    "written by another client".into()
                ),
            ],
            "the overlap replaces the painted suffix; the other client's row is new"
        );
    }

    #[test]
    fn a_resume_verifies_before_discarding_the_replay() {
        // The main wire paints in `open_session_inner` and probes in its
        // handshake, i.e. AFTER the paint. The shared replay body has to re-arm
        // those rows as a merge base when verification downgrades the ask.
        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(NullListener)));
        let painted = seed_painted(&store);
        let host = TestHost::new(store.clone());
        let server = StaleServer {
            store: store.clone(),
            tails: StdMutex::new(Vec::new()),
        };
        let plan = PaintPlan {
            suppress: true,
            merge: false,
            keep: false,
            tail: 1,
            items: Some(painted.clone()),
            complete: true,
        };
        apply_paint(&store, &plan); // the caller's pre-verification paint
        crate::roam::runtime()
            .block_on(replay_session(&server, host.as_ref(), &store, "s1", "/w", true, false))
            .expect("the load answered");

        assert_eq!(server.tails.lock().unwrap().as_slice(), [Some(REPLAY_TAIL as u64)]);
        assert_eq!(
            shape(&store.rich_transcript()),
            vec![
                ("m1".into(), crate::ItemKind::User, "older question".into()),
                ("m2".into(), crate::ItemKind::Agent, "older answer".into()),
                ("m3".into(), crate::ItemKind::Agent, "newest from us".into()),
                (
                    "m4".into(),
                    crate::ItemKind::Agent,
                    "written by another client".into()
                ),
            ],
            "the replay must fill in the other client's row without duplicating ours"
        );
    }

    #[test]
    fn a_failed_merge_load_disarms_the_merge_gate() {
        use futures::future::BoxFuture;
        struct FailingLoad;
        impl ChatEndpoint for FailingLoad {
            fn request(
                &self,
                method: &str,
                _params: Value,
            ) -> BoxFuture<'static, Result<Value, AcpError>> {
                assert_eq!(method, "session/load");
                Box::pin(async { Err(AcpError::internal_error().data("link died")) })
            }
            fn notify(&self, _m: &str, _p: Value) -> Result<(), AcpError> {
                Ok(())
            }
            fn wire_id(&self, display: &str) -> String {
                display.to_string()
            }
            fn accepts_prompt(&self) -> bool {
                true
            }
            fn label(&self) -> Option<String> {
                None
            }
        }

        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(NullListener)));
        let painted = seed_painted(&store);
        let host = TestHost::new(store.clone());
        let plan = PaintPlan {
            suppress: false,
            merge: true,
            keep: false,
            tail: REPLAY_TAIL,
            items: Some(painted.clone()),
            complete: true,
        };
        assert!(crate::roam::runtime()
            .block_on(load_with_plan(
                &FailingLoad,
                host.as_ref(),
                &store,
                "s1",
                "/w",
                &plan
            ))
            .is_err());
        assert!(
            !store.end_merge(),
            "the guard must disarm merge on the error exit; an armed gate would \
             truncate a later load at this abandoned anchor"
        );
        assert_eq!(
            host.events.lock().unwrap().as_slice(),
            ["begin s1 suppress=false", "end s1"],
            "a failed load still ends its replay"
        );
    }

    #[test]
    fn a_cancelled_gap_rebuild_restores_the_painted_rows() {
        use futures::future::BoxFuture;
        struct CancelledRebuild {
            store: Arc<crate::transcript::TranscriptStore>,
            full_started: Arc<std::sync::atomic::AtomicBool>,
        }
        impl ChatEndpoint for CancelledRebuild {
            fn request(
                &self,
                method: &str,
                params: Value,
            ) -> BoxFuture<'static, Result<Value, AcpError>> {
                assert_eq!(method, "session/load");
                if params.pointer("/_meta/replayTail").is_none() {
                    // The whole-session rebuild: park here so the test can
                    // cancel with the store already cleared.
                    self.full_started
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    return Box::pin(futures::future::pending());
                }
                let store = self.store.clone();
                Box::pin(async move {
                    // An id the paint cannot name ⇒ a merge gap ⇒ full rebuild.
                    store.append_chunk("agent", "unmatched", Some("z9"), false);
                    Ok(json!({ "ok": true }))
                })
            }
            fn notify(&self, _m: &str, _p: Value) -> Result<(), AcpError> {
                Ok(())
            }
            fn wire_id(&self, display: &str) -> String {
                display.to_string()
            }
            fn accepts_prompt(&self) -> bool {
                true
            }
            fn label(&self) -> Option<String> {
                None
            }
        }

        let store = Arc::new(crate::transcript::TranscriptStore::new(Box::new(NullListener)));
        let painted = seed_painted(&store);
        let host = TestHost::new(store.clone());
        let full_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen_started = full_started.clone();
        let server = CancelledRebuild {
            store: store.clone(),
            full_started: full_started.clone(),
        };
        let task_store = store.clone();
        let task_host = host.clone();
        let task_painted = painted.clone();
        crate::roam::runtime().block_on(async move {
            let task = crate::roam::runtime().spawn(async move {
                let plan = PaintPlan {
                    suppress: false,
                    merge: true,
                    keep: false,
                    tail: REPLAY_TAIL,
                    items: Some(task_painted),
                    complete: true,
                };
                load_with_plan(&server, task_host.as_ref(), &task_store, "s1", "/w", &plan).await
            });
            for _ in 0..200 {
                if seen_started.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(
                seen_started.load(std::sync::atomic::Ordering::SeqCst),
                "the rebuild must have started before the cancellation"
            );
            task.abort();
            let _ = task.await;
        });

        assert_eq!(
            shape(&store.rich_transcript()),
            shape(&painted),
            "a dropped rebuild must not leave the chat empty or half-loaded"
        );
        assert_eq!(
            host.incomplete.lock().unwrap().as_slice(),
            ["s1"],
            "the restored paint has a hole, so it cannot read back as current"
        );
        assert!(
            host.events.lock().unwrap().iter().any(|e| e == "end s1"),
            "the replay bar must end even for a cancelled load"
        );
    }

    #[test]
    fn an_assistant_project_session_is_not_a_chat_but_keeps_its_cwd() {
        // One chat per thread label is filed under the `assistant` project so the
        // persona prompt applies — and then it must not appear in any chat list,
        // because My Day's thread groups are the way in. Its cwd still has to be
        // recorded: the chat is opened BY ID, and `session/load` rewrites the working
        // dir from the cwd sent, so a missing side-table entry would move it.
        let reply = json!({"sessions": [
            {"sessionId": "s_thread", "title": "Thread: Dentist reschedule", "cwd": "/projects/x",
             "updatedAt": "2026-10-30T21:00:00Z",
             "_meta": {"projectId": "assistant", "lastMessageAt": "2026-10-30T21:02:00Z"}},
            {"sessionId": "s_chat", "title": "Ordinary chat", "cwd": "/projects/y",
             "updatedAt": "2026-10-30T20:00:00Z", "_meta": {}}
        ]});
        let (sessions, cwds, updated) = parse_sessions(&reply);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "s_chat");
        assert!(cwds.contains(&("s_thread".to_string(), "/projects/x".to_string())));
        assert!(updated.contains(&("s_thread".to_string(), "2026-10-30T21:02:00Z".to_string())));
    }

    #[test]
    fn a_new_session_carries_its_project_as_both_a_column_and_a_tag() {
        // goose is moving project association out of the server (grouse#5): the
        // column still drives instruction injection, the tag is what survives its
        // retirement. Writing both at creation is the whole P0 — and a server that
        // predates tags just ignores the key.
        let params = session_new_params("grouse-test", "/w", None, Some("media"), None);
        assert_eq!(
            params.pointer("/_meta/projectId").and_then(Value::as_str),
            Some("media")
        );
        assert_eq!(
            params
                .pointer("/_meta/tags/grouse.project")
                .and_then(Value::as_str),
            Some("media")
        );
        assert_eq!(PROJECT_TAG_KEY, "grouse.project");

        // No project, no tag: an unfiled chat must not grow an empty bag.
        let unfiled = session_new_params("grouse-test", "/w", Some("recipe-1"), None, None);
        assert!(unfiled.pointer("/_meta/tags").is_none());
        assert!(unfiled.pointer("/_meta/projectId").is_none());
    }

    #[test]
    fn a_new_session_can_carry_a_mode_tag() {
        // A mode is a client tag written ONCE, at creation: this server stores and
        // filters on tags but returns none and cannot mutate one, so creation is the
        // only moment a chat can be given a mode at all.
        let dev = session_new_params("grouse-test", "/w", None, None, Some("dev"));
        assert_eq!(
            dev.pointer("/_meta/tags/grouse.mode").and_then(Value::as_str),
            Some("dev")
        );
        // A mode rides alongside a project rather than replacing its tag.
        let both = session_new_params("grouse-test", "/w", None, Some("media"), Some("dev"));
        assert_eq!(
            both.pointer("/_meta/tags/grouse.project").and_then(Value::as_str),
            Some("media")
        );
        assert_eq!(
            both.pointer("/_meta/tags/grouse.mode").and_then(Value::as_str),
            Some("dev")
        );
        // An unknown mode is dropped: a typo must not mint a tag nothing filters on.
        let typo = session_new_params("grouse-test", "/w", None, None, Some("devel"));
        assert!(typo.pointer("/_meta/tags").is_none());
        assert_eq!(MODE_VALUES, &["chat", "dev"]);
        assert_eq!(DEFAULT_MODE, "chat");
        // "chat" is the default, so an explicit chat tag is legal but redundant.
        let chat = session_new_params("grouse-test", "/w", None, None, Some("chat"));
        assert_eq!(
            chat.pointer("/_meta/tags/grouse.mode").and_then(Value::as_str),
            Some("chat")
        );
    }

    #[test]
    fn a_session_list_can_filter_on_tags() {
        // The read path for a tag: ask which sessions carry it. This is the only
        // truthful way to learn a mode, given a row never comes back with tags.
        let plain = session_list_params(None, &[]);
        assert!(plain.pointer("/_meta/tags").is_none());
        assert_eq!(plain.pointer("/_meta/types/0").and_then(Value::as_str), Some("user"));
        let filtered = session_list_params(
            Some("cur"),
            &[(MODE_TAG_KEY.to_string(), "dev".to_string())],
        );
        assert_eq!(
            filtered.pointer("/_meta/tags/grouse.mode").and_then(Value::as_str),
            Some("dev")
        );
        assert_eq!(filtered.pointer("/cursor").and_then(Value::as_str), Some("cur"));
    }

    #[test]
    fn a_project_survives_on_the_tag_alone() {
        // The read half. Until goose returns tags in `SessionInfo` a tag-only row
        // cannot actually arrive, but this is the shape that keeps both clients
        // working the day `project_id` is gone — including the assistant rule, which
        // is what keeps thread chats out of the chat list.
        let reply = json!({"sessions": [
            {"sessionId": "s_filed", "title": "Media chat", "cwd": "/p",
             "_meta": {"tags": {"grouse.project": "media"}}},
            {"sessionId": "s_thread", "title": "Thread: dentist", "cwd": "/p",
             "_meta": {"tags": {"grouse.project": "assistant"}}},
            {"sessionId": "s_both", "title": "Both", "cwd": "/p",
             "_meta": {"projectId": "hacking", "tags": {"grouse.project": "media"}}},
            {"sessionId": "s_blank", "title": "Blank", "cwd": "/p",
             "_meta": {"tags": {"grouse.project": "   "}}},
            {"sessionId": "s_other", "title": "Other tag", "cwd": "/p",
             "_meta": {"tags": {"buzz.thread": "t1"}}}
        ]});
        let (sessions, _, _) = parse_sessions(&reply);
        let project = |id: &str| {
            sessions
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.project_id.clone())
        };
        assert_eq!(project("s_filed").as_deref(), Some("media"));
        assert!(
            project("s_thread").is_none(),
            "the assistant rule must follow the tag, not just the column"
        );
        assert_eq!(
            project("s_both").as_deref(),
            Some("hacking"),
            "the column is authoritative while it exists"
        );
        assert!(project("s_blank").is_none(), "blank is not a project");
        assert!(project("s_other").is_none(), "an unrelated tag is not ours");
    }

    #[test]
    fn the_match_is_case_and_space_insensitive_but_not_fuzzy() {
        // The column holds what a client sent; being lenient about case/whitespace
        // costs nothing and the cost of being wrong is one hidden chat either way.
        let mk = |pid: Option<&str>| SessionSummary {
            mode: None,
            id: "s".into(),
            title: "t".into(),
            updated_at: String::new(),
            last_message_snippet: None,
            project_id: pid.map(|p| p.to_string()),
            message_count: 0,
            model: String::new(),
            has_recipe: false,
            has_new: false,
            archived: false,
        };
        assert!(in_assistant_project(&mk(Some("assistant"))));
        assert!(in_assistant_project(&mk(Some(" Assistant "))));
        assert!(!in_assistant_project(&mk(Some("assistant-2"))));
        assert!(!in_assistant_project(&mk(None)));
    }

    #[test]
    fn a_subagent_notification_becomes_one_step_on_the_delegate_row() {
        use agent_client_protocol::schema::v1::{
            ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
        };
        use serde_json::json;

        let mut meta = agent_client_protocol::schema::v1::Meta::new();
        meta.insert(
            "toolNotification".to_string(),
            json!({
                "type": "message",
                "params": {
                    "level": "info",
                    "logger": "subagent:abc",
                    "data": {
                        "type": "subagent_tool_request",
                        "subagent_id": "abc",
                        "tool_call": {
                            "name": "developer__shell",
                            "arguments": { "command": "git status --short" }
                        }
                    }
                }
            }),
        );
        let update = ToolCallUpdate::new(
            ToolCallId::new("delegate-1"),
            ToolCallUpdateFields::new().status(ToolCallStatus::InProgress),
        )
        .meta(meta);

        let step = tool_subagent_step(&update).expect("a subagent step");
        assert_eq!(step.name, "developer__shell");
        assert_eq!(step.detail, "git status --short");
    }

    #[test]
    fn a_session_info_update_declares_one_of_three_run_states() {
        // goose's `activeRunId` is three-state and the last two must not be
        // conflated: a generated session-name update says NOTHING about runs,
        // while `null` is the server stating the run is over (the end-of-turn
        // signal a client still receives when the prompt response is lost).
        use agent_client_protocol::schema::v1::{Meta, SessionInfoUpdate};

        fn meta(value: serde_json::Value) -> Meta {
            value.as_object().cloned().expect("meta")
        }

        let live = SessionInfoUpdate::new().meta(meta(json!({ "goose": { "activeRunId": "run-9" } })));
        assert_eq!(declared_run_state(&live).as_deref(), Some("run-9"));

        let ended =
            SessionInfoUpdate::new().meta(meta(json!({ "goose": { "activeRunId": null } })));
        assert_eq!(
            declared_run_state(&ended).as_deref(),
            Some(""),
            "an explicit null is an end, not silence"
        );

        let name_only =
            SessionInfoUpdate::new().title("Grouse").meta(meta(json!({ "messageCount": 12 })));
        assert_eq!(
            declared_run_state(&name_only),
            None,
            "a title update must not be read as a run end"
        );
    }

    #[test]
    fn ordinary_tool_notifications_are_not_mistaken_for_subagent_steps() {
        use agent_client_protocol::schema::v1::{
            ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
        };
        use serde_json::json;

        // A shell live-output chunk rides the same `_meta` channel and must not
        // show up as child activity.
        let mut meta = agent_client_protocol::schema::v1::Meta::new();
        meta.insert(
            "toolNotification".to_string(),
            json!({ "type": "live_output", "params": { "chunks": [] } }),
        );
        let update = ToolCallUpdate::new(
            ToolCallId::new("call-1"),
            ToolCallUpdateFields::new().status(ToolCallStatus::InProgress),
        )
        .meta(meta);
        assert!(tool_subagent_step(&update).is_none());

        // And a plain log line from some other logger is not a step either.
        let mut meta = agent_client_protocol::schema::v1::Meta::new();
        meta.insert(
            "toolNotification".to_string(),
            json!({ "type": "message", "params": { "level": "info", "logger": "app", "data": {} } }),
        );
        let update = ToolCallUpdate::new(
            ToolCallId::new("call-2"),
            ToolCallUpdateFields::new().status(ToolCallStatus::InProgress),
        )
        .meta(meta);
        assert!(tool_subagent_step(&update).is_none());
    }

    struct Recorder {
        events: StdMutex<Vec<String>>,
    }

    impl CoreListener for Recorder {
    fn on_advisor_report(&self, _: String, _: String) {}

    fn on_sensitivity(&self, _data: String) {}
    fn on_thread_timeline(&self, _label: String, _rows: Vec<crate::TimelineRow>) {}
    fn on_file(&self, _: crate::attachments::FileEvent) {}
        fn on_status(&self, _: ConnectionStatus) {}
        fn on_sessions(&self, _: Vec<SessionSummary>) {}
        fn on_item(&self, op: TranscriptOp) {
            let line = match op {
                TranscriptOp::Reset { .. } => "item Reset".to_string(),
                TranscriptOp::Upsert { item } => format!("item Upsert {}", item.id),
                TranscriptOp::AppendText { id, .. } => format!("item AppendText {id}"),
                TranscriptOp::AppendOutput { id, .. } => format!("item AppendOutput {id}"),
                TranscriptOp::Remove { id } => format!("item Remove {id}"),
                TranscriptOp::Window { .. } => "item Window".to_string(),
            };
            self.events.lock().unwrap().push(line);
        }
        fn on_usage(&self, _: i64, _: i64, _: f64, _: String) {}
        fn on_run_ended(&self, _session: String, stop: String) {
            self.events
                .lock()
                .unwrap()
                .push(format!("run_ended {stop}"));
        }
        fn on_speak(&self, _: String) {}
        fn on_config(&self, _: Vec<ConfigOption>) {}
        fn on_permission_request(&self, _: PermissionRequest) {}
        fn on_session_touched(&self, id: String, _: String, _: String) {
            self.events.lock().unwrap().push(format!("touched {id}"));
        }
        fn on_projects(&self, _: Vec<ProjectSummary>) {}
        fn on_roam_peer_status(&self, _: String, _: String) {}
        fn on_roam_sessions(&self, _: String, _: Vec<SessionSummary>) {}
        fn on_peer_new_session(&self, _: String, _: String) {}
        fn on_active_run(&self, _: String, _: String) {}
        fn on_commands(&self, _: Vec<String>) {}
        fn on_session_open_failed(&self, _: String, _: String) {}
        fn on_session_opened(&self, session_id: String) {
            self.events
                .lock()
                .unwrap()
                .push(format!("opened {session_id}"));
        }
        fn on_tasks(&self, _: String) {}
        fn on_thread_ready(&self, _: String, _: String) {}
        fn on_replay_progress(&self, _: String, _: i64, _: i64, _: bool) {}
    }

    fn hub() -> (Arc<SessionHub>, Arc<Recorder>) {
        let recorder = Arc::new(Recorder {
            events: StdMutex::new(Vec::new()),
        });
        let listener: Arc<dyn CoreListener> = recorder.clone();
        (SessionHub::new(listener), recorder)
    }

    #[test]
    fn hub_paints_only_the_displayed_session() {
        let (hub, recorder) = hub();
        let background = hub.store("roam:p:s1");
        background.append_chunk("agent", "background", Some("m1"), false);
        assert!(
            recorder.events.lock().unwrap().is_empty(),
            "background must not paint"
        );

        hub.display("roam:p:s1");
        background.append_chunk("agent", " live", Some("m1"), false);
        assert!(recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.starts_with("item ")));
    }

    #[test]
    fn hub_marks_a_background_session_once() {
        let (hub, recorder) = hub();
        hub.store("roam:p:s1");
        hub.mark_new("roam:p:s1");
        hub.mark_new("roam:p:s1");
        let count = recorder
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.as_str() == "touched roam:p:s1")
            .count();
        assert_eq!(count, 1, "the green dot is set once per burst");
        assert!(hub.has_new("roam:p:s1"));
        hub.display("roam:p:s1");
        assert!(!hub.has_new("roam:p:s1"), "displaying clears the dot");
    }

    #[test]
    fn a_fresh_cache_is_only_trusted_when_the_server_agrees() {
        // THE BUG THIS GUARDS (measured 2026-11-12): the "fresh cache" verdict is
        // built from the baseline the core last saw in `session/list` — which does
        // not move when ANOTHER client writes to the session. `Quick` then
        // compared a saved stamp with a stale copy of the same stamp, called the
        // cache current, threw the replay away, and fetched nothing: a refresh
        // that silently does nothing, worst on a chat another client is writing.
        let fresh = ReplayAsk {
            suppress: true,
            merge: false,
            tail: 1,
        };
        // The server's own stamp agrees: keep the suppression (the fast path).
        let same = ask_with_stamp(fresh, "2026-11-12T20:00:00Z", Some("2026-11-12T20:00:00Z"));
        assert!(same.suppress && !same.merge && same.tail == 1);

        // It has moved: merge a bounded tail instead of discarding the replay.
        let moved = ask_with_stamp(fresh, "2026-11-12T20:00:00Z", Some("2026-11-12T21:00:00Z"));
        assert!(!moved.suppress && moved.merge, "{moved:?}");
        assert_eq!(moved.tail, REPLAY_TAIL, "a downgrade owes the overlap tail");

        // A probe that fails or says nothing is NOT evidence of staleness: a
        // hiccup must not repaint every open of a current chat.
        assert!(ask_with_stamp(fresh, "t1", None).suppress);
        assert!(ask_with_stamp(fresh, "t1", Some("")).suppress);
        // An unknown baseline cannot confirm freshness either way; the server's
        // stamp is then the only word, and it is taken as the truth.
        let unknown = ask_with_stamp(fresh, "", Some("t9"));
        assert!(!unknown.suppress && unknown.merge);

        // A merge or a full replay is already fetching: no probe, no change.
        let merge = ReplayAsk {
            suppress: false,
            merge: true,
            tail: REPLAY_TAIL,
        };
        assert_eq!(ask_with_stamp(merge, "t1", Some("t2")), merge);
        let full = ReplayAsk {
            suppress: false,
            merge: false,
            tail: 80,
        };
        assert_eq!(ask_with_stamp(full, "t1", Some("t2")), full);
    }

    #[test]
    fn paint_plan_respects_stamp_freshness() {
        let dir = std::env::temp_dir().join(format!(
            "grouse-session-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache = crate::cache::CacheStore::new(dir);
        let item = crate::Item {
            created: 0,
            id: "m1".into(),
            kind: crate::ItemKind::Agent,
            text: "hi".into(),
            detail: String::new(),
            output: String::new(),
            seq: 0,
            attachments: 0,
            files: Vec::new(),
            images: Vec::new(),
            message_id: String::new(),
            status: String::new(),
            app_key: String::new(),
            calls: Vec::new(),
            thought: String::new(),
            summary: String::new(),
            steps: Vec::new(),
        };
        cache.save_transcript("s1", &[item], "t1", true);

        let fresh = plan_paint(&cache, "s1", "t1", false, false);
        assert!(fresh.suppress && !fresh.merge);

        let stale = plan_paint(&cache, "s1", "t2", false, false);
        assert!(!stale.suppress && stale.merge);

        let forced = plan_paint(&cache, "s1", "t1", true, false);
        assert!(!forced.suppress && !forced.merge && forced.items.is_some());

        let missing = plan_paint(&cache, "nope", "t1", false, false);
        assert!(!missing.suppress && !missing.merge && missing.items.is_none());
    }
}

// ---------------------------------------------------------------------------
// One transmit surface
// ---------------------------------------------------------------------------

/// What an intent needs from a live connection. The spine (WebSocket) and a
/// roam peer (iroh) differ only in how bytes travel and how ids are namespaced;
/// every session intent goes through this.
pub(crate) trait ChatEndpoint: Send + Sync {
    /// A request on this endpoint's wire. The caller applies the timeout
    /// policy ([`rpc_timeout`]); a prompt waits for the turn to finish.
    fn request(&self, method: &str, params: Value) -> BoxFuture<'static, Result<Value, AcpError>>;
    fn notify(&self, method: &str, params: Value) -> Result<(), AcpError>;
    /// The wire id for a display id (identity or prefix-stripped).
    fn wire_id(&self, display: &str) -> String;
    /// Ready to accept a prompt now (handshake complete). Queues otherwise.
    fn accepts_prompt(&self) -> bool;
    /// `None` for the main connection; the peer label otherwise.
    fn label(&self) -> Option<String>;
    /// The session's own working directory, when the endpoint knows it (a
    /// peer's `session/list` carries it; the main path resolves it centrally).
    fn session_cwd(&self, _display: &str) -> Option<String> {
        None
    }
    /// Clear any per-load replay gates before a full in-place rebuild.
    fn reset_replay_gates(&self) {}
}

