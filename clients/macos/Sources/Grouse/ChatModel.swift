import Foundation
import SwiftUI
import GrouseCore

/// One transcript row, shaped for the view rather than for the wire.
///
/// `id` is the CORE's item id, and it is the view's identity: the core addresses every
/// update by that id, so a row must keep it or the list re-shuffles on each chunk.
struct ChatRow: Identifiable, Equatable {
    let id: String
    var kind: String        // user | agent | thought | tool | toolGroup | error | file | chart | mcpApp
    var text: String
    var detail: String
    var output: String
    var status: String
    /// `Item.files[0]` on an `ItemKind::File` row: a REFERENCE (server path + digest),
    /// never bytes. The view uses it to drive `core.fetch_file`; the bytes come back on
    /// `on_file` and land in the core's cache, not in this model.
    var filePath: String = ""
    var fileName: String = ""
    var fileMime: String = ""
    var fileSha256: String = ""
    var fileBytes: UInt64 = 0

    var isAssistant: Bool { kind == "agent" }
    var isUser: Bool { kind == "user" }
    var isToolish: Bool { kind == "tool" || kind == "toolGroup" }
}

/// The transcript the chat view draws, built from the core's ops.
///
/// The core owns the conversation; this is a projection of it, and it follows the same
/// rules the other two clients follow — each of which was learned the hard way:
///
/// * **Never republish the list.** A chunk appends to ONE row (see `appendText`); replacing
///   the array per token makes the list a brand-new model and costs the scroll position.
/// * **Identity is the core's item id**, so appends and completions update a row instead of
///   reordering the list.
/// * **A replay resets**, because the core says so (`reset`) — it is not the UI's decision
///   to keep or drop rows when the server repaints a conversation.
final class ChatModel: ObservableObject, @unchecked Sendable {
    @Published private(set) var rows: [ChatRow] = []
    /// True while the newest row is still being written: the view renders that row as plain
    /// text (re-parsing Markdown per token is the quadratic trap the desktop documents).
    @Published private(set) var streamingId: String?
    /// `window.has_older` from the last Window op — history the core can still page in.
    @Published private(set) var hasOlder = false

    /// Everything needed to put the previous conversation back if a chat open fails
    /// after the core has already painted the target (a cached paint precedes the
    /// `session/load` that can still be refused).
    struct Snapshot {
        let rows: [ChatRow]
        let streamingId: String?
        let hasOlder: Bool
    }

    private var index: [String: Int] = [:]

    func snapshot() -> Snapshot {
        Snapshot(rows: rows, streamingId: streamingId, hasOlder: hasOlder)
    }

    func restore(_ snapshot: Snapshot) {
        rows = snapshot.rows
        index = [:]
        for (i, row) in rows.enumerated() { index[row.id] = i }
        streamingId = snapshot.streamingId
        hasOlder = snapshot.hasOlder
    }

    /// The reference a file row needs for a download. Identity is the core's item id,
    /// not the server path: two messages can link the same path.
    func row(id: String) -> ChatRow? {
        rows.first { $0.id == id }
    }

    func reset() {
        rows = []
        index = [:]
        streamingId = nil
        hasOlder = false
    }

    func apply(_ op: TranscriptOp) {
        switch op {
        case .reset:
            reset()

        case .upsert(let item):
            let shared = item.files.first
            let row = ChatRow(
                id: item.id,
                kind: kindName(item.kind),
                text: item.text,
                detail: item.detail,
                output: item.output,
                status: item.status,
                filePath: shared?.path ?? "",
                fileName: shared?.name ?? "",
                fileMime: shared?.mime ?? "",
                fileSha256: shared?.sha256 ?? "",
                fileBytes: shared?.bytes ?? 0
            )
            if let idx = index[row.id] {
                rows[idx] = row
            } else {
                index[row.id] = rows.count
                rows.append(row)
            }

        case .appendText(let id, let chunk):
            guard let idx = index[id] else { return }
            rows[idx].text += chunk
            streamingId = id

        case .appendOutput(let id, let chunk):
            // Live tool output is bounded in the core; the row shows the tail of it, which
            // is what a long build log is useful for.
            guard let idx = index[id] else { return }
            rows[idx].output = String((rows[idx].output + chunk).suffix(4000))

        case .remove(let id):
            guard let idx = index[id] else { return }
            rows.remove(at: idx)
            reindex(from: idx)

        case .window(let oldestId, let hasOlder):
            self.hasOlder = hasOlder
            // The core may hand back a window that starts later than what we hold (a fresh
            // open, newest-first). Rows the window no longer covers are dropped, so the
            // painted transcript and the core's cursor agree.
            guard !oldestId.isEmpty, let start = index[oldestId], start > 0 else { return }
            rows.removeFirst(start)
            reindex(from: 0)
        }
    }

    /// The model stops streaming when the turn ends: the row's final text is already there,
    /// and the view switches it to rendered Markdown.
    func endTurn() {
        streamingId = nil
    }

    private func reindex(from start: Int) {
        index = index.filter { $0.value < start }
        for i in start..<rows.count { index[rows[i].id] = i }
    }

    private func kindName(_ kind: ItemKind) -> String {
        // Spelled out rather than `String(describing:)`: the raw Swift case name is
        // `toolGroup`, and the views switch on these strings.
        switch kind {
        case .user: return "user"
        case .agent: return "agent"
        case .thought: return "thought"
        case .tool: return "tool"
        case .toolGroup: return "toolGroup"
        case .chart: return "chart"
        case .mcpApp: return "mcpApp"
        case .file: return "file"
        case .error: return "error"
        @unknown default: return "agent"
        }
    }
}
