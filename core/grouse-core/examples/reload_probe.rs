// SPDX-License-Identifier: AGPL-3.0-only
//
//! Prove what a chat's transcript actually holds after a FULL reload, against a
//! REAL goose server — the check for "full refresh doesn't grab/display the
//! latest messages, especially from another client".
//!
//! Phases, all printed as `-- ` lines:
//!   1. connect, refresh the session list, open SID (whatever mode `MODE` says),
//!   2. print the store: item count, window, and how many items mention MARKER,
//!   3. if MARKER is set and `WAIT_MS` is non-zero, announce `READY_FOR_WRITE`
//!      and sleep — the caller writes the marker through ANOTHER path (a second
//!      client, or a direct row insert) while this process holds the chat open,
//!   4. `refresh_session(Full)` (or `Quick` with MODE=quick), print the store
//!      again, and say whether the marker's row arrived.
//!
//!   KEY=<secret> HOST=127.0.0.1 PORT=3284 SID=<id> MARKER=<text> \
//!     cargo run --example reload_probe -p grouse-core
use grouse_core::{ConnectionStatus, Core, CoreListener, ItemKind, RefreshMode, ServerConfig};

struct L;

impl CoreListener for L {
    fn on_thread_timeline(&self, _label: String, _rows: Vec<grouse_core::TimelineRow>) {}
    fn on_status(&self, s: ConnectionStatus) {
        println!("-- status: {s:?}");
    }
    fn on_replay_progress(&self, sid: String, loaded: i64, total: i64, done: bool) {
        println!("-- replay_progress {sid}: {loaded}/{total} done={done}");
    }
    fn on_item(&self, _op: grouse_core::TranscriptOp) {}
    fn on_sessions(&self, _s: Vec<grouse_core::SessionSummary>) {}
    fn on_session_touched(&self, _i: String, _t: String, _u: String) {}
    fn on_usage(&self, _u: i64, _s: i64, _c: f64, _cur: String) {}
    fn on_run_ended(&self, _s: String, _r: String) {}
    fn on_speak(&self, _t: String) {}
    fn on_config(&self, _o: Vec<grouse_core::ConfigOption>) {}
    fn on_permission_request(&self, _r: grouse_core::PermissionRequest) {}
    fn on_projects(&self, _p: Vec<grouse_core::ProjectSummary>) {}
    fn on_roam_peer_status(&self, _a: String, _b: String) {}
    fn on_roam_sessions(&self, _a: String, _b: Vec<grouse_core::SessionSummary>) {}
    fn on_peer_new_session(&self, _a: String, _b: String) {}
    fn on_active_run(&self, _a: String, _b: String) {}
    fn on_commands(&self, _c: Vec<String>) {}
    fn on_session_open_failed(&self, a: String, b: String) {
        println!("-- OPEN FAILED {a}: {b}");
    }
    fn on_thread_ready(&self, _l: String, _s: String) {}
    fn on_tasks(&self, _t: String) {}
    fn on_file(&self, _e: grouse_core::FileEvent) {}
}

fn report(core: &Core, tag: &str, marker: &str) -> usize {
    let items = core.rich_transcript();
    let w = core.window();
    println!(
        "-- [{tag}] items={} window.has_older={} newest_id={}",
        items.len(),
        w.has_older,
        w.newest_id
    );
    for it in items.iter().rev().take(4) {
        let kind = match it.kind {
            ItemKind::User => "user",
            ItemKind::Agent => "agent",
            ItemKind::Thought => "thought",
            ItemKind::Tool => "tool",
            ItemKind::ToolGroup => "group",
            ItemKind::Chart => "chart",
            ItemKind::McpApp => "app",
            ItemKind::File => "file",
            ItemKind::Error => "error",
        };
        let body: String = it.text.chars().take(90).collect();
        println!("--   {kind} id={} :: {body}", it.id);
    }
    let hits = if marker.is_empty() {
        0
    } else {
        items
            .iter()
            .filter(|i| i.text.contains(marker) || i.output.contains(marker) || i.detail.contains(marker))
            .count()
    };
    println!("-- [{tag}] marker {marker:?} hits={hits}");
    hits
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn main() {
    let host = env("HOST", "127.0.0.1");
    let port: u16 = env("PORT", "3284").parse().unwrap();
    let key = env("KEY", "");
    let sid = env("SID", "");
    let marker = env("MARKER", "");
    let mode = env("MODE", "full");
    let use_tls = env("TLS", "1") != "0";
    let wait_ms: u64 = env("WAIT_MS", "0").parse().unwrap();
    assert!(!sid.is_empty(), "usage: SID=<session id> reload_probe");

    let cache_dir = env("CACHE_DIR", "/tmp/grouse-reloadprobe-cache");
    let other = env("OTHER", "");
    let core = Core::new(Box::new(L), cache_dir);
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls,
        accept_invalid_certs: true,
        ca_cert_pem: None,
        cwd: "/tmp".into(),
        auto_connect: false,
        client_id: "grouse-reloadprobe".into(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(4));

    println!("-- open_session({sid})");
    core.open_session(sid.clone());
    std::thread::sleep(std::time::Duration::from_secs(10));
    let before = report(&core, "open", &marker);

    // Warm the CACHE for SID the way a real client does: leave the chat (the core
    // writes the transcript with its stamp), then come back. A `Quick` refresh
    // then has a "settled, fresh" cache in front of it — the case that used to
    // suppress the replay and fetch nothing.
    if !other.is_empty() {
        println!("-- open_session({other}) to persist {sid}'s cache");
        core.open_session(other.clone());
        std::thread::sleep(std::time::Duration::from_secs(8));
        println!("-- back to {sid}");
        core.open_session(sid.clone());
        std::thread::sleep(std::time::Duration::from_secs(10));
        report(&core, "warm", &marker);
    }

    if wait_ms > 0 {
        println!("-- READY_FOR_WRITE (sleeping {wait_ms}ms)");
        std::thread::sleep(std::time::Duration::from_millis(wait_ms));
    }

    let m = if mode == "quick" { RefreshMode::Quick } else { RefreshMode::Full };
    println!("-- refresh_session({sid}, {mode})");
    core.refresh_session(sid.clone(), m);
    std::thread::sleep(std::time::Duration::from_secs(12));
    let after = report(&core, "reload", &marker);

    if !marker.is_empty() {
        println!(
            "{}: marker hits before={before} after={after}",
            if after > 0 { "PASS" } else { "FAIL" }
        );
    }
}
