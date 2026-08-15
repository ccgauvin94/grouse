//! Smoke-test the core's wire against a REAL goose server (used for the
//! Android login debug: the server rejected the handshake with a 400).
//! Usage: KEY=<secret> cargo run --example server_smoke -- <host> <port>
use grouse_core::{ConnectionStatus, Core, CoreListener, ServerConfig};
use std::sync::Arc;

struct L(Arc<std::sync::Mutex<Vec<String>>>);
impl CoreListener for L {
    fn on_status(&self, s: ConnectionStatus) {
        println!("status: {s:?}");
        self.0.lock().unwrap().push(format!("{s:?}"));
    }
    fn on_sessions(&self, _s: Vec<grouse_core::SessionSummary>) {}
    fn on_transcript(&self, _e: grouse_core::TranscriptEvent) {}
    fn on_stream(&self, _e: grouse_core::StreamEvent) {}
    fn on_config(&self, _o: Vec<grouse_core::ConfigOption>) {}
    fn on_permission_request(&self, _r: grouse_core::PermissionRequest) {}
    fn on_session_touched(&self, _a: String, _b: String, _c: String) {}
    fn on_projects(&self, _p: Vec<grouse_core::ProjectSummary>) {}
    fn on_roam_peer_status(&self, _a: String, _b: String) {}
    fn on_roam_sessions(&self, _a: String, _b: Vec<grouse_core::SessionSummary>) {}
    fn on_active_run(&self, _a: String, _b: String) {}
    fn on_commands(&self, _c: Vec<String>) {}
}

fn main() {
    let host = std::env::args().nth(1).unwrap_or_else(|| "goose.gauvin.id".into());
    let port = std::env::args().nth(2).unwrap_or_else(|| "443".into()).parse().unwrap();
    let key = std::env::var("KEY").unwrap_or_else(|_| "dummy-key".into());
    println!("connecting to wss://{host}:{port}/acp (key {} chars)", key.len());
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let core = Core::new(Box::new(L(seen.clone())));
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls: std::env::var("TLS").map(|v| v != "0").unwrap_or(true),
        cwd: "/tmp".into(),
        auto_connect: false,
        client_id: "grouse-smoke".into(),
        initial_recipe_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(18));
    let states = seen.lock().unwrap().clone();
    println!("statuses seen: {states:?}");
}
