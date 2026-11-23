import Foundation
import GrouseCore

/// The core's listener, forwarded to the main queue.
///
/// `CoreListener` is `AnyObject, Sendable` and every callback arrives on the core's own
/// runtime thread, so this class exists only to be the FFI target: it holds the bridge and
/// hops each event onto the main queue, where the published state lives. Nothing here may
/// touch UI or mutable app state directly.
///
/// The per-event mapping lives on `CoreBridge` (`handle…`) so this file stays a list.
final class Listener: CoreListener, @unchecked Sendable {
    private weak var bridge: CoreBridge?

    init(_ bridge: CoreBridge) {
        self.bridge = bridge
    }

    private func onMain(_ body: @escaping (CoreBridge) -> Void) {
        // A weak capture: the bridge owns the core, which owns this listener, so a strong
        // reference here would be a cycle that survives `disconnect()`.
        DispatchQueue.main.async { [weak bridge] in
            guard let bridge else { return }
            body(bridge)
        }
    }

    func onStatus(status: ConnectionStatus) {
        // Alert is NOT a connection transition: the core sends it for a one-off failure
        // on a control path while the wire stays up. It goes to the status line only —
        // routing it through handleStatus would flip `online` and swap the whole chat
        // window for ConnectView.
        let deliver: (CoreBridge) -> Void
        switch status {
        case .disconnected: deliver = { $0.handleStatus("disconnected") }
        case .connecting: deliver = { $0.handleStatus("connecting") }
        case .ready: deliver = { $0.handleStatus("ready") }
        case .syncing: deliver = { $0.handleStatus("syncing") }
        case .error(let message): deliver = { $0.handleStatus("error: \(message)") }
        case .alert(let message): deliver = { $0.handleAlert(message) }
        }
        onMain(deliver)
    }

    func onSessions(sessions: [SessionSummary]) {
        onMain { $0.handleSessions(sessions) }
    }

    func onItem(op: TranscriptOp) {
        onMain { $0.handleItem(op) }
    }

    func onUsage(used: Int64, size: Int64, cost: Double, currency: String) {
        onMain { $0.handleUsage(used: used, size: size, cost: cost) }
    }

    func onRunEnded(sessionId: String, stopReason: String) {
        onMain { $0.handleRunEnded(sessionId: sessionId, stopReason: stopReason) }
    }

    func onPermissionRequest(request: PermissionRequest) {
        onMain { $0.handlePermission(request) }
    }

    func onSessionTouched(sessionId: String, title: String, updatedAt: String) {
        onMain { $0.handleSessionTouched(sessionId: sessionId, title: title) }
    }

    func onSessionOpenFailed(sessionId: String, message: String) {
        onMain { $0.handleOpenFailed(sessionId: sessionId, message: message) }
    }

    /// The explicit "load bound this session" proof. A Reset is only a paint: the core
    /// paints a cached transcript before it asks the server to bind, and that load can
    /// still fail. Never infer open success from rows arriving (QUALITY_AUDIT §3/§6).
    func onSessionOpened(sessionId: String) {
        onMain { $0.handleOpened(sessionId: sessionId) }
    }

    func onActiveRun(sessionId: String, runId: String) {
        onMain { $0.handleActiveRun(sessionId: sessionId, runId: runId) }
    }

    func onFile(event: FileEvent) {
        onMain { $0.handleFile(event) }
    }

    func onRoamPeerStatus(label: String, status: String) {
        onMain { $0.handleRoamStatus(label: label, status: status) }
    }

    func onRoamSessions(label: String, sessions: [SessionSummary]) {
        onMain { $0.handleRoamSessions(label: label, sessions: sessions) }
    }

    func onPeerNewSession(label: String, sessionId: String) {
        onMain { $0.handlePeerNewSession(label: label, sessionId: sessionId) }
    }

    func onReplayProgress(sessionId: String, loaded: Int64, total: Int64, done: Bool) {
        onMain { $0.handleReplay(sessionId: sessionId, loaded: loaded, total: total, done: done) }
    }

    func onConfig(options: [ConfigOption]) {
        onMain { $0.handleConfig(options) }
    }

    func onAdvisorReport(sessionId: String, text: String) {
        onMain { $0.handleAdvisorReport(sessionId: sessionId, text: text) }
    }

    func onProjects(projects: [ProjectSummary]) {
        onMain { $0.handleProjects(projects) }
    }

    func onCommands(commands: [String]) {
        onMain { $0.handleCommands(commands) }
    }
}
