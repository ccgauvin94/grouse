import AppKit
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
/// invariant, and nothing else writes. The only exception is the CONNECT intent itself,
/// which is deliberately issued on `coreQueue` because it can block for the whole
/// reconnect budget (QUALITY_AUDIT §6) — it touches only the captured `Core`, never state.
final class CoreBridge: ObservableObject, @unchecked Sendable {
    // Connection
    @Published var status = "disconnected"
    @Published var online = false
    @Published var connected = false
    @Published var isConnecting = false

    // Sessions. This is the CORE's unified list (main + roam), never a prefix parse of
    // one wire's list (QUALITY_AUDIT §3: `all_sessions` replaces client-side grouping).
    @Published var sessions: [SessionSummary] = []
    @Published var projects: [ProjectSummary] = []
    @Published var currentSessionId: String = ""

    /// Peers are observed, not stored: the core emits status/sessions/new-session for
    /// each one, and there is no client-side copy of a connection card or identity.
    @Published var roamPeers: [PeerState] = []

    // The conversation on screen
    let chat = ChatModel()
    @Published var loadingOlder = false

    /// An open whose outcome is not known yet. The core paints a cached transcript
    /// BEFORE it asks the server to bind, so a paint is NOT proof of success: the
    /// `onSessionOpened` event is. The snapshot exists only for the opposite case — a
    /// later `on_session_open_failed` must put the previous chat back.
    private struct OpenAttempt {
        let sessionId: String
        let previousSessionId: String
        let snapshot: ChatModel.Snapshot
    }
    private var openAttempt: OpenAttempt?

    // Turn state. `onActiveRun` is authoritative; `core.turnInFlight` is the repair
    // read when an event may have been lost on a reconnect. No timers infer this.
    @Published var prompting = false
    private var activeRuns: [String: String] = [:]
    @Published var replayProgress: (loaded: Int64, total: Int64, done: Bool) = (0, 0, false)
    @Published var contextUsed: Int64 = 0
    @Published var contextSize: Int64 = 0

    /// Server→client file fetches, keyed by the core's request id (with a path fallback
    /// for an older core). `FileEvent.sessionId` keeps a download from another chat from
    /// being credited to the one on screen.
    @Published var fileTransfers: [String: FileTransfer] = [:]

    // Prompts the server is parked on. Both queues are keyed by the SERVER's id, not by
    // "the current prompt": a main-wire approval and a roam peer's form can overlap, and a
    // single slot would strand the first request when the second arrives (CONTRACT §5).
    @Published var permissionPrompts: [PermissionPrompt] = []
    @Published var formPrompts: [ServerFormPrompt] = []
    /// Pinned card fingerprints by peer label, read from the core (never forked here).
    @Published var peerPins: [String: String] = [:]

    var activePermission: PermissionPrompt? { permissionPrompts.first }
    var activeForm: ServerFormPrompt? { formPrompts.first }

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
    /// The unstable shim is a SECOND uniffi object with its own listener: recipe/elicitation
    /// forms ride it, and its respond intents are the ones that carry a request id.
    private var unstable: GrouseUnstable?
    private var unstableListener: UnstableListener?
    private let coreQueue = DispatchQueue(label: "id.gauvin.grouse.macos.core", qos: .userInitiated)

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
        // runtime thread, so it is disconnected and dropped rather than reused. Queue
        // its close behind any connect still in flight on the background queue.
        let oldCore = core
        core = nil
        listener = nil
        unstable = nil
        unstableListener = nil
        if let oldCore {
            coreQueue.async { oldCore.disconnect() }
        }
        chat.reset()
        sessions = []
        roamPeers = []
        peerPins = [:]
        permissionPrompts = []
        formPrompts = []
        currentSessionId = ""
        openAttempt = nil
        prompting = false
        activeRuns.removeAll()
        fileTransfers.removeAll()
        loadingOlder = false

        let listener = Listener(self)
        self.listener = listener
        let core = Core(listener: listener, cacheDir: Self.cacheDir)
        self.core = core
        let unstableListener = UnstableListener(self)
        self.unstableListener = unstableListener
        unstable = GrouseUnstable(listener: unstableListener)
        refreshPins()
        let advisorOn = advisorEnabled
        let config = ServerConfig(
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
        )
        isConnecting = true
        status = "connecting"
        // connect* is the one blocking intent in the core (bounded at 15 s per attempt).
        // `connectAttach` binds NOTHING: the old `connect` minted a throwaway session and
        // made it the displayed chat before the user picked one. The main wire is still
        // fully usable — session/list, projects and roam all work with no session bound.
        coreQueue.async { [weak self] in
            core.setAdvisor(on: advisorOn)
            core.connectAttach(config: config)
            DispatchQueue.main.async { [weak self] in
                guard let self, self.core === core else { return }
                self.isConnecting = false
            }
        }
    }

    func disconnect() {
        let oldCore = core
        core = nil
        listener = nil
        unstable = nil
        unstableListener = nil
        if let oldCore {
            coreQueue.async { oldCore.disconnect() }
        }
        online = false
        connected = false
        status = "disconnected"
        isConnecting = false
        openAttempt = nil
        prompting = false
        activeRuns.removeAll()
        loadingOlder = false
        // An unanswered prompt must not survive a disconnect: its responder is gone with
        // the old core, and a stale sheet would send an answer to nothing.
        permissionPrompts = []
        formPrompts = []
        peerPins = [:]
    }

    func listSessions() {
        core?.listSessions()
        refreshSessions()
    }

    /// A new chat. `mode` is nil for a normal chat, `"dev"` for a dev session: a mode is a
    /// tag written at `session/new` and cannot be changed afterwards, which is why the
    /// choice is at creation and nowhere else. A `peer` routes the create to that roam
    /// peer with its existing core intent.
    func newChat(mode: String? = nil, peer: String? = nil) {
        openAttempt = nil
        chat.reset()
        currentSessionId = ""
        prompting = false
        activeRuns.removeAll()
        loadingOlder = false
        advisorReport = nil
        if let peer, !peer.isEmpty {
            core?.roamNewSession(label: peer)
        } else {
            core?.newSessionWithMode(recipeId: nil, projectId: nil, mode: mode)
        }
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

        // The unified list carries roam ids; the CORE says which wire owns each one.
        // `roamOpenSession` also accepts an already-prefixed id and routes it internally,
        // but asking the owner first keeps this client's routing in one place.
        let owner = self.owner(for: sessionId)
        if owner.isRoam, !owner.label.isEmpty {
            core?.roamOpenSession(label: owner.label, sessionId: sessionId)
        } else {
            core?.openSession(sessionId: sessionId)
        }
        // Switch-time repair: a run that started while another chat was on screen was
        // announced through onActiveRun, but asking the core is the authoritative check.
        reconcileTurn(sessionId: sessionId)
    }

    func send(_ text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let core else { return }
        // A turn may have ended without an end notification; the core has already let
        // it go, so do not keep a dead flag that disables the honest Stop state.
        reconcileTurn()
        // The core echoes the prompt into the transcript itself, so the UI does not add a
        // row: doing both is how a chat doubles its own messages.
        let expect = currentSessionId.isEmpty ? nil : SendExpect(sessionId: currentSessionId)
        core.sendPrompt(prompt: Prompt(blocks: [.text(text: trimmed)]), expect: expect)
        prompting = true   // optimistic until onActiveRun confirms the run
    }

    func cancel() {
        core?.cancel()
        prompting = false
    }

    /// Ask the core for an older page. Outcomes arrive as Upserts + a Window on
    /// `on_item`; there is no client-side history store to duplicate here.
    func loadOlder(count: Int = 40) {
        guard !currentSessionId.isEmpty, !loadingOlder, let core else { return }
        loadingOlder = true
        core.loadOlder(count: UInt32(count))
    }

    // MARK: - Turn state

    /// A getter, never a timer: used when switching chats, on a fresh Ready (a dropped
    /// wire can lose an end event) and before sending. `onActiveRun` drives the flag in
    /// the ordinary case (QUALITY_AUDIT §3).
    private func reconcileTurn(sessionId sid: String? = nil) {
        let sid = sid ?? currentSessionId
        guard !sid.isEmpty, let core else {
            prompting = false
            return
        }
        prompting = core.turnInFlight(sessionId: sid)
    }

    // MARK: - Ownership and unified sessions

    private struct ChatOwner {
        let isRoam: Bool
        let label: String
    }

    /// `session_owner(id)` is the core's answer; the id-prefix fallback is only for the
    /// moment before a core exists (nothing can be routed then anyway).
    private func owner(for sessionId: String) -> ChatOwner {
        guard let core else {
            return ChatOwner(isRoam: sessionId.hasPrefix("roam:"),
                             label: Self.peerLabel(from: sessionId))
        }
        let owner = core.sessionOwner(sessionId: sessionId)
        let isRoam = owner.kind == "roam" || owner.kind == "peer"
        let label = owner.label.isEmpty ? Self.peerLabel(from: sessionId) : owner.label
        return ChatOwner(isRoam: isRoam, label: label)
    }

    private static func peerLabel(from sessionId: String) -> String {
        guard sessionId.hasPrefix("roam:") else { return "" }
        let rest = String(sessionId.dropFirst("roam:".count))
        // The FIRST colon separates the label from the wire id: the core rejects
        // ':' in roam labels and resolves the LONGEST known label prefix, so a
        // wire id may itself contain ':' (CONTRACT §3.3/§6).
        if let colon = rest.firstIndex(of: ":") {
            return String(rest[..<colon])
        }
        return rest
    }

    /// The sidebar is the core's unified list, so a peer's chats sit next to the main
    /// wire's. Peers with no sessions yet still get a drawer row from their status event.
    private func refreshSessions() {
        if let core {
            sessions = core.allSessions()
        }
        syncPeersFromSessions()
        refreshPins()
    }

    private func syncPeersFromSessions() {
        for session in sessions {
            let label = Self.peerLabel(from: session.id)
            guard !label.isEmpty else { continue }
            upsertPeer(label, status: nil)
        }
        for idx in roamPeers.indices {
            let prefix = "roam:\(roamPeers[idx].label):"
            roamPeers[idx].sessionCount = sessions.filter { $0.id.hasPrefix(prefix) }.count
        }
    }

    private func upsertPeer(_ label: String, status newStatus: String?) {
        guard !label.isEmpty else { return }
        if let idx = roamPeers.firstIndex(where: { $0.label == label }) {
            if let newStatus { roamPeers[idx].status = newStatus }
        } else {
            roamPeers.append(PeerState(label: label,
                                       status: newStatus ?? "unknown",
                                       sessionCount: 0))
        }
    }

    // MARK: - Roam intents

    func connectPeer(label: String, card: String) {
        let label = label.trimmingCharacters(in: .whitespacesAndNewlines)
        let card = card.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !label.isEmpty, !card.isEmpty else {
            status = "a peer label and a connection card are required"
            return
        }
        guard Self.validPeerLabel(label) else {
            status = "a peer label cannot be blank, padded, or contain ':'"
            return
        }
        upsertPeer(label, status: "connecting: dialing")
        core?.roamConnect(card: card, label: label)
        refreshSessions()
    }

    /// The core rejects the same labels; checking here keeps the sheet honest instead of
    /// watching a dial fail with an Alert. Labels cannot contain ':' because display ids
    /// are `roam:<label>:<id>` and ownership is resolved by label prefix.
    static func validPeerLabel(_ label: String) -> Bool {
        !label.isEmpty
            && label == label.trimmingCharacters(in: .whitespacesAndNewlines)
            && !label.contains(":")
            && !label.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
    }

    func disconnectPeer(label: String) {
        core?.roamDisconnect(label: label)
        upsertPeer(label, status: "disconnected")
        if owner(for: currentSessionId).label == label {
            currentSessionId = ""
            openAttempt = nil
            chat.reset()
            prompting = false
        }
        refreshSessions()
    }

    // MARK: - Roam card pins

    /// The core owns the pin store; this is a read-through cache for the peer row. A
    /// relaunch shows the pins again because `connect()` reads them from the core.
    func refreshPins() {
        guard let core,
              let data = core.roamCardPins().data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data),
              let pins = object as? [String: String] else {
            peerPins = [:]
            return
        }
        peerPins = pins
    }

    func cardPin(for label: String) -> String {
        peerPins[label] ?? ""
    }

    /// Forget a pin after the user confirmed a re-shared card. The NEXT dial re-enrolls
    /// that label; nothing is auto-enrolled by this client, so a stale card cannot be
    /// silently trusted again.
    func clearPeerPin(label: String) {
        guard let core else { return }
        do {
            try core.roamCardPinClear(label: label)
            status = "card pin cleared for \(label) — reconnect with the peer's card to re-enroll"
            refreshPins()
        } catch {
            status = "couldn't clear the card pin for \(label): \(error.localizedDescription)"
        }
    }

    // MARK: - Files

    /// Pull the bytes for a server-shared file row. The ROW carries the reference
    /// (path/name/mime/sha256); the bytes never enter the transcript. No new core API is
    /// needed: `fetch_file` already takes the session id and all five fields.
    func fetchFile(rowId: String) {
        guard let row = chat.row(id: rowId),
              !row.filePath.isEmpty, !row.fileSha256.isEmpty,
              let core else { return }
        core.fetchFile(sessionId: currentSessionId,
                       path: row.filePath,
                       name: row.fileName,
                       mime: row.fileMime,
                       sha256: row.fileSha256)
    }

    /// The local path is a content-addressed cache file the core already verified.
    /// Opening it is an OS action, not a protocol one — no core API involved.
    func openFile(path: String) {
        guard !path.isEmpty else { return }
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    func fileTransfer(for row: ChatRow) -> FileTransfer? {
        guard !row.filePath.isEmpty else { return nil }
        return fileTransfers.values.first {
            $0.path == row.filePath && ($0.sessionId.isEmpty || $0.sessionId == currentSessionId)
        }
    }

    var orderedFileTransfers: [FileTransfer] {
        fileTransfers.values.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Answer ONE request by its tool-call id. Removing by id (rather than clearing a
    /// single slot) is what lets a second request arrive without stranding the first.
    func answerPermission(_ optionId: String?, for id: String) {
        guard let index = permissionPrompts.firstIndex(where: { $0.id == id }) else { return }
        let prompt = permissionPrompts.remove(at: index)
        core?.respondPermission(requestId: prompt.request.toolCallId,
                                outcome: optionId.map { .selected(optionId: $0) } ?? .cancelled)
    }

    /// Escape/dismiss means deny every still-open prompt; leaving a responder parked is
    /// worse than a denial, which the server can always ask again.
    func denyAllPermissions() {
        let prompts = permissionPrompts
        permissionPrompts = []
        for prompt in prompts {
            core?.respondPermission(requestId: prompt.request.toolCallId, outcome: .cancelled)
        }
    }

    // MARK: - Recipe / elicitation forms

    /// Both forms arrive with (owner, requestId, payload). The payload is the server's raw
    /// JSON; `ServerFormPrompt` only decodes what it can render, but the request id is kept
    /// regardless so even a malformed form can be cancelled instead of parked forever.
    func handleRecipeParams(owner: String, requestId: String, parameters: String) {
        guard !requestId.isEmpty else { return }
        guard !formPrompts.contains(where: { $0.id == requestId }) else { return }
        guard let prompt = ServerFormPrompt.recipe(owner: owner, requestId: requestId,
                                                   payload: parameters) else {
            try? unstable?.respondRecipeParams(requestId: requestId, action: "cancel", values: "{}")
            return
        }
        formPrompts.append(prompt)
    }

    func handleElicitation(owner: String, requestId: String, schema: String) {
        guard !requestId.isEmpty else { return }
        guard !formPrompts.contains(where: { $0.id == requestId }) else { return }
        guard let prompt = ServerFormPrompt.elicitation(owner: owner, requestId: requestId,
                                                        payload: schema) else {
            // decline/cancel carry NO content on the ACP wire, only the action.
            try? unstable?.respondElicitation(requestId: requestId, action: "cancel", content: "")
            return
        }
        formPrompts.append(prompt)
    }

    /// `action` is the server's verb: submit/cancel for recipe params, accept/decline/cancel
    /// for elicitation. It rides WITH the request id — never in place of it.
    func answerForm(_ requestId: String, action: String, values: [String: String]) {
        guard let index = formPrompts.firstIndex(where: { $0.id == requestId }) else { return }
        let prompt = formPrompts.remove(at: index)
        let json = prompt.answerJSON(action: action, textValues: values)
        switch prompt.kind {
        case .recipe:
            let valuesJSON = action == "cancel" ? "{}" : json
            try? unstable?.respondRecipeParams(requestId: requestId, action: action, values: valuesJSON)
        case .elicitation:
            let content = (action == "cancel" || action == "decline") ? "" : json
            try? unstable?.respondElicitation(requestId: requestId, action: action, content: content)
        }
    }

    func cancelAllForms() {
        let prompts = formPrompts
        formPrompts = []
        for prompt in prompts {
            switch prompt.kind {
            case .recipe:
                try? unstable?.respondRecipeParams(requestId: prompt.requestId,
                                                   action: "cancel", values: "{}")
            case .elicitation:
                try? unstable?.respondElicitation(requestId: prompt.requestId,
                                                  action: "cancel", content: "")
            }
        }
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
            reconcileTurn()
            refreshSessions()
        } else if text == "disconnected" || text.hasPrefix("error:"),
                  !owner(for: currentSessionId).isRoam {
            // A MAIN wire loss does not end a roam chat's turn.
            prompting = false
        }
        if online && !wasOnline {
            // Every fresh Ready re-lists: the sidebar is the server's view of the world,
            // and this client keeps no copy of it.
            listSessions()
        }
    }

    func handleSessions(_ list: [SessionSummary]) {
        // The event carries the MAIN list; the sidebar shows both wires, so ask the
        // core for the union rather than replacing roam rows with an empty main list.
        sessions = core?.allSessions() ?? list
        syncPeersFromSessions()
    }

    func handleItem(_ op: TranscriptOp) {
        switch op {
        case .reset(let sessionId):
            if openAttempt != nil {
                // The core paints the displayed chat; an unrelated paint while an open
                // is pending must not overwrite the conversation being kept for rollback.
                // The reset's ARRIVAL is not success — onSessionOpened is.
                guard sessionId == currentSessionId else { return }
            } else if currentSessionId.isEmpty && !sessionId.isEmpty {
                // session/new (and a peer create) has no other first name for the chat:
                // the paint's session id is the first thing that names it.
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
            reconcileTurn()
            return
        }
        prompting = false
        activeRuns.removeValue(forKey: sessionId)
        // The row's text is final, so the view can stop treating it as a stream and render
        // it (plain-while-streaming, rich-at-finalize).
        chat.endTurn()
    }

    func handleActiveRun(sessionId: String, runId: String) {
        if runId.isEmpty {
            activeRuns.removeValue(forKey: sessionId)
        } else {
            activeRuns[sessionId] = runId
        }
        guard sessionId == currentSessionId else { return }
        if runId.isEmpty {
            // An empty run id is the core's earliest authoritative "over", often before
            // on_run_ended. Do not re-arm Stop from a stale getter here.
            prompting = false
            chat.endTurn()
        } else {
            // A run id means the core has a live turn; the getter is the tie-breaker if
            // an end event crossed this start on the core thread.
            prompting = core?.turnInFlight(sessionId: sessionId)
                ?? !activeRuns[sessionId].isEmpty
        }
    }

    func handlePermission(_ request: PermissionRequest) {
        let prompt = PermissionPrompt(request: request)
        // The core rejects duplicate tool-call ids across wires; a repeat event is the
        // same request, not a second sheet. Keep the first wrapper (its options are the
        // ones the user was shown) and ignore the duplicate.
        guard !permissionPrompts.contains(where: { $0.id == prompt.id }) else { return }
        permissionPrompts.append(prompt)
    }

    /// The prompt shows WHICH chat/wire asked. The core now stamps both fields on the
    /// request; reading them by label keeps this working whether `owner` is generated as a
    /// plain string or as the `SessionOwner` record (both shapes exist across bindings).
    static func requestField(_ name: String, in request: PermissionRequest) -> String {
        let children = Mirror(reflecting: request).children
        guard let child = children.first(where: { $0.label == name }) else { return "" }
        if let text = child.value as? String { return text }
        let nestedChildren = Mirror(reflecting: child.value).children
        func childText(_ key: String) -> String {
            nestedChildren.first(where: { $0.label == key })?.value as? String ?? ""
        }
        let label = childText("label")
        if !label.isEmpty { return label }
        let kind = childText("kind")
        return kind.isEmpty ? String(describing: child.value) : kind
    }

    func permissionSessionTitle(_ request: PermissionRequest) -> String {
        let id = Self.requestField("sessionId", in: request)
        if let title = sessions.first(where: { $0.id == id })?.title, !title.isEmpty {
            return title
        }
        return id.isEmpty ? "this chat" : id
    }

    func permissionOwner(_ request: PermissionRequest) -> String {
        let owner = Self.requestField("owner", in: request)
        return owner.isEmpty ? "main" : owner
    }

    func handleSessionTouched(sessionId: String, title: String) {
        if sessions.contains(where: { $0.id == sessionId }) {
            // Only the title arrives here; the row keeps everything else.
            refreshSessions()
        }
    }

    func handleOpened(sessionId: String) {
        guard let attempt = openAttempt,
              attempt.sessionId == currentSessionId,
              sessionId == attempt.sessionId else {
            // A new session's first bind (no open attempt because the user just created
            // it) still needs a chat id, but no snapshot to discard.
            if currentSessionId.isEmpty && !sessionId.isEmpty {
                currentSessionId = sessionId
                reconcileTurn(sessionId: sessionId)
            }
            return
        }
        openAttempt = nil
        advisorReport = nil
        // Do NOT reset the chat here. A Reset with identical cached rows emits no paint
        // change, and blanking on it is exactly the 2026-11-22 "open empties the chat" bug.
        reconcileTurn(sessionId: sessionId)
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
        // Replay completion is NOT open success: a cached paint can finish before the
        // `session/load` that can still be refused. onSessionOpened decides.
    }

    func handleFile(_ event: FileEvent) {
        let info = decodeFileEvent(event)
        let key = info.requestId.isEmpty ? "\(info.sessionId):\(info.path)" : info.requestId
        var transfer = fileTransfers[key] ?? FileTransfer(id: key,
                                                          sessionId: info.sessionId,
                                                          path: info.path,
                                                          name: info.name)
        if !info.sessionId.isEmpty { transfer.sessionId = info.sessionId }
        if !info.path.isEmpty { transfer.path = info.path }
        if !info.name.isEmpty { transfer.name = info.name }
        switch info.kind {
        case .progress:
            transfer.state = "downloading"
            transfer.received = info.received
            transfer.total = info.total
        case .saved:
            transfer.state = "saved"
            transfer.localPath = info.localPath
            transfer.received = info.bytes
            transfer.total = info.bytes
            transfer.message = ""
        case .failed:
            transfer.state = "failed"
            transfer.message = info.message
        case .unknown:
            return
        }
        fileTransfers[key] = transfer
    }

    func handleRoamStatus(label: String, status: String) {
        upsertPeer(label, status: status)
        // The first successful dial of a label enrolls its card in the core; show the
        // fingerprint as soon as the peer reports ready.
        refreshPins()
    }

    func handleRoamSessions(label: String, sessions: [SessionSummary]) {
        upsertPeer(label, status: nil)
        if let core {
            self.sessions = core.allSessions()
        } else {
            // No core to ask: keep the other rows and swap only this peer's.
            let main = self.sessions.filter { !$0.id.hasPrefix("roam:") }
            let others = self.sessions.filter {
                $0.id.hasPrefix("roam:") && !$0.id.hasPrefix("roam:\(label):")
            }
            self.sessions = main + others + sessions
        }
        syncPeersFromSessions()
    }

    func handlePeerNewSession(label: String, sessionId: String) {
        upsertPeer(label, status: nil)
        refreshSessions()
        // The peer unions its just-created (still empty) session into the very next
        // list it emits; until that round-trip lands, name the row here so it is visible
        // and tappable rather than silently lost.
        let prefixed = "roam:\(label):\(sessionId)"
        if !sessions.contains(where: { $0.id == prefixed }) {
            sessions.insert(SessionSummary(id: prefixed,
                                           title: "New chat",
                                           updatedAt: "",
                                           lastMessageSnippet: nil,
                                           projectId: nil,
                                           messageCount: 0,
                                           model: "",
                                           hasRecipe: false,
                                           hasNew: true,
                                           mode: "chat",
                                           archived: false), at: 0)
        }
        syncPeersFromSessions()
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

/// A peer observed through the core's roam callbacks. The connection card is never
/// copied here — it is a capability, and the core owns it.
struct PeerState: Identifiable {
    var id: String { label }
    let label: String
    var status: String
    var sessionCount: Int
}

/// The UI's view of one download. Keyed by the core's request id, so two chats fetching
/// the same server path cannot overwrite each other's progress.
struct FileTransfer: Identifiable {
    let id: String
    var sessionId: String
    var path: String
    var name: String
    var received: UInt64 = 0
    var total: UInt64 = 0
    var state: String = "downloading"
    var message: String = ""
    var localPath: String = ""
}

// MARK: - FileEvent decoding

private enum FileEventKind {
    case progress, saved, failed, unknown
}

private struct FileEventInfo {
    let kind: FileEventKind
    let sessionId: String
    let requestId: String
    let path: String
    let name: String
    let received: UInt64
    let total: UInt64
    let mime: String
    let bytes: UInt64
    let localPath: String
    let message: String
}

/// Read the generated enum's associated values BY LABEL.
///
/// The core is adding `sessionId`/`requestId` to all three `FileEvent` variants; a
/// positional Swift pattern (`case .saved(let path, …)`) would silently bind the wrong
/// fields when those are inserted. Matching the case without its payload and reflecting
/// the labels keeps this client correct across both shapes. The positional indexes are
/// only a fallback for a generator that drops labels.
private func decodeFileEvent(_ event: FileEvent) -> FileEventInfo {
    let kind: FileEventKind
    switch event {
    case .progress: kind = .progress
    case .saved: kind = .saved
    case .failed: kind = .failed
    @unknown default: kind = .unknown
    }

    var labelled: [String: String] = [:]
    var strings: [String] = []
    var numbers: [UInt64] = []
    let mirror = Mirror(reflecting: event)
    for child in mirror.children {
        if let label = child.label, let text = child.value as? String {
            labelled[label] = text
        }
        if let text = child.value as? String { strings.append(text) }
        if let number = child.value as? UInt64 { numbers.append(number) }
    }
    func text(_ label: String, _ index: Int) -> String {
        labelled[label] ?? (strings.indices.contains(index) ? strings[index] : "")
    }
    func number(_ label: String, _ index: Int) -> UInt64 {
        if let raw = labelled[label], let value = UInt64(raw) { return value }
        return numbers.indices.contains(index) ? numbers[index] : 0
    }
    // The two ownership fields are appended to every variant. If an enum generator
    // erases labels, they are the LAST two strings, after the variant's own text fields.
    let baseStrings: Int
    switch kind {
    case .progress: baseStrings = 2              // path, name
    case .saved: baseStrings = 4                 // path, name, mime, localPath
    case .failed: baseStrings = 3                // path, name, message
    case .unknown: baseStrings = 0
    }
    let hasOwnerFields = strings.count >= baseStrings + 2
    func sessionId() -> String {
        labelled["sessionId"] ?? (hasOwnerFields ? strings[strings.count - 2] : "")
    }
    func requestId() -> String {
        labelled["requestId"] ?? (hasOwnerFields ? strings[strings.count - 1] : "")
    }

    return FileEventInfo(
        kind: kind,
        sessionId: sessionId(),
        requestId: requestId(),
        path: text("path", 0),
        name: text("name", 1),
        received: number("received", 0),
        total: number("total", 1),
        mime: text("mime", 2),
        bytes: number("bytes", 0),
        localPath: text("localPath", 3),
        message: text("message", 2)
    )
}

/// A permission request as sheet state. Identity is the core's tool-call id, which is also
/// the key of the responder on the owning wire: answering by this id cannot hit another
/// chat's prompt, and a second request cannot replace an unanswered one.
struct PermissionPrompt: Identifiable {
    var id: String { request.toolCallId }
    let request: PermissionRequest
}

/// The advisor's review of the turn that just ended. Not part of the transcript: it is
/// about the turn, and replaying it as history would have the advisor reviewing itself.
struct AdvisorReport: Identifiable {
    let id = UUID()
    let sessionId: String
    let text: String
}
