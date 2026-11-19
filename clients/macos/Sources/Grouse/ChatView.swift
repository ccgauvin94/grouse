import SwiftUI
import GrouseCore

/// The conversation, the composer, and the advisor's note.
///
/// Scroll PINNING here is deliberately the simple version — follow when a row is appended
/// and when the reader sends. The known gap is the one the other two clients had to solve
/// properly: a row whose height is not final when the pin runs (a long message that
/// re-wraps, an image that decodes later) leaves the view a few points short of the end.
/// Phase 2 replaces this with a pin that re-asserts from layout and stops only when the
/// content height stops changing (AGENTS.md, "Scroll is a mode").
struct ChatView: View {
    @EnvironmentObject private var bridge: CoreBridge
    // The transcript is its OWN observable object: `@Published` on a nested model does not
    // reach a view that only holds the parent, so observing it explicitly is what makes a
    // streamed chunk redraw the row at all.
    @ObservedObject private var chat: ChatModel
    @State private var draft = ""

    init(chat: ChatModel) {
        self.chat = chat
    }

    var body: some View {
        VStack(spacing: 0) {
            TranscriptView(chat: chat)

            if let report = bridge.advisorReport {
                AdvisorCard(report: report) { bridge.dismissAdvisorReport() }
            }

            ComposerView(draft: $draft)
        }
        .navigationTitle(title)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    bridge.cancel()
                } label: {
                    Label("Stop", systemImage: "stop.circle")
                }
                .disabled(!bridge.prompting)
                .help("Stop the running turn")
            }
        }
    }

    private var title: String {
        bridge.sessions.first { $0.id == bridge.currentSessionId }?.title ?? "Chat"
    }
}

struct TranscriptView: View {
    @EnvironmentObject private var bridge: CoreBridge
    @ObservedObject var chat: ChatModel

    private static let bottom = "bottom"

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(chat.rows) { row in
                        ChatRowView(row: row, streaming: chat.streamingId == row.id)
                            .id(row.id)
                    }
                    Color.clear.frame(height: 1).id(Self.bottom)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Follow the newest row. Two cheap triggers: a row appearing (new message,
            // tool chip) and the turn state changing (a send, and a turn ending).
            .onChange(of: chat.rows.count) { _, _ in follow(proxy) }
            .onChange(of: bridge.prompting) { _, _ in follow(proxy) }
        }
    }

    private func follow(_ proxy: ScrollViewProxy) {
        withAnimation(.easeOut(duration: 0.15)) {
            proxy.scrollTo(Self.bottom, anchor: .bottom)
        }
    }
}

struct ChatRowView: View {
    let row: ChatRow
    let streaming: Bool

    var body: some View {
        switch row.kind {
        case "user":
            HStack {
                Spacer(minLength: 40)
                Text(row.text)
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Color.accentColor.opacity(0.18),
                                in: RoundedRectangle(cornerRadius: 10))
            }

        case "agent":
            AgentText(text: row.text, streaming: streaming)

        case "thought":
            CollapsibleNote(title: "Thinking", text: row.text, icon: "brain")

        case "tool", "toolGroup":
            ToolRow(row: row)

        case "error":
            Label(row.text, systemImage: "exclamationmark.triangle")
                .font(.callout)
                .foregroundStyle(.red)
                .textSelection(.enabled)

        case "file":
            Label(row.text, systemImage: "doc")
                .font(.callout)
                .textSelection(.enabled)

        default:
            Text(row.text.isEmpty ? row.kind : row.text)
                .font(.callout)
                .textSelection(.enabled)
        }
    }
}

/// Streamed text is plain; finished text is rendered.
///
/// Re-parsing Markdown on every token is the quadratic trap the Qt client documents, so the
/// switch happens once, at the turn's end. `inlineOnlyPreservingWhitespace` keeps the
/// model's own line breaks and does not try to build block structure (lists and code fences
/// as real views are phase 2, with a proper renderer).
struct AgentText: View {
    let text: String
    let streaming: Bool

    var body: some View {
        Group {
            if streaming {
                Text(text)
            } else if let attributed = try? AttributedString(
                markdown: text,
                options: AttributedString.MarkdownParsingOptions(
                    interpretedSyntax: .inlineOnlyPreservingWhitespace
                )
            ) {
                Text(attributed)
            } else {
                Text(text)
            }
        }
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct ToolRow: View {
    let row: ChatRow
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button {
                expanded.toggle()
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "wrench.and.screwdriver").font(.caption2)
                    Text(row.text.isEmpty ? "tool" : row.text).lineLimit(1)
                    if !row.status.isEmpty && row.status != "completed" {
                        Text(row.status).font(.caption2).foregroundStyle(.secondary)
                    }
                    Spacer()
                }
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)

            if expanded {
                if !row.detail.isEmpty {
                    Text(row.detail)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .lineLimit(12)
                }
                if !row.output.isEmpty {
                    Text(row.output)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .lineLimit(20)
                }
            }
        }
        .padding(.vertical, 2)
    }
}

struct CollapsibleNote: View {
    let title: String
    let text: String
    let icon: String
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button {
                expanded.toggle()
            } label: {
                Label(title, systemImage: icon)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            if expanded {
                Text(text)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
        }
    }
}

struct ComposerView: View {
    @EnvironmentObject private var bridge: CoreBridge
    @Binding var draft: String

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField(bridge.prompting ? "Queue a message…" : "Message Goose…",
                      text: $draft, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...8)
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
                .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))

            Button {
                let text = draft
                draft = ""
                bridge.send(text)
            } label: {
                Image(systemName: "arrow.up.circle.fill").font(.title2)
            }
            .buttonStyle(.plain)
            .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            // ⌘↩ sends and Return inserts a newline: a chat composer that sends on plain
            // Return cannot hold a pasted paragraph.
            .keyboardShortcut(.return, modifiers: .command)
            .help("Send (⌘↩)")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.bar)
    }
}

/// The advisor's review of the turn that just ended. Outside the transcript on purpose: it
/// describes the turn, and replayed as history it would have the advisor reviewing itself.
struct AdvisorCard: View {
    let report: AdvisorReport
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: "checkmark.seal").foregroundStyle(.tint)
                Text("Advisor").font(.caption.weight(.semibold))
                Spacer()
                Button(action: onDismiss) {
                    Image(systemName: "xmark").font(.caption2)
                }
                .buttonStyle(.plain)
                .help("Dismiss")
            }
            ScrollView {
                Text(report.text)
                    .font(.caption)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 150)
        }
        .padding(10)
        .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        .overlay(alignment: .leading) {
            Rectangle().fill(Color.accentColor).frame(width: 3)
        }
        .padding(.horizontal, 14)
        .padding(.top, 6)
    }
}

struct PermissionSheet: View {
    let prompt: PermissionPrompt
    let onAnswer: (String?) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Tool approval").font(.headline)
            Text(prompt.request.title)
                .font(.callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            if !prompt.request.detail.isEmpty {
                ScrollView {
                    Text(prompt.request.detail)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 160)
            }
            Divider()
            // The SERVER decides what the answers are; this only labels them. A hardcoded
            // allow/deny pair is what made the KDE dialog offer nothing but "Deny".
            ForEach(prompt.request.options, id: \.optionId) { option in
                Button(pretty(option)) { onAnswer(option.optionId) }
                    .frame(maxWidth: .infinity)
            }
            Button("Cancel") { onAnswer(nil) }
                .keyboardShortcut(.cancelAction)
                .frame(maxWidth: .infinity)
        }
        .padding(18)
        .frame(width: 420)
    }

    private func pretty(_ option: PermissionOption) -> String {
        switch option.kind {
        case "allow_once": return "Allow once"
        case "allow_always": return "Always allow"
        case "reject_once": return "Deny"
        case "reject_always": return "Always deny"
        default: return option.name
        }
    }
}
