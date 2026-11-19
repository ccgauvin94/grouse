// grouse-spike — the macOS de-risking CLI.
//
// It exists to answer ONE question before anyone writes a SwiftUI app: does the core,
// through the uniffi Swift bindings, connect, list sessions, replay a transcript and take
// a prompt on a real Mac? If this runs, the app is a known quantity (the phases in
// clients/macos/README.md hold). If it does not, the failure is here, cheap, instead of a
// month into a UI.
//
// It is deliberately a CLI with environment configuration: no settings UI, no storage, no
// persistence. Nothing here is meant to survive into the app — what it proves is the
// binding boundary, which the app then uses the same way.
//
//   GROUSE_HOST=goose.gauvin.id GROUSE_PORT=443 GROUSE_KEY=… ./.build/debug/grouse-spike
//   … GROUSE_TLS=0 GROUSE_CWD=/home/colin                    # a plain-ws dev host
//   … GROUSE_PEER=laptop                                     # also try a roam peer
//   … --no-send                                             # stop before prompting

import Foundation
import GrouseCore

// MARK: - Thread-safe view of what the core pushed

/// `CoreListener` is `AnyObject, Sendable`, and every callback arrives on the CORE's
/// runtime thread — never the one this CLI reads from. So the state is behind a lock and
/// the class is marked `@unchecked Sendable`: the lock is the invariant.
final class Buffer: CoreListener, @unchecked Sendable {
    private let lock = NSLock()

    private var _status = "unknown"
    private var _sessions: [SessionSummary] = []
    private var _rows: [String] = []
    private var _runEnds = 0
    private var _failed: String?
    private var _usage: (used: Int64, size: Int64)?
    private var _replay = (loaded: Int64, total: Int64, done: Bool)
        = (0, 0, false)

    var status: String { lock.lock(); defer { lock.unlock() }; return _status }
    var sessions: [SessionSummary] { lock.lock(); defer { lock.unlock() }; return _sessions }
    var rows: [String] { lock.lock(); defer { lock.unlock() }; return _rows }
    var runEnds: Int { lock.lock(); defer { lock.unlock() }; return _runEnds }
    var failure: String? { lock.lock(); defer { lock.unlock() }; return _failed }
    var usage: (used: Int64, size: Int64)? { lock.lock(); defer { lock.unlock() }; return _usage }
    var replay: (loaded: Int64, total: Int64, done: Bool) {
        lock.lock(); defer { lock.unlock() }; return _replay
    }

    func onStatus(status: ConnectionStatus) {
        let text: String
        switch status {
        case .disconnected: text = "disconnected"
        case .connecting: text = "connecting"
        case .ready: text = "ready"
        case .syncing: text = "syncing"
        case .error(let message): text = "error: \(message)"
        case .alert(let message): text = "alert: \(message)"
        }
        lock.lock(); _status = text; lock.unlock()
        say("  status → \(text)")
    }

    func onSessions(sessions: [SessionSummary]) {
        lock.lock(); _sessions = sessions; lock.unlock()
        say("  sessions → \(sessions.count)")
    }

    func onItem(op: TranscriptOp) {
        // A CLI prints the SHAPE of the stream, not a transcript: this is the proof that
        // the item ops arrive and carry usable rows. The app builds bubbles from the same
        // ops (docs/TRANSCRIPT_MODEL.md).
        switch op {
        case .upsert(let item):
            lock.lock()
            _rows.append("[\(item.kind)] \(oneLine(item.text))")
            lock.unlock()
        case .appendText(_, let chunk):
            lock.lock()
            if let last = _rows.indices.last { _rows[last] += chunk }
            lock.unlock()
        case .window(let oldest, let hasOlder):
            say("  window → oldest=\(oldest.isEmpty ? "-" : oldest) hasOlder=\(hasOlder)")
        case .reset:
            lock.lock(); _rows.removeAll(); lock.unlock()
        case .remove, .appendOutput:
            break   // not interesting for a smoke test
        }
    }

    func onUsage(used: Int64, size: Int64, cost: Double, currency: String) {
        lock.lock(); _usage = (used, size); lock.unlock()
    }

    func onRunEnded(sessionId: String, stopReason: String) {
        lock.lock(); _runEnds += 1; lock.unlock()
        say("  turn ended (\(stopReason))")
    }

    func onReplayProgress(sessionId: String, loaded: Int64, total: Int64, done: Bool) {
        lock.lock(); _replay = (loaded, total, done); lock.unlock()
    }

    func onSessionOpenFailed(sessionId: String, message: String) {
        lock.lock(); _failed = message; lock.unlock()
        say("  OPEN FAILED: \(message)")
    }

    func onPermissionRequest(request: PermissionRequest) {
        // The CLI answers anything it is asked with "cancel": a smoke test must never
        // approve a tool call on the user's behalf.
        say("  permission requested: \(oneLine(request.title)) → cancelling")
        core?.respondPermission(toolCallId: request.toolCallId, outcome: .cancelled)
    }

    /// Set after construction (the listener is needed to build the core).
    var core: Core?
}

func say(_ line: String) {
    FileHandle.standardOutput.write((line + "\n").data(using: .utf8)!)
}

func oneLine(_ text: String, cap: Int = 110) -> String {
    let flat = text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
    return flat.count <= cap ? flat : String(flat.prefix(cap)) + "…"
}

// MARK: - Env

func env(_ key: String, _ fallback: String = "") -> String {
    ProcessInfo.processInfo.environment[key] ?? fallback
}

let noSend = CommandLine.arguments.contains("--no-send")
let host = env("GROUSE_HOST", "localhost")
let port = UInt16(env("GROUSE_PORT", "3284")) ?? 3284
let key = env("GROUSE_KEY")
let useTls = env("GROUSE_TLS", "1") != "0"
let cwd = env("GROUSE_CWD", NSHomeDirectory())
let cacheDir = env("GROUSE_CACHE",
                   NSHomeDirectory() + "/Library/Caches/grouse-spike")

guard !key.isEmpty else {
    FileHandle.standardError.write("""
    GROUSE_KEY is required (the server's X-Secret-Key).

      GROUSE_HOST=goose.gauvin.id GROUSE_PORT=443 GROUSE_KEY=… ./.build/debug/grouse-spike

    Optional: GROUSE_TLS=0 (plain ws), GROUSE_CWD=/abs/path (session/new needs one that
    exists INSIDE the goose container), GROUSE_CACHE=<dir>, --no-send.
    """.data(using: .utf8)! + Data("\n".utf8))
    exit(2)
}

// MARK: - Poll a pushed value

@discardableResult
func waitFor(_ what: String, seconds: Double, _ test: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(seconds)
    while Date() < deadline {
        if test() { return true }
        Thread.sleep(forTimeInterval: 0.1)
    }
    say("TIMEOUT waiting for \(what)")
    return false
}

// MARK: - The run

say("grouse-spike — Mac \u{2192} goose \u{2192} core \u{2192} Swift bindings")
say("server \(useTls ? "wss" : "ws")://\(host):\(port)  cwd=\(cwd)")
say("cache  \(cacheDir)")

let buffer = Buffer()
let core = Core(listener: buffer, cacheDir: cacheDir)
buffer.core = core

let config = ServerConfig(
    host: host,
    port: port,
    secretKey: key,
    useTls: useTls,
    acceptInvalidCerts: false,   // a real cert on the public host; flip for a self-signed one
    caCertPem: nil,
    cwd: cwd,
    autoConnect: false,          // connect() below is the explicit step under test
    clientId: "grouse-cli",      // NOT "grouse-desktop": this must not look like a chat client
    initialRecipeId: nil,
    initialProjectId: nil
)

say("\n1. connect")
core.connect(config: config)
guard waitFor("the wire to come up", seconds: 30, { buffer.status == "ready" }) else {
    say("FAILED: status is \(buffer.status)")
    exit(1)
}

say("\n2. list sessions")
core.listSessions()
guard waitFor("sessions", seconds: 30, { !buffer.sessions.isEmpty }) else {
    say("FAILED: no sessions (an empty server?)")
    exit(1)
}
for s in buffer.sessions.prefix(10) {
    let mode = s.mode ?? "-"
    let dev = mode == "dev" ? " </>" : ""
    say("  \(s.id)  [\(mode)]\(dev)  \(oneLine(s.title, cap: 60))  (\(s.messageCount) msgs)")
}

say("\n3. open the newest session and read its transcript")
guard let target = buffer.sessions.first else { exit(1) }
core.openSession(sessionId: target.id)
// Either rows arrive (a replay) or the chat is genuinely empty; both are answers.
_ = waitFor("transcript rows or a finished replay", seconds: 60, {
    !buffer.rows.isEmpty || buffer.replay.done
})
say("  rows: \(buffer.rows.count), replay \(buffer.replay.loaded)/\(buffer.replay.total)")
for row in buffer.rows.suffix(8) { say("    \(oneLine(row))") }
if let usage = buffer.usage {
    say("  context: \(usage.used)/\(usage.size) tokens")
}

if noSend {
    say("\n4. skipped (--no-send)")
} else {
    say("\n4. send a prompt and wait for the turn to end")
    core.sendPrompt(
        prompt: Prompt(blocks: [.text(text: "Reply with exactly: SPIKE-OK")]),
        expect: SendExpect(sessionId: target.id)
    )
    let ended = waitFor("the turn to end", seconds: 180, { buffer.runEnds > 0 })
    say("  last rows:")
    for row in buffer.rows.suffix(4) { say("    \(oneLine(row))") }
    if !ended { exit(1) }
}

say("\nOK — bindings, transport, transcript and (if run) a prompt all work from Swift.")
core.disconnect()
