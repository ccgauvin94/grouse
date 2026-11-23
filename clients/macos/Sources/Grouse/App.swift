import SwiftUI

/// The app. One window, one connection, one chat on screen — the same shape as the KDE
/// client, because the core is the same and the surfaces map one-to-one.
@main
struct GrouseApp: App {
    @StateObject private var bridge = CoreBridge()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(bridge)
                .frame(minWidth: 760, minHeight: 480)
        }
        .commands {
            CommandGroup(after: .newItem) {
                Button("New Chat") { bridge.newChat() }
                    .keyboardShortcut("n", modifiers: .command)
                Button("New Dev Chat") { bridge.newChat(mode: "dev") }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
                Divider()
                Button("Reload Session List") { bridge.listSessions() }
                    .keyboardShortcut("r", modifiers: .command)
            }
        }
    }
}

struct RootView: View {
    @EnvironmentObject private var bridge: CoreBridge

    var body: some View {
        if !bridge.connected {
            ConnectView()
        } else {
            NavigationSplitView {
                SidebarView()
                    .navigationSplitViewColumnWidth(min: 220, ideal: 260, max: 360)
            } detail: {
                if bridge.currentSessionId.isEmpty {
                    EmptyChatView()
                } else {
                    ChatView(chat: bridge.chat)
                }
            }
            // ONE presented sheet over two QUEUES: a second request appends instead of
            // replacing the first, and answering by id reveals the next one. SwiftUI only
            // presents one sheet per hierarchy reliably, so the form queue follows the
            // approval queue rather than stacking a second `.sheet` modifier. Dismissing
            // without choosing denies/cancels every parked responder instead of stranding
            // a wire that is waiting for an answer.
            .sheet(isPresented: Binding(
                get: { !bridge.permissionPrompts.isEmpty || !bridge.formPrompts.isEmpty },
                set: { presented in
                    if !presented {
                        bridge.denyAllPermissions()
                        bridge.cancelAllForms()
                    }
                }
            )) {
                if let prompt = bridge.activePermission {
                    PermissionSheet(
                        prompt: prompt,
                        chatTitle: bridge.permissionSessionTitle(prompt.request),
                        owner: bridge.permissionOwner(prompt.request),
                        remaining: bridge.permissionPrompts.count
                    ) { id, optionId in
                        bridge.answerPermission(optionId, for: id)
                    }
                } else if let form = bridge.activeForm {
                    ServerFormSheet(prompt: form) { id, action, values in
                        bridge.answerForm(id, action: action, values: values)
                    }
                }
            }
            .safeAreaInset(edge: .bottom) {
                // The connection line is a status bar, not chrome: it says what the wire is
                // doing without taking a row from the conversation.
                StatusBar()
            }
        }
    }
}

/// First run, and every later launch until a connection is made.
struct ConnectView: View {
    @EnvironmentObject private var bridge: CoreBridge

    var body: some View {
        VStack(spacing: 16) {
            Text("Grouse").font(.largeTitle.weight(.semibold))
            Text("A native client for your own goose server.")
                .foregroundStyle(.secondary)

            Form {
                TextField("Host or tailnet IP", text: $bridge.host)
                TextField("Port", text: $bridge.port)
                SecureField("Secret key", text: $bridge.secretKey)
                Toggle("Use TLS (wss)", isOn: $bridge.useTls)
                TextField("Working directory on the server", text: $bridge.cwd)
            }
            .formStyle(.grouped)
            .frame(maxWidth: 460)

            Button { bridge.connect() } label: {
                if bridge.isConnecting {
                    ProgressView().controlSize(.small)
                } else {
                    Text("Connect")
                }
            }
            .keyboardShortcut(.defaultAction)
            .disabled(bridge.host.isEmpty || bridge.secretKey.isEmpty || bridge.isConnecting)

            if bridge.status != "disconnected" {
                Text(bridge.status).foregroundStyle(.secondary).font(.callout)
            }
            Text("The working directory must exist inside the goose container — `session/new` requires one.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 460)
        }
        .padding(40)
    }
}

struct SidebarView: View {
    @EnvironmentObject private var bridge: CoreBridge
    @State private var showPeerSheet = false

    var body: some View {
        List(selection: Binding(
            get: { bridge.currentSessionId },
            set: { id in if let id, id != bridge.currentSessionId { bridge.open(sessionId: id) } }
        )) {
            Section("Chats") {
                ForEach(bridge.sessions) { session in
                    SessionRow(session: session)
                        .tag(session.id)
                }
            }
            if !bridge.roamPeers.isEmpty {
                Section("Peers") {
                    ForEach(bridge.roamPeers) { peer in
                        PeerRow(peer: peer)
                    }
                }
            }
        }
        .listStyle(.sidebar)
        .sheet(isPresented: $showPeerSheet) {
            PeerConnectSheet()
        }
        .overlay {
            if bridge.sessions.isEmpty {
                ContentUnavailableView(
                    "No chats yet",
                    systemImage: "bubble.left.and.bubble.right",
                    description: Text("Start one with ⌘N, or ⌘⇧N for a dev chat.")
                )
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu {
                    Button("New Chat") { bridge.newChat() }
                    Button("New Dev Chat") { bridge.newChat(mode: "dev") }
                    Divider()
                    Button("Reload") { bridge.listSessions() }
                    Divider()
                    Button("Add Roam Peer…") { showPeerSheet = true }
                    Button("Disconnect") { bridge.disconnect() }
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
    }
}

struct SessionRow: View {
    let session: SessionSummary

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 4) {
                // "</>" marks a dev session — a readout, not a toggle: the mode is a tag
                // written at creation and the server cannot change one afterwards.
                if session.mode == "dev" {
                    Image(systemName: "chevron.left.forwardslash.chevron.right")
                        .font(.caption2)
                        .foregroundStyle(.tint)
                        .help("Dev session")
                }
                Text(session.title.isEmpty ? "Untitled chat" : session.title)
                    .lineLimit(1)
            }
            Text(session.lastMessageSnippet?.isEmpty == false
                 ? session.lastMessageSnippet!
                 : "\(session.messageCount) msgs")
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(.vertical, 2)
        .opacity(session.archived ? 0.5 : 1)
    }
}

/// A peer observed through `on_roam_peer_status` / `on_roam_sessions`. Its chats are in
/// the Chats section above (the core's `all_sessions()` already carries the `roam:` ids);
/// this row is the connection itself — status, a new chat, and a way to close it.
struct PeerRow: View {
    @EnvironmentObject private var bridge: CoreBridge
    let peer: PeerState
    @State private var showPinConfirm = false

    private var pin: String { bridge.cardPin(for: peer.label) }

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(peer.status.hasPrefix("ready") ? Color.green
                      : peer.status.hasPrefix("error") ? Color.red : Color.secondary)
                .frame(width: 7, height: 7)
            VStack(alignment: .leading, spacing: 2) {
                Text(peer.label).lineLimit(1)
                Text("\(peer.status) · \(peer.sessionCount) chats")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                if !pin.isEmpty {
                    // A fingerprint readout, not a secret: the core pins the card on every
                    // dial and refuses a changed one until it is explicitly cleared here.
                    Text("card pin \(pin.prefix(12))…")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
        }
        .contextMenu {
            Button("New Chat Here") { bridge.newChat(peer: peer.label) }
            Button("Clear Card Pin…") { showPinConfirm = true }
                .disabled(pin.isEmpty)
            Button("Disconnect Peer") { bridge.disconnectPeer(label: peer.label) }
        }
        .confirmationDialog("Forget the pinned card for \(peer.label)?", isPresented: $showPinConfirm) {
            Button("Clear Pin", role: .destructive) {
                bridge.clearPeerPin(label: peer.label)
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The next dial re-enrolls the card you paste. Verify the new card with the peer out of band first.")
        }
    }
}

/// A connection card is a capability: paste it here, give the peer a label, and let the
/// core own the card and the iroh identity from then on.
struct PeerConnectSheet: View {
    @EnvironmentObject private var bridge: CoreBridge
    @Environment(\.dismiss) private var dismiss
    @State private var label = ""
    @State private var card = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Connect to a roam peer").font(.headline)
            TextField("Peer label (e.g. home)", text: $label)
            if !label.isEmpty && !CoreBridge.validPeerLabel(label) {
                Text("A label cannot be padded, blank, or contain ':' — roam ids are roam:<label>:<id>.")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            Text("Connection card from `goose serve --roam`")
                .font(.caption)
                .foregroundStyle(.secondary)
            TextEditor(text: $card)
                .font(.caption.monospaced())
                .frame(height: 110)
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(.quaternary))
            if bridge.status.contains("peer") {
                Text(bridge.status).font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                Button("Connect") {
                    bridge.connectPeer(label: label, card: card)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!CoreBridge.validPeerLabel(label)
                          || card.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(18)
        .frame(width: 440)
    }
}

struct EmptyChatView: View {
    @EnvironmentObject private var bridge: CoreBridge

    var body: some View {
        ContentUnavailableView {
            Label("No chat open", systemImage: "message")
        } description: {
            Text("Pick a chat on the left, or start a new one.")
        } actions: {
            Button("New Chat") { bridge.newChat() }
                .keyboardShortcut(.defaultAction)
        }
    }
}

struct StatusBar: View {
    @EnvironmentObject private var bridge: CoreBridge

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(bridge.online ? Color.green : Color.secondary)
                .frame(width: 7, height: 7)
            Text(bridge.status).font(.caption).foregroundStyle(.secondary)
            Spacer()
            if bridge.contextSize > 0 {
                // The same reading the other clients put next to their donut; a real ring
                // is phase 2, the number is the honest part of it.
                Text("\(bridge.contextUsed)/\(bridge.contextSize) tokens")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
            if bridge.replayProgress.total > 0 && !bridge.replayProgress.done {
                Text("loading \(bridge.replayProgress.loaded)/\(bridge.replayProgress.total)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
        .background(.bar)
    }
}
