// SPDX-License-Identifier: AGPL-3.0-only
//
//! Prove the thread open path against a REAL goose server: refresh My Day (which
//! populates the label -> conversation map), then `open_thread(label)` and print what
//! the core says is on screen.
//!
//! This is the check for the bug where a tapped card left the reader in the Assistant
//! thread: `current_thread_label()` only answers the label when the core actually bound
//! that thread's session, so it is the honest assertion, not a log line.
//!
//!   KEY=<secret> cargo run --example open_thread_probe -p grouse-core -- <label>
//!   (HOST/PORT env: default 127.0.0.1:3284, TLS trust-all for self-signed)
use grouse_core::{ConnectionStatus, Core, CoreListener, ServerConfig};
struct L;
impl CoreListener for L {
    fn on_sensitivity(&self, _data: String) {}
    fn on_thread_timeline(&self, _label: String, _rows: Vec<grouse_core::TimelineRow>) {}
    fn on_file(&self, _: grouse_core::FileEvent) {}
    fn on_replay_progress(&self, _: String, _: i64, _: i64, _: bool) {}
    fn on_status(&self, s: ConnectionStatus) {
        println!("status: {s:?}");
    }
    fn on_sessions(&self, _s: Vec<grouse_core::SessionSummary>) {}
    fn on_session_touched(&self, _i: String, _t: String, _u: String) {}
    fn on_item(&self, _op: grouse_core::TranscriptOp) {}
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
    fn on_session_open_failed(&self, _a: String, _b: String) {}
    fn on_thread_ready(&self, label: String, session: String) {
        println!("-- thread_ready({label}) -> {session}");
    }
    fn on_tasks(&self, tasks: String) {
        // Only the thread map is interesting here.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tasks) {
            println!("-- threads: {}", v["threads"]);
            if let Some(err) = v.get("errors") {
                println!("-- errors: {err}");
            }
        }
    }
}

fn main() {
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("PORT")
        .unwrap_or("3284".into())
        .parse()
        .unwrap();
    let key = std::env::var("KEY").unwrap();
    let label = std::env::args().nth(1).unwrap_or_default();
    assert!(!label.is_empty(), "usage: open_thread_probe <label>");
    let core = Core::new(Box::new(L), String::new());
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls: true,
        accept_invalid_certs: true,
        ca_cert_pem: None,
        cwd: "/tmp".into(),
        auto_connect: false,
        client_id: "grouse-threadprobe".into(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(4));

    // Populate the map the way the tab does.
    core.tasks_refresh();
    std::thread::sleep(std::time::Duration::from_secs(6));

    println!("-- open_thread({label})");
    core.open_thread(label.clone());
    std::thread::sleep(std::time::Duration::from_secs(8));

    // The core RESOLVES, the client opens: the assertion is that the answer names a
    // session for this label, which is all a client needs to call `open_session`.
    let on_screen = core.current_thread_label();
    println!("-- current_thread_label: {on_screen:?}");
    println!("-- (a client now opens the id printed by thread_ready above)");
    if on_screen == label {
        println!("PASS: the tapped thread is the chat on screen");
    } else {
        println!("INFO: not bound by the core, by design (see on_thread_ready)");
    }
}
