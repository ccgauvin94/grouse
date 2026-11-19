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
            .sheet(item: $bridge.permission) { prompt in
                PermissionSheet(prompt: prompt) { optionId in
                    bridge.answerPermission(optionId)
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

            Button("Connect") { bridge.connect() }
                .keyboardShortcut(.defaultAction)
                .disabled(bridge.host.isEmpty || bridge.secretKey.isEmpty)

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

    var body: some View {
        List(bridge.sessions, id: \.id, selection: Binding(
            get: { bridge.currentSessionId },
            set: { id in if let id, id != bridge.currentSessionId { bridge.open(sessionId: id) } }
        )) { session in
            SessionRow(session: session)
                .tag(session.id)
        }
        .listStyle(.sidebar)
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
