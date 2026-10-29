// SPDX-License-Identifier: AGPL-3.0-only
//
//! Probe the per-message stats (message_usage → on_message_usage) end to end
//! against a REAL goose server: connect, create a session, send two turns,
//! print everything the core hands the client. Usage:
//!   KEY=<secret> cargo run --example stats_probe -p grouse-core
//!   (HOST/PORT env: default 127.0.0.1:3284, TLS trust-all for self-signed)
use grouse_core::{ConnectionStatus, Core, CoreListener, Prompt, PromptBlock, ServerConfig};
use std::sync::Arc;
struct L(Arc<std::sync::Mutex<Vec<String>>>);
impl CoreListener for L {
    fn on_status(&self, s: ConnectionStatus) { println!("status: {s:?}"); self.0.lock().unwrap().push(format!("{s:?}")); }
    fn on_sessions(&self, _s: Vec<grouse_core::SessionSummary>) {}
    fn on_session_touched(&self, _i: String, t: String, u: String) { println!("-- touched: {:?} {}", t, u); }
    fn on_item(&self, _op: grouse_core::TranscriptOp) {}
    fn on_usage(&self, u: i64, s: i64, c: f64, cur: String) { println!("-- on_usage(acp): used={u} call={s} cost={c} model={cur}"); }
    fn on_run_ended(&self, s: String, r: String) { println!("-- run_ended: {s} {r}"); }
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
}
struct UL;
impl grouse_core::GrouseUnstableListener for UL {
    fn on_export(&self, _d: String) {}
    fn on_recipe_params(&self, _p: String) {}
    fn on_elicitation(&self, _s: String) {}
    fn on_compaction_status(&self, _m: String) {}
    fn on_usage(&self, u: i64, s: i64, c: f64) { println!("-- on_usage(unstable): used={u} limit={s} cost={c}"); }
    fn on_message_usage(&self, o: u64, e: u64, t: u64, c: f64) {
        println!("-- on_message_usage: out={o} elapsed={e}ms ttft={t}ms cost={c} tok/s={}",
            if e>0 {o as f64 / (e as f64/1000.0)} else {0.0});
    }
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
    fn on_session_probe(&self, _s: String, _u: String, _m: i64) {}
    fn on_tool_result(&self, _t: String, _e: bool) {}
    fn on_dictation_config(&self, _p: String) {}
    fn on_transcript(&self, _t: String) {}
    fn on_error(&self, _m: String, _e: String) { println!("-- on_error: {_m} {_e}"); }
}
fn main() {
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("PORT").unwrap_or("3284".into()).parse().unwrap();
    let key = std::env::var("KEY").unwrap();
    let core = Core::new(Box::new(L(Arc::new(Default::default()))), String::new());
    let _unstable = grouse_core::GrouseUnstable::new(Box::new(UL));
    core.connect(ServerConfig { host: host.clone(), port, secret_key: key, use_tls: true, accept_invalid_certs: true, ca_cert_pem: None, cwd: "/tmp".into(), auto_connect: false, client_id: "grouse-statsprobe".into(), initial_recipe_id: None, initial_project_id: None });
    std::thread::sleep(std::time::Duration::from_secs(4));
    core.new_session(None, None);
    std::thread::sleep(std::time::Duration::from_secs(4));
    core.send_prompt(Prompt { blocks: vec![PromptBlock::Text { text: "Answer with the single word: ok".into() }] }, None);
    std::thread::sleep(std::time::Duration::from_secs(40));
    // Second turn in the SAME session (the phone case: an existing chat, a new turn).
    core.send_prompt(Prompt { blocks: vec![PromptBlock::Text { text: "Answer with the single word: ok".into() }] }, None);
    std::thread::sleep(std::time::Duration::from_secs(40));
    println!("done");
}
