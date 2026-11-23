// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end spine tests against an in-process scripted WebSocket server
//! (the only wire-level protocol coverage; the desktop is a thin client over
//! the C ABI and has no C++ wire client to test).
//!
//! Every test builds its Core through `spine_harness()`, which owns a private
//! cache dir and a strict protocol oracle; see `test_support`. The fake server
//! replays ACP exactly enough to catch protocol drift (a misspelled method or
//! param fails the test instead of getting a harmless `{}`).

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use grouse_core::{
    ConfigChoice, ConfigOption, ConnectionStatus, Prompt, PromptBlock, SendExpect, TranscriptOp,
};
use serde_json::{Value, json};

mod test_support;
use test_support::*;

/// The fake server is a protocol ORACLE, not a permissive stub: a typo used to
/// get `{}` and every test still passed. Both halves are checked here — an
/// unscripted method and a misspelled required param — plus the recording that
/// fails the harness even when the core tolerates the unanswered request.
#[test]
fn strict_oracle_catches_misspelled_method_and_param() {
    let method_params = json!({ "sessionId": "sess-r" });
    let method_reason = check_request("session/laod", &method_params).unwrap_err();
    assert!(
        method_reason.contains("unknown RPC method"),
        "{method_reason}"
    );

    let typo_params = json!({
        "sessionID": "sess-r", // misspelled: goose drops unknown params silently
        "cwd": "/tmp",
        "mcpServers": []
    });
    let param_reason = check_request("session/load", &typo_params).unwrap_err();
    assert!(param_reason.contains("sessionId"), "{param_reason}");

    let log = Mutex::new(Vec::new());
    record_violation(&log, "session/laod", &method_params, &method_reason);
    record_violation(&log, "session/load", &typo_params, &param_reason);
    let found = log.lock();
    assert_eq!(found.len(), 2);
    assert!(found[0].contains("session/laod"), "{found:?}");
    assert!(found[1].contains("sessionId"), "{found:?}");
}

// ---------------------------------------------------------------------------
// Cache: a resume replay must persist the fresh transcript + updatedAt so the
// next open renders from cache instead of re-streaming the same delta
// ---------------------------------------------------------------------------

#[test]
fn resume_replay_persists_the_fresh_cache() {
    let h = spine_harness();
    // Seed a STALE cache: old content, old updatedAt.
    let cache = h.cache_store();
    let stale = vec![item("m-old", grouse_core::ItemKind::User, "stale cached line")];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z", true));

    let (_server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    // Stale cache for sess-r (no session/list yet -> unknown updatedAt ->
    // replay). The replay must end with the cache rewritten.
    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    // The probe (13:00) lands asynchronously after Ready; poll for it. It
    // wins over the replay's own session_info_update (12:00): the stamp is
    // deterministic and matches the session/list entry byte-for-byte, so the
    // next open is fresh across restarts.
    let (messages, updated_at, _complete) = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last: Option<(Vec<grouse_core::Item>, String, bool)> = None;
        while Instant::now() < deadline {
            if let Some(v) = cache.load_transcript("sess-r") {
                last = Some(v.clone());
                if v.1 == "2026-08-12T13:00:00.000Z" { break; }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        last.expect("cache written after the replay")
    };
    assert_eq!(updated_at, "2026-08-12T13:00:00.000Z", "cache stamped from the session/info probe");
    let text: String = messages.iter().map(|m| m.text.clone()).collect::<Vec<_>>().join("");
    assert!(text.contains("replayed line one and two"), "cache holds the replayed content: {text}");

    core.disconnect();
}

// ---------------------------------------------------------------------------
// grouse#19: a failed open must REPORT, never silently bind a new session
// ---------------------------------------------------------------------------

/// A live-wire resume the server rejects (stale/archived id) must not become a
/// fresh session behind the client's back. The client already moved its screen
/// to the tapped session; a silent substitute is what made the app show a blank
/// chat labelled with the selected one.
#[test]
fn rejected_resume_reports_and_binds_no_substitute_session() {
    let h = spine_harness();

    let (server, port) = h.server();
    server.fail_load.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));

    core.open_session("sess-gone".to_string());
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::OpenFailed(..)), "the open failure") {
        Ev::OpenFailed(session_id, message) => {
            assert_eq!(session_id, "sess-gone");
            assert!(
                message.contains("session/load"),
                "the report names the failing call: {message}"
            );
        }
        _ => panic!("expected OpenFailed from the rejected resume"),
    }

    // No substitute: only the connect's own transient session/new ever crossed
    // the wire, and nothing is left bound — a rejected load must not leave the
    // connection claiming a session it never loaded.
    assert_eq!(
        server.frames_for("session/new").len(),
        1,
        "the rejected resume must not create a session"
    );
    assert_eq!(
        core.active_session_id(),
        None,
        "a rejected resume binds no session"
    );

    core.disconnect();
}

/// The same rejection on the COLD path (the handshake's resume, what a
/// cold-start tap rides): report it, leave the wire with NO session, and never
/// hand the client a chat it did not ask for.
#[test]
fn cold_resume_rejection_reports_and_binds_nothing() {
    let h = spine_harness();

    let (server, port) = h.server();
    server.fail_load.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect_resume(
        server_config(port),
        "sess-gone".to_string(),
    );

    match wait_for(&ev_rx, |ev| matches!(ev, Ev::OpenFailed(..)), "the cold open failure") {
        Ev::OpenFailed(session_id, message) => {
            assert_eq!(session_id, "sess-gone");
            assert!(
                message.contains("session/load"),
                "the report names the failing call: {message}"
            );
        }
        _ => panic!("expected OpenFailed from the rejected resume"),
    }
    // The wire is still usable, so Ready follows — but with nothing bound.
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "ready after failure");
    assert!(server.frames_for("session/new").is_empty(), "no substitute session/new");
    assert_eq!(
        core.active_session_id(),
        None,
        "a rejected resume binds no session at all"
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// grouse#18: a replay is ingested silently and painted once
// ---------------------------------------------------------------------------

/// A reload must not walk the history from the top. While the server is still
/// streaming the replay the client sees nothing new; the load's completion paints
/// the newest window in ONE shot (Reset + rows + cursor), which is what lets a
/// refresh land on the most recent messages with scroll-back for the rest.
#[test]
fn a_replay_is_ingested_silently_and_painted_once() {
    let h = spine_harness();

    let (server, port) = h.server();
    server.gate_load.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    // The replay's rows are on the wire and the reply is held open: nothing they
    // contain may reach the client yet. (The open's own empty paint may.)
    std::thread::sleep(Duration::from_millis(400));
    let mut streamed = 0;
    while let Ok(ev) = ev_rx.try_recv() {
        if let Ev::Item(TranscriptOp::Upsert { .. }) = ev {
            streamed += 1;
        }
    }
    assert_eq!(streamed, 0, "the replay was painted row by row instead of held");

    // Release the reply: the paint lands as one batch, Reset first.
    server.release_load.notify_one();
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Item(_)), "the paint") {
        Ev::Item(TranscriptOp::Reset { .. }) => {}
        _ => panic!("the paint must start with a Reset"),
    }
    let mut upserts = 0;
    loop {
        match wait_for(&ev_rx, |ev| matches!(ev, Ev::Item(_)), "more of the paint") {
            Ev::Item(TranscriptOp::Upsert { .. }) => upserts += 1,
            Ev::Item(TranscriptOp::Window { .. }) => break,
            _ => panic!("unexpected op in the paint"),
        }
    }
    // The paint is exactly the newest window of what the store now holds — one
    // shot, not a stream of per-row updates.
    let expected = core.rich_transcript().len().min(60);
    assert!(expected >= 1, "the replay reached the store");
    assert_eq!(upserts, expected, "the paint carries the newest window, once");

    // The held rows did reach the store (silence is not loss).
    let text: String = core
        .transcript()
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("replayed line one and two"),
        "the silently ingested replay is in the transcript: {text}"
    );

    core.disconnect();
}

/// A reconnect that is only about the CONNECTION binds nothing, so the chat on
/// screen survives it.
///
/// Reported 2026-11-22: "when I leave the app and reopen it, it always opens
/// the last non-roam chat". The client reconnected the main wire on a foreground
/// return and named the last MAIN chat as the resume target; `connect_resume`
/// BINDS what it resumes, and binding is also DISPLAYING (`hub.display` +
/// `replace_rich`), so the whole transcript was repainted with that chat while
/// the client still thought it was showing a roam peer's. `connect_attach` is
/// that reconnect without the binding: the wire comes up, `session/list` works,
/// and the first explicit `open_session` binds.
#[test]
fn a_reconnect_that_binds_nothing_leaves_the_screen_alone() {
    let h = spine_harness();

    let (server, port) = h.server();
    *server.history.lock() = 3;

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    let config = || server_config(port);
    core.connect(config());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "first ready");

    // A chat is on screen — the thing a reconnect must not replace.
    core.open_session("sess-hist".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "open ready");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-hist"));
    let shown: Vec<String> = core.transcript().into_iter().map(|m| m.content).collect();
    assert!(!shown.is_empty(), "the chat has content to lose");
    let loads = server.frames_for("session/load").len();
    let news = server.frames_for("session/new").len();

    // The reconnect: up, and bound to nothing.
    core.connect_attach(config());
    assert!(core.connected(), "the attached wire is live");
    assert!(!core.ready(), "…and it is not 'ready' in CONTRACT §4's sense: no session");
    assert_eq!(
        core.active_session_id(),
        None,
        "attach must bind no session — anything else would be displayed over the chat on screen"
    );
    assert_eq!(
        core.transcript().into_iter().map(|m| m.content).collect::<Vec<_>>(),
        shown,
        "the displayed transcript is untouched"
    );
    assert_eq!(
        server.frames_for("session/load").len(),
        loads,
        "attach must not send a session/load"
    );
    assert_eq!(
        server.frames_for("session/new").len(),
        news,
        "attach must not mint a session either"
    );

    // …and the wire is fully usable: the connection-level calls work with no
    // session bound, and the first explicit open binds again.
    core.list_sessions();
    core.open_session("sess-hist".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "open after attach");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-hist"));

    // THE CONTRAST, which is the defect: resuming BINDS, and binding is
    // DISPLAYING. A reconnect that names a chat replaces whatever is on screen
    // with it — the "reopen the app and it opens the last non-roam chat" report,
    // where the client believed it was showing a peer's chat.
    *server.history.lock() = 0;
    core.connect_resume(config(), "sess-other".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");
    assert_eq!(
        core.active_session_id().as_deref(),
        Some("sess-other"),
        "a resume binds its target…"
    );
    assert_ne!(
        core.transcript().into_iter().map(|m| m.content).collect::<Vec<_>>(),
        shown,
        "…and binding repaints the screen with it, which is why the reconnect must attach"
    );

    core.disconnect();
}

/// The two reload strengths (grouse#18). QUICK lets freshness decide — a settled
/// cache means a bounded tail, not a replay of the whole session — while FULL
/// ignores the cache and always replays. Both are what the clients' "reload"
/// gesture and its hold variant call.
#[test]
fn refresh_modes_differ_on_the_wire() {
    let h = spine_harness();

    let (server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    // Open once so a settled cache exists for the reloads to reason about.
    core.open_session("sess-a".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "first open");

    core.refresh_session("sess-a".to_string(), grouse_core::RefreshMode::Quick);
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "quick reload ready");
    let quick = server
        .frames_for("session/load")
        .last()
        .cloned()
        .expect("a session/load for the quick reload");
    assert!(
        quick.pointer("/params/_meta/replayTail").is_some(),
        "a QUICK reload must let freshness pick the bounded path: {quick:?}"
    );

    core.refresh_session("sess-a".to_string(), grouse_core::RefreshMode::Full);
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "full reload ready");
    let full = server
        .frames_for("session/load")
        .last()
        .cloned()
        .expect("a session/load for the full reload");
    // A FULL reload is the escape hatch for a drifted local view, and it is
    // STAGED now: the newest turn is authoritative on the first round trip, and
    // the history behind it is re-fetched from the server page by page
    // (docs/TRANSCRIPT_MODEL.md, "Windows"). The cache is deleted first, so
    // nothing stale survives either way.
    assert_eq!(
        full
            .pointer("/params/_meta/replayTail")
            .and_then(Value::as_u64),
        Some(80),
        "a FULL reload must fetch the recent part first, not the whole session: {full:?}"
    );

    core.disconnect();
}

/// The open plan is the riskiest decision in the core (audit §7): the old
/// tests called the helpers with flags they chose themselves. This table
/// drives the REAL intents (`open_session`, `refresh_session`) over the fake
/// wire and checks the one thing the client can observe from the plan: the
/// `replayTail` on `session/load`, and whether the replay repainted a chat
/// that owns a live turn.
#[test]
fn open_plan_selection_is_table_driven_over_the_public_path() {
    #[derive(Clone, Copy)]
    enum PlanCase {
        StaleCache,
        FreshCache,
        MidTurnRefresh,
        BindOnlyReopen,
    }

    // Byte-for-byte the stamp the fake's `session/list` hands out. The info
    // probe must agree for the fresh path to keep its suppression.
    const LIST_STAMP: &str = "2026-08-12T00:00:00.000Z";

    for (name, case, expected_tail) in [
        ("stale cache", PlanCase::StaleCache, 100u64),
        ("fresh cache", PlanCase::FreshCache, 1u64),
        ("mid-turn full refresh", PlanCase::MidTurnRefresh, 1u64),
        ("bind-only reopen", PlanCase::BindOnlyReopen, 1u64),
    ] {
        let h = spine_harness();
        let (core, server, port, ev_rx) = wire!(h);
        let sid = "sess-e2e";
        let marker = format!("{name}: cached row");

        let live_turn = matches!(case, PlanCase::MidTurnRefresh | PlanCase::BindOnlyReopen);
        server
            .hold_prompts
            .store(live_turn, std::sync::atomic::Ordering::SeqCst);

        core.connect(server_config(port));
        wait_for(
            &ev_rx,
            |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)),
            "the connect's transient ready",
        );
        if matches!(case, PlanCase::FreshCache) {
            // The freshness baseline comes from session/list, not thin air.
            core.list_sessions();
            wait_for(&ev_rx, |ev| matches!(ev, Ev::Sessions(_)), "the listed stamp");
        }
        if live_turn {
            core.send_prompt(
                Prompt { blocks: vec![PromptBlock::Text { text: "live-turn".to_string() }] },
                Some(SendExpect { session_id: sid.to_string() }),
            );
            wait_for_frames(&server, "session/prompt", 1, "the in-flight turn");
        }
        while ev_rx.try_recv().is_ok() {}

        // Seed AFTER the connect: session/new binds sess-e2e and would otherwise
        // overwrite the seeded cache with an empty transcript on Ready. The
        // fresh case also needs the listed baseline (from session/list) first.
        if matches!(case, PlanCase::StaleCache | PlanCase::FreshCache) {
            let stamp = if matches!(case, PlanCase::FreshCache) {
                *server.info_stamp.lock() = LIST_STAMP.to_string();
                LIST_STAMP
            } else {
                "2026-01-01T00:00:00.000Z"
            };
            assert!(h.cache_store().save_transcript(
                sid,
                &[item("cached-1", grouse_core::ItemKind::Agent, &marker)],
                stamp,
                true,
            ));
        }

        match case {
            PlanCase::MidTurnRefresh => {
                core.refresh_session(sid.to_string(), grouse_core::RefreshMode::Full)
            }
            _ => core.open_session(sid.to_string()),
        }

        let frames = wait_for_frames(&server, "session/load", 1, "the open plan's load");
        let load = frames.last().expect("a session/load");
        assert_eq!(
            load.pointer("/params/_meta/replayTail").and_then(Value::as_u64),
            Some(expected_tail),
            "{name}: the plan asked for the wrong window: {load:?}"
        );

        // Watch the paint events while waiting for the load's Ready. A live
        // turn may be re-shown (repaint_window at bind, plus the replay's own
        // window repaint of that SAME store); what must never happen is a
        // replay row being painted over a store the server cannot reproduce.
        let mut resets = 0;
        let mut painted: Vec<String> = Vec::new();
        let mut take = |ev: Ev| {
            match ev {
                Ev::Item(TranscriptOp::Reset { .. }) => resets += 1,
                Ev::Item(TranscriptOp::Upsert { item }) => painted.push(item.text),
                _ => {}
            }
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match ev_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ev::Status(ConnectionStatus::Ready)) => break,
                Ok(ev) => take(ev),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => assert!(
                    Instant::now() < deadline,
                    "{name}: the open never finished"
                ),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("{name}: listener dropped before the open finished")
                }
            }
        }
        while let Ok(ev) = ev_rx.try_recv() {
            take(ev);
        }

        let text: String = core
            .rich_transcript()
            .iter()
            .map(|i| i.text.clone())
            .collect::<Vec<_>>()
            .join("");
        match case {
            PlanCase::StaleCache => assert!(
                text.contains("replayed line one and two"),
                "a stale open must replay: {text}"
            ),
            PlanCase::FreshCache => assert!(
                text.contains(marker.as_str()) && !text.contains("replayed line one"),
                "a fresh cache is authoritative: {text}"
            ),
            PlanCase::MidTurnRefresh | PlanCase::BindOnlyReopen => {
                assert!(
                    text.contains("live-turn") && !text.contains("replayed line one"),
                    "the live turn is the only copy of the reply: {text}"
                );
                assert!(
                    resets >= 1,
                    "{name}: the live store must have been re-shown on bind"
                );
                assert!(
                    painted.iter().any(|t| t.contains("live-turn"))
                        && !painted.iter().any(|t| t.contains("replayed line one")),
                    "{name}: only the live store may be painted, never the replay: {painted:?}"
                );
            }
        }
        core.disconnect();
    }
}

// ---------------------------------------------------------------------------
// Reverse loading: a tail first, then a page at a time back to the origin
// ---------------------------------------------------------------------------

#[test]
fn a_tail_first_open_pages_the_rest_of_the_history_from_the_wire() {
    let h = spine_harness();
    // docs/TRANSCRIPT_MODEL.md, "Windows". Opening a chat this device has not
    // seen used to stream its WHOLE history before anything appeared (the
    // Assistant thread: 4,221 messages). It now asks for a bounded tail, paints
    // the recent part, and pages the rest back on demand — through the server's
    // own `_meta.replayTail` / `_meta.replaySkipped` contract, which the fake
    // server here implements the way goose does.

    let (server, port) = h.server();
    *server.history.lock() = 300;

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-p".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "open ready");

    // 1. The open asked for a BOUNDED tail, not the whole conversation.
    let first = server
        .frames_for("session/load")
        .last()
        .cloned()
        .expect("the open's session/load");
    assert_eq!(
        first
            .pointer("/params/_meta/replayTail")
            .and_then(Value::as_u64),
        Some(80),
        "an uncached open must fetch the recent part first: {first:?}"
    );

    // 2. The newest window is on screen, and the cursor says there is more.
    let window = core.window();
    assert!(window.has_older, "the server still holds the history behind it");
    let rows = core.transcript();
    assert!(!rows.is_empty(), "the recent part painted");
    assert_eq!(
        rows.last().map(|m| m.content.clone()).unwrap_or_default(),
        "history row 299",
        "the NEWEST message is what shines first"
    );
    assert!(
        !rows.iter().any(|m| m.content == "history row 0"),
        "the origin is NOT fetched up front"
    );

    // 3. Scroll back until the origin. The first batches come from the STORE
    //    (the painted window's own tail); once it runs out, scrolling fetches a
    //    page from the wire — and each page asks for MORE trailing messages
    //    than the one before, because this server has no backwards cursor.
    let loads_before = server.frames_for("session/load").len();
    let mut guard = 0;
    while core.window().has_older {
        guard += 1;
        assert!(guard < 30, "paging must terminate");
        core.load_older(200);
        wait_for(
            &ev_rx,
            |ev| matches!(ev, Ev::Item(TranscriptOp::Window { .. })),
            "a batch or a page settled",
        );
    }
    let page_tails: Vec<u64> = server
        .frames_for("session/load")
        .iter()
        .skip(loads_before)
        .filter_map(|f| f.pointer("/params/_meta/replayTail"))
        .filter_map(Value::as_u64)
        .collect();
    assert!(
        !page_tails.is_empty(),
        "scrolling past the painted tail must fetch a page, not report the end"
    );
    assert!(
        page_tails.iter().all(|tail| *tail > 80),
        "a page always asks for more than the open did: {page_tails:?}"
    );
    // Growth is geometric, not a crawl: seven pages reach 4,221 messages (~2x
    // the transcript on the wire), where +120 a page would re-send it 17 times.
    let mut last = 80u64;
    for tail in &page_tails {
        assert!(
            *tail >= (last * 2).min(300),
            "each page at least doubles the previous ask ({last} -> {tail})"
        );
        last = *tail;
    }

    // 5. The whole conversation, in order, exactly once.
    let texts: Vec<String> = core.transcript().iter().map(|m| m.content.clone()).collect();
    assert_eq!(texts.len(), 300, "no duplicates, nothing missing");
    assert_eq!(texts[0], "history row 0", "the origin is the transcript's start");
    assert_eq!(texts[299], "history row 299");
    let unique: std::collections::HashSet<&String> = texts.iter().collect();
    assert_eq!(unique.len(), 300, "a page may not duplicate a row");

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Directory cache: a cold start emits the cached session names immediately
// ---------------------------------------------------------------------------

#[test]
fn cold_start_emits_cached_directory() {
    let h = spine_harness();
    let cache = h.cache_store();
    let sessions = vec![grouse_core::SessionSummary {
        mode: None,
        id: "sess-c".into(),
        title: "Cached Chat".into(),
        updated_at: "2026-08-12T10:00:00Z".into(),
        last_message_snippet: None,
        project_id: None,
        message_count: 3,
        model: String::new(),
        has_recipe: false,
        has_new: false,
        archived: false,
    }];
    let mut cwds = std::collections::BTreeMap::new();
    cwds.insert("sess-c".into(), "/tmp".into());
    assert!(cache.save_directory(&sessions, &cwds));

    // A brand-new Core (no connect yet) must emit the cached names right away.
    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Sessions(_)), "cached sessions") {
        Ev::Sessions(s) => {
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].id, "sess-c");
            assert_eq!(s[0].title, "Cached Chat");
        }
        _ => unreachable!(),
    }
    core.disconnect();
}

/// The Android regression: the UI supplies a real cache dir at construction
/// (context.filesDir). The env-based default resolves nowhere writable there,
/// so a core built without the dir silently cached nothing — the drawer was
/// empty on every cold start.
#[test]
fn cold_start_emits_cached_directory_from_supplied_dir() {
    let h = spine_harness();
    let cache = h.cache_store();
    let sessions = vec![grouse_core::SessionSummary {
        mode: None,
        id: "sess-d".into(),
        title: "Supplied Chat".into(),
        updated_at: "2026-08-13T11:00:00Z".into(),
        last_message_snippet: None,
        project_id: None,
        message_count: 2,
        model: String::new(),
        has_recipe: false,
        has_new: false,
        archived: false,
    }];
    assert!(cache.save_directory(&sessions, &std::collections::BTreeMap::new()));

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Sessions(_)), "cached sessions") {
        Ev::Sessions(s) => {
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].id, "sess-d");
            assert_eq!(s[0].title, "Supplied Chat");
        }
        _ => unreachable!(),
    }
    core.disconnect();
}

// ---------------------------------------------------------------------------
// Gap fixes: active-run + commands events, recipe on a cold-start connect
// ---------------------------------------------------------------------------

#[test]
fn spine_e2e_active_run_commands_and_recipe_connect() {
    let h = spine_harness();
    let (server, port) = h.server();
    server.notify.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);

    core.connect(grouse_core::ServerConfig {
        initial_recipe_id: Some("r-42".to_string()),
        initial_project_id: Some("proj-7".to_string()),
        ..server_config(port)
    });

    // The recipe rode the session/new call (cold-start recipe run, gap 4).
    let new_frames = server.frames_for("session/new");
    assert_eq!(new_frames.len(), 1, "exactly one session/new");
    assert_eq!(
        new_frames[0].pointer("/params/_meta/recipeId").and_then(Value::as_str),
        Some("r-42"),
        "recipeId must ride _meta on session/new"
    );
    assert_eq!(
        new_frames[0].pointer("/params/_meta/projectId").and_then(Value::as_str),
        Some("proj-7"),
        "projectId must ride _meta on session/new (grouse#5 P0)"
    );

    // The run-id event (gap 1) and the commands event (gap 2) arrived.
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::ActiveRun(..)), "active run") {
        Ev::ActiveRun(sid, run_id) => {
            assert_eq!(sid, "sess-e2e");
            assert_eq!(run_id, "run-abc");
        }
        _ => unreachable!(),
    }
    match wait_for(&ev_rx, |ev| matches!(ev, Ev::Commands(..)), "commands") {
        Ev::Commands(names) => assert_eq!(names, vec!["/compact", "/undo"]),
        _ => unreachable!(),
    }

    core.disconnect();
}

// ---------------------------------------------------------------------------
// End-to-end: connect -> ready -> send_prompt -> streamed chunk on the listener
// ---------------------------------------------------------------------------

/// Turns are concurrent PER SESSION: with the first chat's `session/prompt`
/// still unanswered, a prompt for a second chat on the same wire must go out.
/// The old global `prompting` latch queued it until the first turn ended.
#[test]
fn a_second_chat_prompts_while_the_first_turn_is_in_flight() {
    let h = spine_harness();

    let (server, port) = h.server();
    // Neither prompt is answered: the test observes the wire, not run ends.
    server
        .hold_prompts
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "first".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.frames_for("session/prompt").is_empty() {
        assert!(Instant::now() < deadline, "the first prompt never reached the wire");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A second chat on the same wire (the display moves; the first turn is
    // still in flight).
    core.open_session("sess-b".to_string());
    let deadline = Instant::now() + Duration::from_secs(10);
    while core.active_session_id().as_deref() != Some("sess-b") {
        assert!(Instant::now() < deadline, "sess-b never became the displayed chat");
        std::thread::sleep(Duration::from_millis(20));
    }

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "second".to_string() }] },
        Some(SendExpect { session_id: "sess-b".to_string() }),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.frames_for("session/prompt").len() < 2 {
        assert!(
            Instant::now() < deadline,
            "a turn in the first chat held the second chat's prompt"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let ids: Vec<String> = server
        .frames_for("session/prompt")
        .iter()
        .filter_map(|frame| {
            frame
                .pointer("/params/sessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert!(ids.contains(&"sess-e2e".to_string()), "{ids:?}");
    assert!(ids.contains(&"sess-b".to_string()), "{ids:?}");

    core.disconnect();
}

#[test]
fn spine_e2e_connect_prompt_stream() {
    let h = spine_harness();
    let (server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);

    // connect() blocks (bounded) until the handshake completes.
    core.connect(server_config(port));

    // Ready + the bound session + config.
    assert!(
        matches!(
            wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready"),
            Ev::Status(ConnectionStatus::Ready)
        ),
        "expected the connection to reach Ready"
    );
    assert!(core.ready(), "ready() should be true after the handshake");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));
    assert_eq!(
        core.config(),
        vec![
            ConfigOption {
                id: "provider".to_string(),
                value: "openai".to_string(),
                name: "Provider".to_string(),
                choices: vec![],
            },
            ConfigOption {
                id: "model".to_string(),
                value: "m-a".to_string(),
                name: "Model".to_string(),
                choices: vec![
                    ConfigChoice { value: "m-a".to_string(), name: "A".to_string() },
                    ConfigChoice { value: "m-b".to_string(), name: "B".to_string() },
                ],
            }
        ]
    );

    // The wire shape: initialize carried the goose client caps + the secret
    // key rode the upgrade header.
    assert_eq!(
        server.secret_header.lock().as_deref(),
        Some("test-secret"),
        "the custom transport must send X-Secret-Key on the upgrade"
    );
    let init = server.frames_for("initialize");
    assert_eq!(init.len(), 1, "exactly one initialize");
    assert_eq!(
        init[0]
            .pointer("/params/protocolVersion")
            .and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        init[0]
            .pointer("/params/clientCapabilities/_meta/goose/recipeParameterRequests")
            .and_then(Value::as_bool),
        Some(true),
        "session/new would hard-fail for parameterized recipes without this cap"
    );
    let new_session = server.frames_for("session/new");
    assert_eq!(
        new_session[0]
            .pointer("/params/_meta/client")
            .and_then(Value::as_str),
        Some("grouse-core-test")
    );

    // Prompt with the matching expect -> streams back on the listener.
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "hello".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );

    let chunk = wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::Item(TranscriptOp::Upsert { item }) if item.id == "m-1"),
        "agent item",
    );
    let Ev::Item(TranscriptOp::Upsert { item }) = chunk else {
        unreachable!()
    };
    assert_eq!(item.id, "m-1");
    assert!(item.text.contains("hello from fake goose"), "{:?}", item.text);

    let ended = wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::RunEnded(_, r) if r == "end_turn"),
        "RunEnded",
    );
    let Ev::RunEnded(_session, stop_reason) = ended else {
        unreachable!()
    };
    assert_eq!(stop_reason, "end_turn");

    // The accumulated transcript: user echo + one agent bubble.
    let transcript = core.transcript();
    let user = transcript
        .iter()
        .find(|message| message.role == "user")
        .expect("user bubble");
    assert_eq!(user.content, "hello");
    let agent = transcript
        .iter()
        .find(|message| message.role == "agent")
        .expect("agent bubble");
    assert_eq!(
        agent.content, "hello from fake goose and more",
        "same messageId accumulates into one bubble"
    );
    assert_eq!(agent.id, "m-1");

    // SendExpect rejection: a mismatched session is dropped, never mis-routed.
    let prompt_frames_before = server.frames_for("session/prompt").len();
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "stray".to_string() }] },
        Some(SendExpect { session_id: "some-other-chat".to_string() }),
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        server.frames_for("session/prompt").len(),
        prompt_frames_before,
        "a send against a stale UI session must not reach the wire"
    );

    // Explicit disconnect: no reconnect, terminal Disconnected.
    core.disconnect();
    let status = wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::Status(ConnectionStatus::Disconnected)),
        "Disconnected",
    );
    assert!(matches!(status, Ev::Status(ConnectionStatus::Disconnected)));
    assert!(!core.ready());
    // No session/prompt ever went out after the first one.
    assert_eq!(server.frames_for("session/prompt").len(), 1);
}

// ---------------------------------------------------------------------------
// A prompt's own run-end `session_info_update` must NOT trigger a resync replay
// of the turn the client just streamed. On a long chat that replay is the whole
// history reloading ("send a reply and the chat reloads from the top").
// ---------------------------------------------------------------------------

#[test]
fn prompt_does_not_resync_its_own_turn() {
    let h = spine_harness();
    let (server, port) = h.server();
    server
        .post_turn_touch
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);

    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "hello".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );
    wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::RunEnded(_, _)),
        "RunEnded",
    );

    // Event-driven wait: the server's run-end touch schedules a 1.5s debounce
    // whose FIRST observable effect is the `_goose/unstable/session/info`
    // probe. Waiting for that frame proves the debounce actually fired; no
    // production clock seam and no blind 2.5s sleep. A spurious resync would
    // clear the store and issue `session/load`, re-streaming the transcript.
    wait_for_frames(
        &server,
        "_goose/unstable/session/info",
        1,
        "the post-turn debounce probe",
    );
    assert!(
        server.frames_for("session/load").is_empty(),
        "the server's own run-end touch must not replay the session: {:?}",
        server.frames_for("session/load")
    );
    let agent_count = core
        .transcript()
        .iter()
        .filter(|m| m.content == "hello from fake goose and more")
        .count();
    assert_eq!(
        agent_count, 1,
        "the reply must appear once; a resync rebuild would duplicate it"
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// set_config_option updates merge IN PLACE: the changed option takes the new
// value while its choices list survives an update that carries none (the
// typed notification shape), and untouched options keep everything. A
// wholesale replace emptied both dropdowns with no way back.
// ---------------------------------------------------------------------------

#[test]
fn set_config_update_merges_and_preserves_choices() {
    let h = spine_harness();
    let (_server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);

    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "Ready");

    core.set_config_option("model".to_string(), "m-b".to_string());
    wait_for(
        &ev_rx,
        |ev| {
            matches!(ev, Ev::Config(options)
                if options.iter().any(|o| o.id == "model" && o.value == "m-b"))
        },
        "merged config",
    );

    let config = core.config();
    assert_eq!(config.len(), 2, "both options survive");
    let provider = config.iter().find(|o| o.id == "provider").expect("provider");
    assert_eq!(provider.value, "openai", "untouched option keeps its value");
    let model = config.iter().find(|o| o.id == "model").expect("model");
    assert_eq!(model.value, "m-b", "the update lands");
    assert_eq!(
        model.choices.len(),
        2,
        "an update without choices must not erase the initial list"
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Stale cache: painted for the connect, then REPLACED by the replay — not
// appended to. The replay's chunks open new bubbles (append_chunk only ever
// continues the currently-open one; it never matches an existing bubble by
// message id), so a painted cache left in place duplicates the transcript.
// ---------------------------------------------------------------------------

#[test]
fn stale_cache_is_painted_then_replaced_not_appended() {
    let h = spine_harness();
    let cache = h.cache_store();
    let stale = vec![item("m-old", grouse_core::ItemKind::Agent, "STALE-MARKER")];
    // An updatedAt the server will not agree with => stale => the load replays.
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z", true));

    let (_server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    // The paint is synchronous inside open_session, so by the time the intent
    // returns the stale rows are on screen — that is the point of painting.
    let painted: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(painted.contains("STALE-MARKER"), "cache must paint instantly, got: {painted}");

    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    let final_text: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(
        final_text.contains("replayed line one and two"),
        "the replay must land: {final_text}"
    );
    // The regression: the painted rows must be GONE, not sitting above the
    // replayed copy of the same conversation.
    assert!(
        !final_text.contains("STALE-MARKER"),
        "painted cache survived the replay (transcript duplicated): {final_text}"
    );
    assert_eq!(
        final_text.matches("replayed line one").count(),
        1,
        "replayed content must appear exactly once: {final_text}"
    );

    // The ordering guarantee, which is what actually bit on 2026-09-16: the
    // painted rows had been NEWER than the history the replay delivered, so an
    // append left a 19-hour-old message as the LAST row and the user answered
    // it. After a replay the last row must be the server's last row.
    let rows: Vec<String> = core.transcript().iter().map(|m| m.content.clone()).collect();
    assert_eq!(
        rows.last().map(String::as_str),
        Some("replayed line one and two"),
        "the newest row on screen must be the newest the server sent: {rows:?}"
    );

    core.disconnect();
}

#[test]
fn reconnect_replaces_the_live_transcript_not_appends() {
    let h = spine_harness();
    let cache = h.cache_store();
    let stale = vec![item("m-old", grouse_core::ItemKind::Agent, "STALE-MARKER")];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z", true));

    let (server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");
    let opened: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(opened.contains("replayed line one and two"), "open replay landed: {opened}");

    // A remote change bumps the session's updatedAt, so the on-disk cache
    // (stamped when the open finished) is now STALE — the reconnect plans a
    // merge. The store holds the LIVE transcript, not a painted cache, so there
    // is nothing to merge into: the replay must REPLACE it. (goose also
    // re-assigns ids on replay, so the replayed rows do not match the live ones.)
    std::thread::sleep(Duration::from_millis(300)); // let the open's stamp-save settle
    server.push_notification(json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "sess-r",
            "update": {
                "sessionUpdate": "session_info_update",
                "title": "Remote",
                "updatedAt": "2026-08-12T15:00:00.000Z"
            }
        }
    }));
    std::thread::sleep(Duration::from_millis(150)); // let the core ingest it
    server.replay_alt_id.store(true, std::sync::atomic::Ordering::SeqCst);

    // Key on a fresh `session/load` rather than a `Ready`: the open itself emits
    // a Ready, so a Ready wait could be satisfied without any reconnect.
    let loads_before = server.frames_for("session/load").len();
    server.drop_connections();
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.frames_for("session/load").len() <= loads_before {
        assert!(Instant::now() < deadline, "the core never reconnected and reloaded");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(500));

    let after: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert_eq!(
        after.matches("replayed line one").count(),
        1,
        "reconnect must REPLACE the live rows, not append a copy: {after}"
    );
    assert!(!after.contains("STALE-MARKER"), "the stale paint must not survive: {after}");

    core.disconnect();
}

/// grouse#1: a prompt typed while the socket is down (the reconnect window)
/// must be QUEUED, not fired into the dead wire — and must reach the resumed
/// session once Ready arrives, with no substitute session created.
#[test]
fn prompt_while_reconnecting_is_queued_and_flushed_after_ready() {
    let h = spine_harness();

    let (server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");
    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "open ready");
    let sessions_before = server.frames_for("session/new").len();

    // Drop the wire and WAIT until the core has observed it: sending before the
    // drop is noticed would hit the still-live socket, which is a different
    // (and valid) path — this test is about the window after.
    server.drop_connections();
    wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::Status(ConnectionStatus::Error { .. })),
        "the drop is observed",
    );

    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "typed-while-down".to_string() }] },
        Some(SendExpect { session_id: "sess-r".to_string() }),
    );
    // Nothing may go out on the dead wire: the prompt is queued instead.
    assert_eq!(
        server.frames_for("session/prompt").len(),
        0,
        "a prompt in the reconnect window must not be fired at the dead socket"
    );

    // Ready flushes it to the RESUMED session (no fresh session/new).
    let deadline = Instant::now() + Duration::from_secs(15);
    while server.frames_for("session/prompt").is_empty() {
        assert!(Instant::now() < deadline, "the queued prompt never reached the resumed wire");
        std::thread::sleep(Duration::from_millis(50));
    }
    let frame = server.frames_for("session/prompt")[0].clone();
    assert_eq!(frame.pointer("/params/sessionId").and_then(Value::as_str), Some("sess-r"));
    assert_eq!(
        frame.pointer("/params/prompt/0/text").and_then(Value::as_str),
        Some("typed-while-down")
    );
    assert_eq!(
        server.frames_for("session/new").len(),
        sessions_before,
        "the reconnect must resume the session, not substitute a new one"
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Phase 1: the rich item transcript paints the cache, then the replay replaces
// it — with the row's KIND intact (a paint is lossless now).
// ---------------------------------------------------------------------------

#[test]
fn rich_transcript_paints_and_replaces_on_a_stale_open() {
    let h = spine_harness();
    let cache = h.cache_store();
    let stale = vec![item("m-old", grouse_core::ItemKind::Agent, "STALE-MARKER")];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z", true));

    let (_server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    // The paint is synchronous: the rich snapshot carries the item's kind, and
    // the window cursor points at it.
    let painted = core.rich_transcript();
    assert_eq!(painted.len(), 1, "one painted item");
    assert_eq!(painted[0].kind, grouse_core::ItemKind::Agent);
    assert_eq!(painted[0].text, "STALE-MARKER");
    assert_eq!(core.window().oldest_id, "m-old");
    assert!(!core.window().has_older, "the whole cache is the window for now");

    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    let items = core.rich_transcript();
    let text: String = items.iter().map(|i| i.text.clone()).collect::<Vec<_>>().join("");
    assert!(text.contains("replayed line one and two"), "replay lands: {text}");
    assert!(!text.contains("STALE-MARKER"), "paint replaced, not appended: {text}");
    // item(id) resolves the row from the id index.
    let replayed = items.iter().find(|i| i.text.contains("replayed line one")).unwrap();
    assert_eq!(core.item(replayed.id.clone()).unwrap().kind, grouse_core::ItemKind::Agent);

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Phase 1: a stale cache is caught up with a BOUNDED TAIL that merges into the
// painted prefix, instead of replaying the whole session from the top.
// ---------------------------------------------------------------------------

#[test]
fn stale_cache_tail_merge_keeps_the_older_prefix() {
    let h = spine_harness();
    // Same harness as stale_cache_is_painted_then_replaced_not_appended, but the
    // cache CONTAINS the replay's first id, so the bounded tail anchors into it:
    // the older prefix survives and the replayed row lands exactly once.
    let cache = h.cache_store();
    let stale = vec![
        item("old-1", grouse_core::ItemKind::User, "VERY-OLD-PREFIX"),
        item("r-1", grouse_core::ItemKind::Agent, "replayed line one and two"),
    ];
    assert!(cache.save_transcript("sess-r", &stale, "2026-01-01T00:00:00.000Z", true));

    let (server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    core.open_session("sess-r".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "resume ready");

    // The open asked for the bounded tail, not a full replay.
    let loads = server.frames_for("session/load");
    assert!(
        loads.iter().any(|f| {
            f.pointer("/params/_meta/replayTail").and_then(Value::as_u64) == Some(100)
        }),
        "a stale-cache open must request the bounded tail: {loads:?}"
    );

    let rows: Vec<String> = core.transcript().iter().map(|m| m.content.clone()).collect();
    let joined = rows.join(" | ");
    assert!(joined.contains("VERY-OLD-PREFIX"), "the older prefix must survive: {joined}");
    assert!(joined.contains("replayed line one and two"), "the tail must land: {joined}");
    assert_eq!(
        joined.matches("replayed line one").count(),
        1,
        "the replayed row must not duplicate: {joined}"
    );
    let prefix = rows.iter().position(|t| t.contains("VERY-OLD-PREFIX"));
    let tail = rows.iter().position(|t| t.contains("replayed line one"));
    assert!(prefix < tail, "the older prefix must precede the tail: {joined}");

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Session switch: opening A, then B, then back to A must reuse A's cache and
// merge a bounded tail — re-streaming the whole chat from the top is the bug.
// ---------------------------------------------------------------------------

#[test]
fn reopening_a_cached_session_merges_a_tail_instead_of_full_replay() {
    let h = spine_harness();

    let (server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");

    // Open A (no cache yet → the first load may be a full replay); its transcript
    // is cached on Ready.
    core.open_session("sess-a".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "A ready");
    assert!(
        h.cache_dir().join("sess-a.json").exists(),
        "opening a session must persist its transcript for the next open"
    );

    // Away to B, then back to A.
    core.open_session("sess-b".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "B ready");

    core.open_session("sess-a".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "A again ready");

    // The reopen asked for the bounded tail, NOT a full replay.
    let last = server
        .frames_for("session/load")
        .last()
        .cloned()
        .expect("a session/load for the reopen");
    assert_eq!(
        last.pointer("/params/_meta/replayTail").and_then(Value::as_u64),
        Some(100),
        "reopening a cached session must merge a bounded tail, not replay from the top: {last:?}"
    );
    // And it still shows the conversation.
    let text: String = core
        .rich_transcript()
        .iter()
        .map(|i| i.text.clone())
        .collect::<Vec<_>>()
        .join("");
    assert!(text.contains("replayed line one and two"), "reopen shows content: {text}");

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Cold start: the painted cache belongs to the session it came from, NOT to the
// throwaway session the first connect creates on the way to resuming it.
// ---------------------------------------------------------------------------

#[test]
fn cold_start_paint_is_not_filed_under_the_transient_session() {
    let h = spine_harness();
    let cache = h.cache_store();
    let seeded = vec![item("m-1", grouse_core::ItemKind::Agent, "OWNED-BY-SESS-R")];
    assert!(cache.save_transcript("sess-r", &seeded, "2026-01-01T00:00:00.000Z", true));

    let (_server, port) = h.server();

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);

    // The cold-start sequence the UI runs: paint the last chat, THEN connect.
    // connect() creates a throwaway session before the real resume.
    core.load_cached_transcript("sess-r".to_string());
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");
    // save_cache runs on ready, and probe_stamp_and_save follows asynchronously.
    std::thread::sleep(Duration::from_millis(600));

    // sess-e2e is the throwaway the fake server hands back from session/new.
    assert!(
        cache.load_transcript("sess-e2e").is_none(),
        "the painted rows were filed under the throwaway session"
    );

    // And nothing else picked them up either: exactly one transcript file, the
    // one they came from.
    let transcripts: Vec<String> = std::fs::read_dir(cache.dir())
        .expect("cache dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".json") && n != "directory.json" && !n.ends_with("-tools.json"))
        .collect();
    assert_eq!(
        transcripts,
        vec!["sess-r.json".to_string()],
        "one transcript file, owned by the session it came from"
    );

    // The paint itself must survive — it is what the user is looking at.
    let painted: String =
        core.transcript().iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("");
    assert!(painted.contains("OWNED-BY-SESS-R"), "paint must stay on screen: {painted}");

    core.disconnect();
}

// ---------------------------------------------------------------------------
// session/list is PAGED — following the cursor is what keeps a project's chats
// ---------------------------------------------------------------------------

#[test]
fn session_list_follows_the_cursor_instead_of_stopping_at_the_first_page() {
    let h = spine_harness();
    // goose pages `session/list` at 50 rows ordered by last message. This box's
    // Assistant thread chats and hourly scheduled runs fill that page on their own, so
    // the sessions a project view is built from fell off the end: the Media project
    // showed "Games" alone where it used to show Movies and Music too (2026-11-08).
    // The cursor was always in the reply; nothing followed it.

    let (server, port) = h.server();
    server
        .page_lists
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "ready");
    core.list_sessions();

    let ids: Vec<String> = match wait_for(&ev_rx, |ev| matches!(ev, Ev::Sessions(_)), "session list") {
        Ev::Sessions(list) => list.into_iter().map(|s| s.id).collect(),
        _ => Vec::new(),
    };
    assert_eq!(
        ids,
        vec!["sess-e2e".to_string(), "sess-old".to_string()],
        "both pages must arrive, in the server's order"
    );
    // And the cursor is what asked for it: one request without, one with. The
    // list also probes each NON-DEFAULT mode (a tag filter), because a session row
    // never carries its tags back — those are separate requests, so count the
    // unfiltered walk only.
    let lists: Vec<_> = server
        .frames_for("session/list")
        .into_iter()
        .filter(|frame| frame.pointer("/params/_meta/tags").is_none())
        .collect();
    assert_eq!(lists.len(), 2, "one page per request: {lists:?}");
    assert_eq!(
        lists[1].pointer("/params/cursor").and_then(Value::as_str),
        Some("page-2")
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// Run summaries (issue #13): a turn's tool run is labelled by the model
// ---------------------------------------------------------------------------

#[test]
fn a_tool_run_gets_a_model_written_label() {
    let h = spine_harness();

    let (server, port) = h.server();
    server.tool_turn.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, _ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "hi".to_string() }] },
        None,
    );

    // The run's provisional label comes from the thinking it absorbed; the
    // model's rewrite lands after the turn ends, on the hidden session.
    let mut label = String::new();
    let mut thought = String::new();
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(100));
        let run = core.rich_transcript().into_iter().find(|i| {
            matches!(i.kind, grouse_core::ItemKind::Tool | grouse_core::ItemKind::ToolGroup)
        });
        if let Some(run) = run {
            label = run.summary.clone();
            thought = run.thought.clone();
        }
        if label == "Look up the charging issue" {
            break;
        }
    }
    assert_eq!(label, "Look up the charging issue", "the model's rewrite must land");
    // The absorbed thinking is what the expanded chip still shows.
    assert_eq!(thought, "Let me look up the charging issue.");

    // The summarizer session is a HIDDEN one: asked for with `_meta.hidden`, and
    // without `_meta.client` (so it is not a user session either).
    let new_frames = server.frames_for("session/new");
    assert!(
        new_frames
            .iter()
            .any(|f| f.pointer("/params/_meta/hidden").and_then(Value::as_bool) == Some(true)),
        "the summarizer must ask for a hidden session: {new_frames:?}"
    );

    // Its context must not grow turn on turn: the core empties it after use.
    let truncations = server.frames_for("_goose/unstable/session/conversation/truncate");
    assert_eq!(truncations.len(), 1, "one truncate per summarization");
    assert_eq!(
        truncations[0].pointer("/params/truncateFrom").and_then(Value::as_i64),
        Some(0)
    );

    core.disconnect();
}

// ---------------------------------------------------------------------------
// A prompt is NOT echoed on the wire (goose sends no `user_message_chunk` for a
// prompt we sent — it next crosses the wire in a `session/load` replay). So the
// core keeps its own copy. Without it the transcript, and the cache saved from
// it, holds no record of what was asked: a reopen that trusts the cache shows
// the replies with the questions missing.
// ---------------------------------------------------------------------------

#[test]
fn a_prompt_survives_a_reopen_because_the_core_echoes_it() {
    let h = spine_harness();

    let (server, port) = h.server();
    // The server's run-end touch, so the freshness stamp moves exactly as it does
    // against a live serve.
    server.post_turn_touch.store(true, std::sync::atomic::Ordering::SeqCst);

    let config = server_config(port);

    // --- Ask something and let the turn finish. ---
    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(config.clone());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "ready");
    core.send_prompt(
        Prompt { blocks: vec![PromptBlock::Text { text: "what did I ask?".to_string() }] },
        Some(SendExpect { session_id: "sess-e2e".to_string() }),
    );
    wait_for(&ev_rx, |ev| matches!(ev, Ev::RunEnded(_, _)), "RunEnded");

    // No user chunk ever arrived (the fake no longer invents one), so this row is
    // the CORE's echo — the only record of the question.
    let asked = core
        .rich_transcript()
        .into_iter()
        .find(|i| i.kind == grouse_core::ItemKind::User)
        .expect("the core echoes the prompt it sent");
    assert_eq!(asked.text, "what did I ask?");

    // And the cache SAVED FROM IT records the question. This is the claim that
    // matters: a reopen that trusts this file must still show what was asked.
    //
    // POLLED, not read once: the save runs after the `session/info` freshness
    // probe (see `probe_stamp_and_save`), so the write lands AFTER RunEnded and a
    // single read can lose that race — it did, on CI. Same bounded wait the other
    // cache assertions in this file use.
    let cache_path = h.cache_dir().join("sess-e2e.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    let cache_file = loop {
        if let Ok(body) = std::fs::read_to_string(&cache_path) {
            break body;
        }
        assert!(
            Instant::now() < deadline,
            "the turn's transcript was never cached at {cache_path:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        cache_file.contains("what did I ask?"),
        "the saved transcript must record the question: {cache_file}"
    );
    core.disconnect();

    // --- Reopen, as a fresh launch would. ---
    let (ev_tx2, ev_rx2) = h.listener();
    let core2 = h.core(ev_tx2);
    core2.connect_resume(config, "sess-e2e".to_string());
    wait_for(&ev_rx2, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "ready again");
    std::thread::sleep(Duration::from_millis(600)); // let the paint/replay settle

    let after: Vec<String> = core2
        .rich_transcript()
        .into_iter()
        .map(|i| format!("{:?}:{}", i.kind, i.text))
        .collect();
    assert!(
        after.iter().any(|r| r.contains("what did I ask?")),
        "the question must survive a reopen: {after:?}"
    );

    core2.disconnect();
}


// ---------------------------------------------------------------------------
// 2026-11-22 regression cluster: superseded handshakes, bindless reconnect,
// and a verification that downgrades a fresh-cache paint to a merge
// ---------------------------------------------------------------------------

/// An attach wire's unexpected drop must redial with an ATTACH spec, not a
/// resume: no `session/load`, no `session/new`, and no session bound. The core
/// owns the reconnect budget, so this is where the bind==display invariant has
/// to hold even when the client never asks for a reconnect itself.
#[test]
fn an_attach_reconnect_restores_the_wire_without_binding() {
    let h = spine_harness();

    let (server, port) = h.server();
    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    let config = || server_config(port);

    core.connect_attach(config());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "attach ready");
    assert!(core.connected());
    assert!(!core.ready(), "attach has a wire but no open session");
    assert!(server.frames_for("session/load").is_empty());
    assert!(server.frames_for("session/new").is_empty());
    while ev_rx.try_recv().is_ok() {}

    // Drop the socket out from under the core. Its own backoff must bring a
    // SECOND wire up, and that wire must also be bindless.
    server.drop_connections();
    wait_for_frames(&server, "initialize", 2, "the attach wire's reconnect");
    wait_for(
        &ev_rx,
        |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)),
        "reconnected ready",
    );
    assert!(core.connected(), "the reconnect brought the wire back");
    assert_eq!(core.active_session_id(), None, "a reconnect without a displayed chat must not bind one");
    assert!(
        server.frames_for("session/load").is_empty(),
        "reconnect must not resume (and repaint) a session: {:#?}",
        server.frames_for("session/load")
    );
    assert!(
        server.frames_for("session/new").is_empty(),
        "reconnect must not mint a throwaway session"
    );

    core.disconnect();
}

/// A handshake held at `initialize` is superseded by a newer connect. When the
/// stale reply finally lands, that wire must not load, bind, emit config, or
/// emit Ready — every one of those would overwrite the newer wire's state.
#[test]
fn a_superseded_handshake_never_binds_or_emits_ready() {
    let h = spine_harness();

    let (server, port) = h.server();
    server.hold_first_initialize.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    let config = || server_config(port);

    // The OLD connect is parked inside initialize. Its `connect_resume` would
    // bind `sess-old` and emit config if the shutdown flag were ignored.
    let old_core = core.clone();
    let old_config = config();
    let old = std::thread::spawn(move || {
        old_core.connect_resume(old_config, "sess-old".to_string());
    });
    wait_for_frames(&server, "initialize", 1, "the old handshake's initialize");

    // The NEW connect shuts the old wire down and owns the status story.
    core.connect(config());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ready = 0;
    while Instant::now() < deadline {
        match ev_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ev::Status(ConnectionStatus::Ready)) => { ready += 1; break; }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    assert_eq!(ready, 1, "only the new wire may emit Ready");

    // Let the stale initialize answer land. A cancelled wire must stop before
    // session/load; `shutdown()` also has to wake its bounded connect() now.
    server.release_initialize.notify_one();
    old.join().expect("the superseded connect must not panic");
    std::thread::sleep(Duration::from_millis(400));

    assert!(
        server.frames_for("session/load").is_empty(),
        "a superseded handshake must never load a session: {:#?}",
        server.frames_for("session/load")
    );
    assert_eq!(
        server.frames_for("session/new").len(),
        1,
        "only the new connect may mint its transient session"
    );
    let mut late_ready = 0;
    let mut stale_configs = 0;
    for ev in ev_rx.try_iter() {
        match ev {
            Ev::Status(ConnectionStatus::Ready) => late_ready += 1,
            Ev::Config(options) if options.len() < 2 => stale_configs += 1,
            _ => {}
        }
    }
    assert_eq!(late_ready, 0, "the stale wire emitted Ready after shutdown");
    assert_eq!(stale_configs, 0, "the stale wire emitted config after shutdown");
    assert_eq!(core.active_session_id().as_deref(), Some("sess-e2e"));

    core.disconnect();
}

/// The paint/verify duplicate-tail case at the wire level: the cache looks
/// FRESH against `session/list`, so it is painted authoritatively, but the
/// `_goose/unstable/session/info` probe says another client moved the session.
/// The ask is downgraded to a bounded merge tail; that tail must replace the
/// stale suffix rather than append a second copy of the newest turn.
#[test]
fn a_verification_downgrade_does_not_duplicate_the_replayed_tail() {
    let h = spine_harness();

    let cache = h.cache_store();
    let cached = vec![
        item("old-1", grouse_core::ItemKind::User, "CACHED-PREFIX"),
        item("r-1", grouse_core::ItemKind::Agent, "replayed line one and two"),
    ];
    // Byte-for-byte the listed `lastMessageAt` below: the paint plan calls it
    // fresh, but the session/info probe (13:00) refutes it.
    assert!(cache.save_transcript(
        "sess-fresh",
        &cached,
        "2026-08-12T06:00:00Z",
        true
    ));

    let (server, port) = h.server();
    server.fresh_session_row.store(true, std::sync::atomic::Ordering::SeqCst);

    let (ev_tx, ev_rx) = h.listener();
    let core = h.core(ev_tx);
    core.connect(server_config(port));
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "transient ready");
    core.list_sessions();
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Sessions(_)), "the fresh session baseline");

    core.open_session("sess-fresh".to_string());
    wait_for(&ev_rx, |ev| matches!(ev, Ev::Status(ConnectionStatus::Ready)), "verified open ready");

    // The probe refuted the fresh verdict, so the wire asked for the bounded
    // merge tail — not one message (suppression) and not a full rebuild.
    let loads = server.frames_for("session/load");
    let last = loads.last().expect("the open's session/load");
    assert_eq!(
        last.pointer("/params/_meta/replayTail").and_then(Value::as_u64),
        Some(100),
        "verification downgrade must merge a tail: {last:?}"
    );

    let text: String = core
        .transcript()
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("CACHED-PREFIX"),
        "the older cached prefix must survive the merge: {text}"
    );
    assert_eq!(
        text.matches("replayed line one").count(),
        1,
        "the merged tail replaced the stale suffix instead of appending it: {text}"
    );

    core.disconnect();
}
