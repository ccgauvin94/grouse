// SPDX-License-Identifier: AGPL-3.0-or-later

//! Smoke-test the core's wire against a REAL goose server (used for the
//! Android login + cache debugging).
//! Usage: KEY=<secret> cargo run --example server_smoke -- <host> <port>
//!
//! Drives: connect (transient) -> list_sessions -> open first session ->
//! prints the session/list updatedAt vs the session_info_update updatedAt
//! (the cache-freshness comparison sources).
use grouse_core::{ConnectionStatus, Core, CoreListener, ServerConfig};
use std::sync::Arc;

struct L(Arc<std::sync::Mutex<Vec<String>>>);
impl CoreListener for L {
    fn on_status(&self, s: ConnectionStatus) {
        println!("status: {s:?}");
        self.0.lock().unwrap().push(format!("{s:?}"));
    }
    fn on_sessions(&self, sessions: Vec<grouse_core::SessionSummary>) {
        println!("-- on_sessions ({}):", sessions.len());
        for s in &sessions {
            println!("   list: id={} updatedAt={}", s.id, s.updated_at);
        }
        if let Some(first) = sessions.first() {
            let mut v = self.0.lock().unwrap();
            if !v.iter().any(|l| l.starts_with("FIRST:")) {
                v.push(format!("FIRST:{}", first.id));
            }
        }
    }
    fn on_session_touched(&self, session_id: String, title: String, updated_at: String) {
        println!("-- touched: id={} title={:?} updatedAt={}", session_id, title, updated_at);
    }
    fn on_transcript(&self, _e: grouse_core::TranscriptEvent) {}
    fn on_stream(&self, _e: grouse_core::StreamEvent) {}
    fn on_config(&self, _o: Vec<grouse_core::ConfigOption>) {}
    fn on_permission_request(&self, _r: grouse_core::PermissionRequest) {}
    fn on_projects(&self, _p: Vec<grouse_core::ProjectSummary>) {}
    fn on_roam_peer_status(&self, _a: String, _b: String) {}
    fn on_roam_sessions(&self, _a: String, _b: Vec<grouse_core::SessionSummary>) {}
    fn on_peer_new_session(&self, _a: String, _b: String) {}
    fn on_active_run(&self, _a: String, _b: String) {}
    fn on_commands(&self, _c: Vec<String>) {}
}

struct UL;
impl grouse_core::GrouseUnstableListener for UL {
    fn on_export(&self, _d: String) {}
    fn on_recipe_params(&self, _p: String) {}
    fn on_elicitation(&self, _s: String) {}
    fn on_compaction_status(&self, _m: String) {}
    fn on_message_usage(&self, _o: u64, _e: u64, _t: u64, _c: f64) {}
    fn on_app_resource(&self, _k: String, _h: String) {}
    fn on_recipes(&self, _r: String) {}
    fn on_schedules(&self, _s: String) {}
    fn on_projects(&self, _p: String) {}
    fn on_skills(&self, _s: String) {}
    fn on_tools(&self, _s: String, _t: String) {}
    fn on_extensions(&self, _e: String) {}
    fn on_session_extensions(&self, _s: String, _e: String) {}
    fn on_config_value(&self, _k: String, _v: String) {}
    fn on_supported_models(&self, _p: String, _m: String) {}
    fn on_providers(&self, _providers: String) {}
    fn on_session_probe(&self, session_id: String, updated_at: String, message_count: i64) {
        println!("-- probe: id={} updatedAt={} count={}", session_id, updated_at, message_count);
    }
    fn on_tool_result(&self, _t: String, _e: bool) {}
    fn on_error(&self, _m: String, _e: String) {}
}

fn main() {
    let host = std::env::args().nth(1).unwrap_or_else(|| "goose.gauvin.id".into());
    let port = std::env::args().nth(2).unwrap_or_else(|| "443".into()).parse().unwrap();
    let key = std::env::var("KEY").unwrap_or_else(|_| "dummy-key".into());
    println!("connecting to wss://{host}:{port}/acp (key {} chars)", key.len());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let core = Core::new(Box::new(L(seen.clone())), String::new());
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls: true,
        accept_invalid_certs: false,
        ca_cert_pem: None,
        cwd: "/tmp".into(),
        auto_connect: false,
        client_id: "grouse-smoke".into(),
        initial_recipe_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(4));
    core.list_sessions();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let unstable = grouse_core::GrouseUnstable::new(Box::new(UL));
    let first_sid = seen
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .find_map(|l| l.strip_prefix("FIRST:").map(|s| s.to_string()));
    if let Some(sid) = &first_sid {
        unstable.session_info(sid.clone());
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    // Open whatever session the list reported first (proves the replay path).
    if let Some(sid) = &first_sid {
        println!("== opening {sid}");
        core.open_session(sid.to_string());
        core.list_sessions();
        std::thread::sleep(std::time::Duration::from_secs(6));
        println!("== re-opening {sid} (freshness check)");
        core.open_session(sid.to_string());
        std::thread::sleep(std::time::Duration::from_secs(6));
    } else {
        std::thread::sleep(std::time::Duration::from_secs(4));
    }
    let states = seen.lock().unwrap().clone();
    println!("statuses: {states:?}");
}
