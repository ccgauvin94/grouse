// SPDX-License-Identifier: AGPL-3.0-or-later

//! Diagnose a server-shared file row: open an EXISTING session against a real
//! goose server and print every transcript item the core produces.
//!
//! Usage: cargo run --example share_probe -- <host> <port> <session-id> [plain]
//!
//! `plain` (any 4th arg) connects without TLS, for a throwaway `goose serve` on
//! loopback. The point is the REPLAY path: the same tool call that streamed live
//! must rebuild as `ItemKind::File` when the chat is reopened, and this prints
//! what the core actually emits rather than what the source suggests it should.
use grouse_core::{ConnectionStatus, Core, CoreListener, ServerConfig};
use std::sync::Arc;

struct L(Arc<std::sync::Mutex<Vec<String>>>);

impl CoreListener for L {
    fn on_file(&self, event: grouse_core::FileEvent) {
        println!("-- file event: {event:?}");
    }
    fn on_replay_progress(&self, _: String, _: i64, _: i64, _: bool) {}
    fn on_status(&self, s: ConnectionStatus) {
        println!("status: {s:?}");
    }
    fn on_thread_ready(&self, _: String, _: String) {}
    fn on_sessions(&self, _: Vec<grouse_core::SessionSummary>) {}
    fn on_item(&self, op: grouse_core::TranscriptOp) {
        self.0.lock().unwrap().push(format!("{op:?}"));
        println!("ITEM {op:?}");
    }
    fn on_usage(&self, _: i64, _: i64, _: f64, _: String) {}
    fn on_run_ended(&self, _: String, _: String) {}
    fn on_speak(&self, _: String) {}
    fn on_config(&self, _: Vec<grouse_core::ConfigOption>) {}
    fn on_permission_request(&self, _: grouse_core::PermissionRequest) {}
    fn on_projects(&self, _: Vec<grouse_core::ProjectSummary>) {}
    fn on_roam_peer_status(&self, _: String, _: String) {}
    fn on_roam_sessions(&self, _: String, _: Vec<grouse_core::SessionSummary>) {}
    fn on_peer_new_session(&self, _: String, _: String) {}
    fn on_active_run(&self, _: String, _: String) {}
    fn on_commands(&self, _: Vec<String>) {}
    fn on_session_open_failed(&self, _: String, _: String) {}
    fn on_tasks(&self, _: String) {}
    fn on_session_touched(&self, _: String, _: String, _: String) {}
}

fn main() {
    let host = std::env::args().nth(1).expect("host");
    let port: u16 = std::env::args().nth(2).expect("port").parse().expect("port");
    let session = std::env::args().nth(3).expect("session id");
    let plain = std::env::args().nth(4).is_some();
    let key = std::env::var("KEY").unwrap_or_else(|_| "dummy-key".into());

    let cache = std::env::temp_dir().join(format!("grouse-share-probe-{}", std::process::id()));
    std::fs::create_dir_all(&cache).expect("cache dir");
    let items = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let core = Core::new(Box::new(L(items.clone())), cache.to_string_lossy().to_string());
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls: !plain,
        accept_invalid_certs: true,
        ca_cert_pem: None,
        cwd: "/home/colin".into(),
        auto_connect: false,
        client_id: "grouse-share-probe".into(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(6));
    core.open_session(session);
    std::thread::sleep(std::time::Duration::from_secs(25));

    let all = items.lock().unwrap().clone();
    let files: Vec<&String> = all.iter().filter(|l| l.contains("File")).collect();
    println!("\n=== {} items, {} mentioning File", all.len(), files.len());
    for f in files {
        println!("FILE ROW: {}", &f[..f.len().min(400)]);
    }
}
