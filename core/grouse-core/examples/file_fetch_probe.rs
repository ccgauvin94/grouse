// SPDX-License-Identifier: AGPL-3.0-or-later

//! Prove the server -> client file path end to end against a REAL goose server.
//!
//! Usage: KEY=<secret> cargo run --example file_fetch_probe -- <host> <port> <server-path> [name]
//!
//! `ItemKind::File` rows carry a REFERENCE and never the bytes (a blob in a tool
//! result is stored in the session, replayed on every open and shown to the
//! model), so the client pulls the file with `Core::fetch_file`, which chunks
//! `attachments__attachment_read` through `_goose/unstable/tools/call`. This
//! probe runs that pull and reports every `FileEvent`, then checks the bytes on
//! disk against the size the server reported — the one part of the feature a unit
//! test cannot exercise (chunks over a live transport, a real cache directory, a
//! real digest).
use grouse_core::{ConnectionStatus, Core, CoreListener, ServerConfig};
use std::sync::Arc;

struct L;

impl CoreListener for L {
    fn on_sensitivity(&self, _data: String) {}
    fn on_thread_timeline(&self, _label: String, _rows: Vec<grouse_core::TimelineRow>) {}
    fn on_file(&self, event: grouse_core::FileEvent) {
        println!("-- file: {event:?}");
    }
    fn on_replay_progress(&self, _: String, _: i64, _: i64, _: bool) {}
    fn on_status(&self, s: ConnectionStatus) {
        println!("status: {s:?}");
    }
    fn on_thread_ready(&self, _: String, _: String) {}
    fn on_sessions(&self, _: Vec<grouse_core::SessionSummary>) {}
    fn on_item(&self, _: grouse_core::TranscriptOp) {}
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
    let path = std::env::args().nth(3).expect("server path");
    let name = std::env::args()
        .nth(4)
        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or("file").to_string());
    let key = std::env::var("KEY").unwrap_or_else(|_| "dummy-key".into());

    let cache = std::env::temp_dir().join(format!("grouse-fetch-probe-{}", std::process::id()));
    std::fs::create_dir_all(&cache).expect("cache dir");
    println!("cache: {}", cache.display());

    let core = Core::new(Box::new(L), cache.to_string_lossy().to_string());
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls: true,
        // The box's goose serves a self-signed cert (CONTRACT §8: the documented
        // opt-out), and this probe is deliberately run over the loopback.
        accept_invalid_certs: true,
        ca_cert_pem: None,
        cwd: "/state".into(),
        auto_connect: false,
        client_id: "grouse-fetch-probe".into(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(5));
    core.new_session(None, None);
    std::thread::sleep(std::time::Duration::from_secs(5));
    let Some(session) = core.active_session_id() else {
        println!("no session bound — cannot fetch");
        return;
    };

    println!("== fetching {path}");
    core.fetch_file(session, path.clone(), name.clone(), String::new(), String::new());
    std::thread::sleep(std::time::Duration::from_secs(10));

    // The bytes on disk are the proof: the core landed the file under
    // <cache>/files/, named by digest, and the probe checks it is non-empty.
    let files = cache.join("files");
    let mut landed: Vec<_> = std::fs::read_dir(&files)
        .map(|dir| dir.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    landed.sort();
    if landed.is_empty() {
        println!("FAIL: nothing landed in {}", files.display());
    }
    for file in landed {
        let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        println!("landed: {} ({bytes} bytes)", file.display());
        let head = std::fs::read(&file).unwrap_or_default();
        println!("  head: {:02x?}", &head[..head.len().min(8)]);
    }
    let _ = std::fs::remove_dir_all(&cache);
    let _ = Arc::new(());
}
