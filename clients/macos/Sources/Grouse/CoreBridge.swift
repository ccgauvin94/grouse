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
    @Published var loadingOlder = false

    /// An open whose outcome is not known yet. The core paints a cached transcript
    /// BEFORE it asks the server to bind, so a Reset is not proof of success: a
    /// later `on_session_open_failed` must still put the previous chat back.
    private struct OpenAttempt {
        let sessionId: String
        let previousSessionId: String
        let snapshot: ChatModel.Snapshot
        var didPaint = false
    }
    private var openAttempt: OpenAttempt?

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
        openAttempt = nil
        prompting = false
        loadingOlder = false

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
        openAttempt = nil
        prompting = false
        loadingOlder = false
    }

    func listSessions() {
        core?.listSessions()
    }

    /// A new chat. `mode` is nil for a normal chat, `"dev"` for a dev session: a mode is a
    /// tag written at `session/new` and cannot be changed afterwards, which is why the
    /// choice is at creation and nowhere else.
    func newChat(mode: String? = nil) {
        // A brand-new chat is minted by the core; the paint's Reset carries its id and
        // is adopted in handleItem. There is no failed open to roll back here.
        openAttempt = nil
        chat.reset()
        currentSessionId = ""
        prompting = false
        loadingOlder = false
        advisorReport = nil
        core?.newSessionWithMode(recipeId: nil, projectId: nil, mode: mode)
    }

    func open(sessionId: String) {
        guard sessionId != currentSessionId else { return }
        // Keep the transcript on screen until the core proves the open landed. A failed
        // `session/load` binds nothing; clearing here used to leave an empty chat with no
        // way back to the conversation the user was reading.
        openAttempt = OpenAttempt(sessionId: sessionId,
                                  previousSessionId: currentSessionId,
                                  snapshot: chat.snapshot())
        currentSessionId = sessionId
        prompting = false          // a turn in another chat is not this chat's turn
        loadingOlder = false
        core?.openSession(sessionId: sessionId)
        reconcileTurnSoon(sessionId: sessionId)
    }

    func send(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let core else { return }
        // If a previous turn ended without its notification, the core has already let
        // the turn go; do not keep a dead flag that disables the honest Stop state.
        reconcileTurn()
        // The core echoes the prompt into the transcript itself, so the UI does not add a
        // row: doing both is how a chat doubles its own messages.
        let expect = currentSessionId.isEmpty ? nil : SendExpect(sessionId: currentSessionId)
        core.sendPrompt(prompt: Prompt(blocks: [.text(text: trimmed)]), expect: expect)
        prompting = true
        reconcileTurnSoon(sessionId: expect?.sessionId, delay: 1.0)
    }

    func cancel() {
        core?.cancel()
        prompting = false
        reconcileTurnSoon()
    }

    /// Ask the core for an older page. Outcomes arrive as Upserts + a Window on
    /// `on_item`; there is no client-side history store to duplicate here.
    func loadOlder(count: Int = 40) {
        guard !currentSessionId.isEmpty, !loadingOlder, let core else { return }
        loadingOlder = true
        core.loadOlder(count: UInt32(count))
    }

    // MARK: - Turn state

    /// The core owns turn state; this flag is only the view's answer to "is Stop live?".
    private func reconcileTurn(sessionId sid: String? = nil) {
        let sid = sid ?? currentSessionId
        guard !sid.isEmpty, let core else {
            prompting = false
            return
        }
        prompting = core.turnInFlight(sessionId: sid)
    }

    /// Core intents are enqueued on its runtime thread, so an immediate getter can still
    /// see the pre-intent state. Re-check once it has had time to run.
    private func reconcileTurnSoon(sessionId sid: String? = nil, delay: TimeInterval = 0.4) {
        let sid = sid ?? currentSessionId
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self, self.currentSessionId == sid else { return }
            self.reconcileTurn(sessionId: sid)
        }
    }

    /// A Ready (or a completed replay) after an open is the proof that the core bound
    /// the session the user picked. A cached paint before it is not: the load can still
    /// fail. On success the rolled-back state is simply dropped.
    private func finishOpenIfBound(from eventSessionId: String? = nil) {
        guard let attempt = openAttempt,
              attempt.sessionId == currentSessionId,
              eventSessionId == nil || eventSessionId == attempt.sessionId,
              core?.activeSessionId() == attempt.sessionId else { return }
        openAttempt = nil
        advisorReport = nil
        if !attempt.didPaint { chat.reset() }
        reconcileTurn(sessionId: attempt.sessionId)
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

    func handleAlert(_ message: String) {
        // CONTRACT §3.3: Alert is an advisory failure on a control path with the wire
        // still up. Text only — no change to online, connected or the turn (the desktop
        // does the same at manager.cpp's `s.contains("Alert")` branch).
        status = message
    }

    func handleStatus(_ text: String) {
        status = text
        let wasOnline = online
        online = text == "ready" || text == "syncing"
        connected = online || text == "connecting"
        if text == "ready" || text == "syncing" {
            // A reconnect can close a turn without an end notification; ask the core,
            // which is the only one that knows whether the wire still carries it.
            reconcileTurnSoon()
            finishOpenIfBound()
        } else if text == "disconnected" || text.hasPrefix("error:") {
            prompting = false
        }
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
        switch op {
        case .reset(let sessionId):
            if openAttempt != nil {
                // The core paints the displayed chat; an unrelated paint while an open
                // is pending must not overwrite the conversation being kept for rollback.
                guard sessionId == currentSessionId else { return }
                openAttempt?.didPaint = true
            } else if currentSessionId.isEmpty && !sessionId.isEmpty {
                // session/new (and a resume) has no separate "opened" event: the paint's
                // session id is the first thing that names the chat.
                currentSessionId = sessionId
                reconcileTurn(sessionId: sessionId)
            }
            loadingOlder = false
        case .window:
            // The core answers every load_older with a Window, including an empty one.
            loadingOlder = false
        default:
            break
        }
        chat.apply(op)
    }

    func handleUsage(used: Int64, size: Int64, cost: Double) {
        contextUsed = used
        contextSize = size
    }

    func handleRunEnded(sessionId: String, stopReason: String) {
        guard sessionId == currentSessionId else {
            // A background run ending says nothing about THIS chat's turn, but it is a
            // chance to notice that a local flag was left behind by a dropped event.
            reconcileTurn()
            return
        }
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
        loadingOlder = false
        guard let attempt = openAttempt else { return }
        openAttempt = nil
        // The core binds NOTHING on a failed load. Restore the chat the user was in
        // rather than leaving the id on a session with no transcript (or no session
        // at all). A failure for a chat that is no longer current is just a status line.
        guard sessionId == currentSessionId else { return }
        currentSessionId = attempt.previousSessionId
        chat.restore(attempt.snapshot)
        reconcileTurn(sessionId: attempt.previousSessionId)
    }

    func handleReplay(sessionId: String, loaded: Int64, total: Int64, done: Bool) {
        replayProgress = (loaded, total, done)
        if done { finishOpenIfBound(from: sessionId) }
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
