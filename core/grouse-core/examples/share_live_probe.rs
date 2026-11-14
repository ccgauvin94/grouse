// SPDX-License-Identifier: AGPL-3.0-or-later

//! Live half of the share diagnostic: open a NEW session against a real goose
//! server, ask the model to call `attachment_share`, and print every item the
//! core paints — specifically whether a `kind: File` row arrives DURING a turn,
//! as opposed to only in the store after a replay (see `share_probe`).
//!
//! Usage: KEY=<secret> cargo run --example share_live_probe -- <host> <port> <server-path> [plain]
use grouse_core::{ConnectionStatus, Core, CoreListener, Prompt, PromptBlock, ServerConfig};

struct L;

impl CoreListener for L {
    fn on_sensitivity(&self, _data: String) {}
    fn on_thread_timeline(&self, _label: String, _rows: Vec<grouse_core::TimelineRow>) {}
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
        println!("ITEM {op:?}");
    }
    fn on_usage(&self, _: i64, _: i64, _: f64, _: String) {}
    fn on_run_ended(&self, _: String, _: String) {}
    fn on_speak(&self, _: String) {}
    fn on_config(&self, _: Vec<grouse_core::ConfigOption>) {}
    fn on_permission_request(&self, r: grouse_core::PermissionRequest) {
        println!("PERMISSION {r:?}");
    }
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
    let plain = std::env::args().nth(4).is_some();
    let key = std::env::var("KEY").unwrap_or_else(|_| "dummy-key".into());

    let cache = std::env::temp_dir().join(format!("grouse-live-probe-{}", std::process::id()));
    std::fs::create_dir_all(&cache).expect("cache dir");
    let core = Core::new(Box::new(L), cache.to_string_lossy().to_string());
    core.connect(ServerConfig {
        host,
        port,
        secret_key: key,
        use_tls: !plain,
        accept_invalid_certs: true,
        ca_cert_pem: None,
        cwd: "/home/colin".into(),
        auto_connect: false,
        client_id: "grouse-live-probe".into(),
        initial_recipe_id: None,
        initial_project_id: None,
    });
    std::thread::sleep(std::time::Duration::from_secs(6));
    core.new_session(None, None);
    std::thread::sleep(std::time::Duration::from_secs(8));
    if core.active_session_id().is_none() {
        println!("no session bound");
        return;
    }
    std::thread::sleep(std::time::Duration::from_secs(4));
    let prompt = format!("Call the tool attachment_share once with path {path}. Then reply DONE.");
    core.send_prompt(
        Prompt {
            blocks: vec![PromptBlock::Text { text: prompt }],
        },
        None,
    );
    std::thread::sleep(std::time::Duration::from_secs(120));
    println!("--- end of probe");
}
