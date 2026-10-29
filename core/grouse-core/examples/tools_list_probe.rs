// SPDX-License-Identifier: AGPL-3.0-only
//
//! List a session's tools against a REAL goose server — the check that names
//! the `extension__tool` prefix an app `tools/call` must send.
//!   KEY=<secret> cargo run --example tools_list_probe -p grouse-core
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

struct UL(Arc<std::sync::Mutex<Vec<String>>>);
impl grouse_core::GrouseUnstableListener for UL {
    fn on_export(&self, _d: String) {}
    fn on_recipe_params(&self, _p: String) {}
    fn on_elicitation(&self, _s: String) {}
    fn on_compaction_status(&self, _m: String) {}
    fn on_usage(&self, _u: i64, _s: i64, _c: f64) {}
    fn on_message_usage(&self, _o: u64, _e: u64, _t: u64, _c: f64) {}
    fn on_app_resource(&self, _k: String, _h: String) {}
    fn on_recipes(&self, _r: String) {}
    fn on_schedules(&self, _s: String) {}
    fn on_projects(&self, _p: String) {}
    fn on_skills(&self, _s: String) {}
    fn on_tools(&self, _s: String, t: String) {
        // Print every tool name; the assistant-state ones carry the prefix the
        // app `tools/call` has to send.
        let v: serde_json::Value = serde_json::from_str(&t).unwrap_or(serde_json::Value::Null);
        for tool in v.as_array().cloned().unwrap_or_default() {
            if let Some(name) = tool.get("name").and_then(|n| n.as_str()) {
                if name.contains("state_") {
                    println!("tool: {name}");
                }
            }
        }
        self.0.lock().unwrap().push(format!(
            "tools: {} entries",
            v.as_array().map(|a| a.len()).unwrap_or(0)
        ));
    }
    fn on_extensions(&self, _e: String) {}
    fn on_session_extensions(&self, _s: String, e: String) {
        println!("extensions: {e}");
    }
    fn on_config_value(&self, _k: String, _v: String) {}
    fn on_supported_models(&self, _p: String, _m: String) {}
    fn on_providers(&self, _p: String) {}
    fn on_session_probe(&self, _s: String, _u: String, _m: i64) {}
    fn on_tool_result(&self, _t: String, _e: bool) {}
    fn on_dictation_config(&self, _p: String) {}
    fn on_transcript(&self, _t: String) {}
    fn on_error(&self, m: String, e: String) {
        println!("error: {m} {e}");
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
        client_id: "grouse-toolslist".into(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(4));
    core.new_session(None, None);
    std::thread::sleep(std::time::Duration::from_secs(4));
    let sid = core.active_session_id().unwrap_or_default();
    println!("session: {sid}");
    let unstable = grouse_core::GrouseUnstable::new(Box::new(UL(sink.clone())));
    unstable.list_tools(sid);
    std::thread::sleep(std::time::Duration::from_secs(6));
    for d in sink.lock().unwrap().iter() {
        println!("-- {d}");
    }
}
