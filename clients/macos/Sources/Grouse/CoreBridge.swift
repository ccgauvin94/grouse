import Foundation
import SwiftUI
import GrouseCore

/// Everything the UI talks to: one `Core`, the published state it produces, and the
/// intents the views fire. There is no protocol logic here — that is the core's job, and
/// the whole point of this client is that it does not have any.
///
/// Concurrency: the core's callbacks arrive on its own thread, and every mutation of this
/// object happens inside `DispatchQueue.main.async` (see `Listener`). The class is
/// therefore `@unchecked Sendable` in the boring Swift 5 sense: the main queue is the
/// invariant, and nothing else writes.
final class CoreBridge: ObservableObject, @unchecked Sendable {
    // Connection
    @Published var status = "disconnected"
    @Published var online = false
    @Published var connected = false

    // Sessions
    @Published var sessions: [SessionSummary] = []
    @Published var projects: [ProjectSummary] = []
    @Published var currentSessionId: String = ""

    // The conversation on screen
    let chat = ChatModel()

    // Turn state
    @Published var prompting = false
    @Published var replayProgress: (loaded: Int64, total: Int64, done: Bool) = (0, 0, false)
    @Published var contextUsed: Int64 = 0
    @Published var contextSize: Int64 = 0

    // Prompts the server is parked on
    @Published var permission: PermissionPrompt?
    @Published var commands: [String] = []

    // Dev advisor (Settings → Dev, global like the other clients)
    @Published var advisorEnabled = UserDefaults.standard.bool(forKey: "advisor") {
        didSet {
            UserDefaults.standard.set(advisorEnabled, forKey: "advisor")
            core?.setAdvisor(on: advisorEnabled)
        }
    }
    @Published var advisorReport: AdvisorReport?

    // Connection settings, persisted so a relaunch does not re-type the key
    @Published var host: String = UserDefaults.standard.string(forKey: "host") ?? ""
    @Published var port: String = UserDefaults.standard.string(forKey: "port") ?? "3284"
    @Published var secretKey: String = UserDefaults.standard.string(forKey: "secret") ?? ""
    @Published var useTls: Bool = UserDefaults.standard.object(forKey: "tls") as? Bool ?? true
    @Published var cwd: String = UserDefaults.standard.string(forKey: "cwd") ?? "/home/colin"

    private var core: Core?
    private var listener: Listener?

    /// Where the core keeps transcripts, the session cache and the roam identity.
    static var cacheDir: String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory,
                                            in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory())
        let dir = base.appendingPathComponent("grouse", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }

    // MARK: - Intents

    func connect() {
        host = host.trimmingCharacters(in: .whitespaces)
        secretKey = secretKey.trimmingCharacters(in: .whitespaces)
        guard !host.isEmpty, !secretKey.isEmpty else {
            status = "host and secret key are required"
            return
        }
        UserDefaults.standard.set(host, forKey: "host")
        UserDefaults.standard.set(port, forKey: "port")
        UserDefaults.standard.set(secretKey, forKey: "secret")
        UserDefaults.standard.set(useTls, forKey: "tls")
        UserDefaults.standard.set(cwd, forKey: "cwd")

        // A second connect means a different server: the old core owns sockets and a
        // runtime thread, so it is disconnected and dropped rather than reused.
        core?.disconnect()
        chat.reset()
        sessions = []
        currentSessionId = ""

        let listener = Listener(self)
        self.listener = listener
        let core = Core(listener: listener, cacheDir: Self.cacheDir)
        self.core = core
        core.setAdvisor(on: advisorEnabled)
        core.connect(config: ServerConfig(
            host: host,
            port: UInt16(port) ?? 3284,
            secretKey: secretKey,
            useTls: useTls,
            // The public host serves a WebPKI cert. A self-signed tailnet box is a
            // deliberate opt-out, not the default (CONTRACT §8).
            acceptInvalidCerts: false,
            caCertPem: nil,
            cwd: cwd,
            autoConnect: false,
            // NOT "grouse-desktop": the server stamps the client on session/new, and
            // claiming another client's name is how the session type gets mislabelled.
            clientId: "grouse-macos",
            initialRecipeId: nil,
            initialProjectId: nil
        ))
    }

    func disconnect() {
        core?.disconnect()
        core = nil
        listener = nil
        online = false
        connected = false
        status = "disconnected"
    }

    func listSessions() {
        core?.listSessions()
    }

    /// A new chat. `mode` is nil for a normal chat, `"dev"` for a dev session: a mode is a
    /// tag written at `session/new` and cannot be changed afterwards, which is why the
    /// choice is at creation and nowhere else.
    func newChat(mode: String? = nil) {
        chat.reset()
        currentSessionId = ""
        prompting = false
        advisorReport = nil
        core?.newSessionWithMode(recipeId: nil, projectId: nil, mode: mode)
    }

    func open(sessionId: String) {
        chat.reset()
        advisorReport = nil
        currentSessionId = sessionId
        core?.openSession(sessionId: sessionId)
    }

    func send(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let core else { return }
        // The core echoes the prompt into the transcript itself, so the UI does not add a
        // row: doing both is how a chat doubles its own messages.
        let expect = currentSessionId.isEmpty ? nil : SendExpect(sessionId: currentSessionId)
        core.sendPrompt(prompt: Prompt(blocks: [.text(text: trimmed)]), expect: expect)
        prompting = true
    }

    func cancel() {
        core?.cancel()
        prompting = false
    }

    func answerPermission(_ optionId: String?) {
        guard let prompt = permission else { return }
        permission = nil
        core?.respondPermission(toolCallId: prompt.request.toolCallId,
                               outcome: optionId.map { .selected(optionId: $0) } ?? .cancelled)
    }

    func setConfig(id: String, value: String) {
        core?.setConfigOption(configId: id, value: value)
    }

    func dismissAdvisorReport() {
        advisorReport = nil
    }

    /// Dev-session check for the UI: the mode is the core's derived value on the row.
    var currentIsDev: Bool {
        sessions.first { $0.id == currentSessionId }?.mode == "dev"
    }

    // MARK: - Listener hand-off (main queue)

    func handleStatus(_ text: String) {
        status = text
        let wasOnline = online
        online = text == "ready" || text == "syncing"
        connected = online || text == "connecting"
        if online && !wasOnline {
            // Every fresh Ready re-lists: the sidebar is the server's view of the world,
            // and this client keeps no copy of it.
            listSessions()
        }
    }

    func handleSessions(_ list: [SessionSummary]) {
        sessions = list
    }

    func handleItem(_ op: TranscriptOp) {
        chat.apply(op)
    }

    func handleUsage(used: Int64, size: Int64, cost: Double) {
        contextUsed = used
        contextSize = size
    }

    func handleRunEnded(sessionId: String, stopReason: String) {
        guard sessionId == currentSessionId else { return }
        prompting = false
        // The row's text is final, so the view can stop treating it as a stream and render
        // it (plain-while-streaming, rich-at-finalize).
        chat.endTurn()
    }

    func handlePermission(_ request: PermissionRequest) {
        // A fresh wrapper per request: `PermissionPrompt` is the sheet's identity, and
        // reusing one object is how a dialog shows a previous request's options.
        permission = PermissionPrompt(request: request)
    }

    func handleSessionTouched(sessionId: String, title: String) {
        if let idx = sessions.firstIndex(where: { $0.id == sessionId }) {
            // Only the title arrives here; the row keeps everything else.
            _ = idx
            listSessions()
        }
    }

    func handleOpenFailed(sessionId: String, message: String) {
        status = "couldn't open that chat: \(message)"
        if sessionId == currentSessionId { currentSessionId = "" }
    }

    func handleReplay(loaded: Int64, total: Int64, done: Bool) {
        replayProgress = (loaded, total, done)
    }

    func handleConfig(_ options: [ConfigOption]) {
        // The provider/model pickers are phase 1.5; the options are kept out of the way
        // rather than dropped, so adding them is a view, not a plumbing change.
        _ = options
    }

    func handleCommands(_ list: [String]) {
        commands = list
    }

    func handleProjects(_ list: [ProjectSummary]) {
        projects = list
    }

    func handleAdvisorReport(sessionId: String, text: String) {
        guard sessionId == currentSessionId, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { return }
        advisorReport = AdvisorReport(sessionId: sessionId, text: text)
    }
}

/// A permission request as sheet state. `Identifiable` so SwiftUI treats each request as a
/// new sheet instead of mutating an already-presented one.
struct PermissionPrompt: Identifiable {
    let id = UUID()
    let request: PermissionRequest
}

/// The advisor's review of the turn that just ended. Not part of the transcript: it is
/// about the turn, and replaying it as history would have the advisor reviewing itself.
struct AdvisorReport: Identifiable {
    let id = UUID()
    let sessionId: String
    let text: String
}
