// SPDX-License-Identifier: AGPL-3.0-only
//
//! Probe the My Day task path end to end against a REAL goose server:
//! connect, create a chat, then `tasks_refresh` (which mints the hidden
//! auto-mode session and calls assistant-state's `state_items` twice) and print
//! whatever `on_tasks` hands the client. This is the check that "app tool calls
//! require auto mode" is satisfied by the hidden session, and that the reply
//! shape is what the tab parses.
//!   KEY=<secret> cargo run --example tasks_probe -p grouse-core
//!   (HOST/PORT env: default 127.0.0.1:3284, TLS trust-all for self-signed)
use grouse_core::{ConnectionStatus, Core, CoreListener, ServerConfig};
use std::sync::Arc;

struct L(Arc<std::sync::Mutex<Vec<String>>>);
impl CoreListener for L {
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
    fn on_tasks(&self, tasks: String) {
        self.0.lock().unwrap().push(tasks.clone());
        println!("-- on_tasks: {tasks}");
    }
}

fn main() {
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("PORT")
        .unwrap_or("3284".into())
        .parse()
        .unwrap();
    let key = std::env::var("KEY").unwrap();
    let sink = Arc::new(std::sync::Mutex::new(Vec::new()));
    let core = Core::new(Box::new(L(sink.clone())), String::new());
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls: true,
        accept_invalid_certs: true,
        ca_cert_pem: None,
        cwd: "/tmp".into(),
        auto_connect: false,
        client_id: "grouse-tasksprobe".into(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(4));
    core.new_session(None, None);
    std::thread::sleep(std::time::Duration::from_secs(4));

    core.tasks_refresh();
    std::thread::sleep(std::time::Duration::from_secs(25));
    let got = sink.lock().unwrap().clone();
    println!("docs received: {}", got.len());
    if got.iter().any(|d| d.contains("\"errors\"")) {
        println!("FAIL: at least one section reported an error");
    } else {
        println!("OK: both sections parsed");
    }

    // The snooze regression: a snoozed row still arrives in `open` (server-side it is
    // open, only quiet), so `snooze_until` is the ONLY thing that lets a client group it
    // apart. Losing it in the projection is what made Snooze look like a no-op.
    let last = got.last().cloned().unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&last).unwrap_or_default();
    let snoozed: Vec<serde_json::Value> = v["open"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| r["state"] == "snoozed")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for r in &snoozed {
        println!("snoozed row: {} until {}", r["id"], r["snooze_until"]);
    }
    if snoozed.is_empty() {
        println!("NOTE: nothing snoozed on the server, so the field was not checked");
    } else if snoozed.iter().any(|r| r["snooze_until"].is_null()) {
        println!("FAIL: a snoozed row arrived without snooze_until");
    } else {
        println!("OK: snoozed rows carry snooze_until");
    }
}
