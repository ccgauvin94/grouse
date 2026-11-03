// SPDX-License-Identifier: AGPL-3.0-or-later

//! File ingest: the client half of the `attachments-mcp` extension.
//!
//! An ACP client can send an image inline and nothing else. goose's prompt
//! converter DROPS a `resource` block whose contents are a blob (no `else` in
//! `convert_acp_prompt_to_message`), so the old path -- put the bytes in the
//! prompt -- showed a chip, echoed "1 attachment", and delivered nothing. The
//! bytes have to go somewhere else: this module drives a chunked upload through
//! `_goose/unstable/tools/call`, and the prompt then carries an ordinary
//! `resource_link` pointing at the path the server wrote.
//!
//! WHY THIS LIVES IN THE CORE. Three clients share this crate (Android, the
//! desktop, the CLI), and every rule here is a protocol rule, not a UI one: the
//! chunk size, the offset bookkeeping, the `auto`-mode requirement, the shape of
//! the link block. A rule that differs per app is a bug fixed three times --
//! the same argument `imgrescale.rs` makes for the image path. The apps keep
//! what only they can do: picking a file and streaming its bytes.
//!
//! Everything here is pure (params in, JSON out) so it is testable without a
//! server; the plumbing lives on `Core`.

use serde_json::{json, Value};

use crate::PromptBlock;

/// The extension, as the config names it. goose normalises `extensionName`
/// (`name_to_key`), so the display name is accepted here; the TOOL name is
/// compared verbatim and so needs the key.
pub(crate) const EXTENSION: &str = "Attachments";

/// The extension key, which is what prefixes every tool name.
pub(crate) const EXTENSION_KEY: &str = "attachments";

/// Raw bytes per chunk. 8 MiB base64-encodes to ~10.7 MiB, which clears every
/// hop we measured: tungstenite's 64 MiB message limit, goose's dispatch, and
/// the 16 MiB `MAX_POST_BODY_BYTES` on the HTTP ACP transport (unused by this
/// client today, but a client that used it would be capped there).
pub const CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Mirrors the server's own ceiling so an oversized file fails here, with a
/// message the UI can show, instead of after a partial upload.
pub const MAX_BYTES: u64 = 100 * 1024 * 1024;

/// The two tools that bring a file the OTHER way (server -> client).
pub(crate) const SHARE_TOOL: &str = "attachment_share";
pub(crate) const READ_TOOL: &str = "attachment_read";

/// Raw bytes asked for per `attachment_read`.
///
/// 512 KiB, deliberately far below the upload path's 8 MiB: this direction's
/// floor is `RoamCodec::MAX_FRAME_BYTES = 1 MiB` (`roam.rs`), and bytes past that
/// cap are DROPPED until the next newline — an oversized frame is a corrupt
/// download, not a slow one. 512 KiB base64-encodes to ~683 KiB, leaving the JSON
/// envelope comfortable room. The WebSocket transport would take far more (32 MiB
/// measured against the live server); this is the default precisely because the
/// tighter transport must never bite by accident.
pub const DOWNLOAD_CHUNK: usize = 512 * 1024;

/// `_goose/unstable/tools/call` params for one attachments tool.
///
/// `extensionName` is REQUIRED by the server's `GooseToolCallRequest` -- the
/// older two-argument shape in `unstable.rs` omitted it and could never have
/// worked -- and the tool name must be the full `<key>__<tool>`.
pub(crate) fn tool_params(wire: &str, tool: &str, arguments: Value) -> Value {
    json!({
        "sessionId": wire,
        "extensionName": EXTENSION,
        "name": format!("{EXTENSION_KEY}__{tool}"),
        "arguments": arguments,
    })
}

pub(crate) fn begin_params(wire: &str, name: &str, mime: &str, size: u64) -> Value {
    tool_params(wire, "attachment_begin", json!({
        "name": name,
        "mime": mime,
        "size": size,
    }))
}

pub(crate) fn chunk_params(wire: &str, upload_id: &str, offset: u64, data_b64: &str) -> Value {
    tool_params(wire, "attachment_chunk", json!({
        "uploadId": upload_id,
        "offset": offset,
        "data": data_b64,
    }))
}

pub(crate) fn commit_params(wire: &str, upload_id: &str) -> Value {
    tool_params(wire, "attachment_commit", json!({ "uploadId": upload_id }))
}

/// `(offset, len)` for each chunk of `total` bytes.
///
/// Pure, and tested at the boundaries that actually occur: an empty file, an
/// exact multiple of the chunk size, and a remainder. An off-by-one here would
/// show up as a server refusal ("offset N does not match the M bytes already
/// received") on the LAST chunk only, which is the expensive kind of bug.
pub(crate) fn chunk_plan(total: u64, chunk: usize) -> Vec<(u64, usize)> {
    let chunk = chunk.max(1) as u64;
    let mut plan = Vec::new();
    let mut offset = 0;
    while offset < total {
        let len = chunk.min(total - offset);
        plan.push((offset, len as usize));
        offset += len;
    }
    plan
}

/// goose's own rendering of a `resource_link` it did NOT inline.
///
/// Verbatim from `crates/goose/src/acp/server.rs` (confirmed in the shipped
/// binary): `format!("\n\n--- Resource link (not inlined) ---\n{}\n---", …)`.
/// The client cannot choose this text, which is why recognising it is the only
/// way back to the file a replayed prompt carried: the server stores the
/// RENDERED prompt, not our record.
const LINK_MARKER: &str = "--- Resource link (not inlined) ---";

/// A MIME type guessed from a name's suffix. Only used to label a file recovered
/// from a replay (the link stub carries a name and a URI, no MIME).
pub(crate) fn mime_for_name(name: &str) -> String {
    let ext = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "odt" => "application/vnd.oasis.opendocument.text",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "xml" => "text/xml",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

/// Split a RE-READ user message into its visible text and the files it carried.
///
/// A prompt we echoed at send time keeps its own file record (`Item.files`), but
/// one the server replays has only what goose stored: the rendered prompt. For a
/// file that means the link stub, so the bubble used to show a JSON object where
/// the attachment had been — and, worse, said nothing at all about it being an
/// attachment.
///
/// Returns the text unchanged (`files` empty) when the marker is absent or the
/// JSON does not parse, so a malformed stub degrades to what the old clients
/// showed rather than eating the message.
///
/// The recovered record has no size (`bytes: 0`) and no hash: the stub carries
/// neither, and inventing one would make a card claim a figure nothing measured.
pub(crate) fn split_links(text: &str) -> (String, Vec<UploadedFile>) {
    if !text.contains(LINK_MARKER) {
        return (text.to_string(), Vec::new());
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut files: Vec<UploadedFile> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let is_stub = lines[i].trim() == LINK_MARKER
            && i + 2 < lines.len()
            && lines[i + 2].trim() == "---";
        if is_stub {
            if let Some(file) = link_from_stub(lines[i + 1]) {
                files.push(file);
                i += 3;
                continue;
            }
        }
        kept.push(lines[i]);
        i += 1;
    }
    if files.is_empty() {
        return (text.to_string(), Vec::new());
    }
    (kept.join("\n").trim().to_string(), files)
}

/// One `{"name":…,"uri":…}` line, as an `UploadedFile`. `None` when it is not
/// that shape — a `file:` URI is required, because anything else is not a path
/// this client ever uploaded.
fn link_from_stub(line: &str) -> Option<UploadedFile> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_string();
    let uri = value.get("uri")?.as_str()?;
    if name.is_empty() || !uri.starts_with("file://") {
        return None;
    }
    Some(UploadedFile {
        mime: mime_for_name(&name),
        name,
        // Unknown after a replay: the stub stores neither. UIs show a kind
        // without a size rather than a made-up one.
        bytes: 0,
        // Still percent-encoded, exactly as it was sent (see `file_uri`); the
        // path is an identifier here, not something a UI prints.
        path: uri.trim_start_matches("file://").to_string(),
        sha256: String::new(),
    })
}

/// A `file://` URI for a server-side path.
///
/// goose only inlines a `resource_link` when the URI parses as `file:` AND the
/// file is readable on ITS filesystem -- which is the whole point: a text file
/// arrives as its contents, a binary as a path the agent can act on. Encoding
/// matters because an unparseable URI silently downgrades to an opaque stub, so
/// the characters that break `Url::parse` are escaped (paths are made of
/// `[A-Za-z0-9._-]` by the server, so this is belt-and-braces).
pub(crate) fn file_uri(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    out.push_str("file://");
    for byte in path.bytes() {
        match byte {
            b' ' => out.push_str("%20"),
            b'%' => out.push_str("%25"),
            b'#' => out.push_str("%23"),
            b'?' => out.push_str("%3F"),
            b'"' => out.push_str("%22"),
            b'<' => out.push_str("%3C"),
            b'>' => out.push_str("%3E"),
            b'`' => out.push_str("%60"),
            b'{' => out.push_str("%7B"),
            b'}' => out.push_str("%7D"),
            other if other < 0x20 => out.push_str(&format!("%{other:02X}")),
            other => out.push(other as char),
        }
    }
    out
}

/// The prompt block that hands an uploaded file to the agent.
pub(crate) fn link_block(file: &UploadedFile) -> PromptBlock {
    PromptBlock::Link {
        uri: file_uri(&file.path),
        name: file.name.clone(),
        mime_type: file.mime.clone(),
    }
}

/// A file on the client's OWN disk.
///
/// The desktop is a local app, so its attachments are paths and the core can
/// open them itself -- which keeps that client's whole C ABI addition to one
/// function taking a JSON array of paths, instead of a callback vtable. Android
/// resolves `content://` URIs and cannot do that, so it implements
/// [`crate::FileSource`] instead. The upload protocol above is identical either
/// way; only "where do the bytes come from" differs.
pub struct PathFileSource {
    path: std::path::PathBuf,
    name: String,
    mime: String,
    size: u64,
    /// Open handle plus the offset it sits at. Re-opened when the caller asks
    /// for a position before it, which is what a retry after a dropped socket
    /// looks like -- the same rule the Android source follows.
    handle: std::sync::Mutex<Option<(std::fs::File, u64)>>,
}

impl PathFileSource {
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("not a regular file".to_string());
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let mime = match path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .as_deref()
        {
            Some("pdf") => "application/pdf",
            Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Some("txt") => "text/plain",
            Some("md") => "text/markdown",
            Some("csv") => "text/csv",
            Some("json") => "application/json",
            Some("png") => "image/png",
            Some("jpg") | Some("jpeg") => "image/jpeg",
            _ => "application/octet-stream",
        }
        .to_string();
        Ok(Self {
            path,
            name,
            mime,
            size: meta.len(),
            handle: std::sync::Mutex::new(None),
        })
    }
}

impl crate::FileSource for PathFileSource {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn mime(&self) -> String {
        self.mime.clone()
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: u32) -> Vec<u8> {
        use std::io::{Read, Seek, SeekFrom};
        let mut guard = match self.handle.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let needs_open = match guard.as_ref() {
            Some((_, position)) => offset < *position,
            None => true,
        };
        if needs_open {
            let Ok(file) = std::fs::File::open(&self.path) else {
                return Vec::new();
            };
            *guard = Some((file, 0));
        }
        let Some((file, position)) = guard.as_mut() else {
            return Vec::new();
        };
        if *position != offset && file.seek(SeekFrom::Start(offset)).is_err() {
            return Vec::new();
        }
        let mut buf = vec![0u8; len as usize];
        let mut filled = 0;
        while filled < buf.len() {
            match file.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => break,
            }
        }
        *position = offset + filled as u64;
        buf.truncate(filled);
        buf
    }
}

/// One file, landed on the server. This is what the app gets back from an
/// upload and what the transcript records, so it is a flat record rather than
/// anything borrowed from the upload machinery.
#[derive(uniffi::Record, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UploadedFile {
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    pub path: String,
    pub sha256: String,
}

/// Progress/outcome of one upload, as the UI needs it: bytes moved, and enough
/// to name the file in a chip.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum UploadEvent {
    /// `sent` of `total`; `total` is 0 when the client did not know the size.
    Progress { id: String, name: String, sent: u64, total: u64 },
    Done { id: String, file: UploadedFile },
    Failed { id: String, name: String, message: String },
}

/// A file the SERVER handed us, described but not carried.
///
/// The transcript stores this and nothing else (`Item.files`): the bytes are
/// fetched on demand (`Core::fetch_file`). Putting them in the item instead is
/// what makes a shared PDF cost 4/3 of its size in SQLite, again in every
/// `session/load` replay, again in each client's transcript cache — and, because
/// a tool result is shown to the model, tens of thousands of tokens of context on
/// every turn. Measured 2026-11-03, which is why this type exists.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SharedFile {
    /// The name to show: the sharer's own, never a sanitised or hashed form.
    pub name: String,
    pub mime: String,
    /// 0 until the share tool's result reports it (the REQUEST carries no size).
    pub bytes: u64,
    /// The server-side path. Both the item's identity and the fetch key.
    pub path: String,
    /// Empty until the result reports it; then the client cache key.
    pub sha256: String,
}

/// Progress and outcome of one server -> client file fetch, as the UI needs it.
///
/// Keyed by the SERVER-side `path` (the same value the `ItemKind::File` row
/// carries) rather than by request order: several rows can be fetching at once,
/// and a chat can be reopened mid-fetch.
#[derive(
    uniffi::Enum, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
pub enum FileEvent {
    /// `received` of `total` bytes; `total` is 0 when the server did not say.
    Progress {
        path: String,
        name: String,
        received: u64,
        total: u64,
    },
    /// Finished and landed in the client's cache.
    Saved {
        path: String,
        name: String,
        mime: String,
        bytes: u64,
        local_path: String,
    },
    Failed {
        path: String,
        name: String,
        message: String,
    },
}

/// One `attachment_read` window, decoded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReadChunk {
    pub offset: u64,
    pub total: u64,
    pub data: Vec<u8>,
}

/// A shared file, recovered from the tool CALL rather than its result.
///
/// The call is the only trustworthy source: `raw_input` is structured and the
/// model never reformats it, while the result is a text payload the model may
/// paraphrase in what it says next. Recognising the tool name is likewise exact —
/// `_meta.goose.toolCall.toolName` carries `<extension>__<tool>` verbatim (the
/// extension itself is normalised, so the suffix is what we match).
pub(crate) fn share_from_call(
    tool_name: &str,
    raw_input: &Value,
) -> Option<SharedFile> {
    let tool = tool_name.trim();
    if tool.is_empty() || !tool.ends_with(SHARE_TOOL) {
        return None;
    }
    // Guard the separator: a tool literally named `xxxattachment_share` is not ours.
    let prefixed = tool.len() > SHARE_TOOL.len()
        && tool[..tool.len() - SHARE_TOOL.len()].ends_with("__");
    if !prefixed {
        return None;
    }
    let path = raw_input.get("path").and_then(Value::as_str)?.trim();
    if path.is_empty() {
        return None;
    }
    let name = raw_input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| file_name(path));
    let mime = raw_input
        .get("mime")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| mime_for_name(&name));
    Some(SharedFile {
        name,
        mime,
        bytes: 0,
        path: path.to_string(),
        sha256: String::new(),
    })
}

/// The last path segment, which is what a card shows when no name was given.
pub(crate) fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .trim()
        .to_string()
}

/// Fill the facts only the RESULT knows (size, digest, canonical mime).
///
/// A result that is not the expected envelope changes nothing: a failed share
/// still leaves a card naming the file, and the fetch reports its own error.
pub(crate) fn apply_share_result(file: &mut SharedFile, payload: &Value) {
    if let Some(bytes) = payload.get("bytes").and_then(Value::as_u64) {
        if bytes > 0 {
            file.bytes = bytes;
        }
    }
    if let Some(sha) = payload.get("sha256").and_then(Value::as_str) {
        if !sha.trim().is_empty() {
            file.sha256 = sha.trim().to_string();
        }
    }
    // The NAME: a share call often carries only `path`, and a stored file is
    // `<sha16>-<name>` — the hash is not a filename. So the result wins whenever
    // the call's name was merely the path's last segment, and never when the
    // sharer named it explicitly.
    if let Some(name) = payload
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        if file.name.is_empty() || file.name == file_name(&file.path) {
            file.name = name.to_string();
        }
    }
    if let Some(mime) = payload
        .get("mime")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        if file.mime.is_empty() || file.mime == "application/octet-stream" {
            file.mime = mime.to_string();
        }
    }
}

/// A safe on-disk name for a downloaded file.
///
/// The client writes what the server named, minus anything a path cannot hold.
/// A name of only separators or dots is not a name, so it becomes `file` rather
/// than a traversal or a hidden file.
pub(crate) fn safe_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' ' | '(' | ')') {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(['.', '-', ' ']).to_string();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.chars().take(120).collect()
    }
}

/// `_goose/unstable/tools/call` params for one window of a shared file.
pub(crate) fn read_params(wire: &str, path: &str, offset: u64, length: usize) -> Value {
    tool_params(
        wire,
        READ_TOOL,
        json!({ "path": path, "offset": offset, "length": length }),
    )
}

/// Decode an `attachment_read` reply into a window.
///
/// Strict about `offset`: the caller assembles the file by appending, so a chunk
/// that arrives out of place must fail loudly rather than produce a file that
/// looks whole and is not.
pub(crate) fn parse_read(payload: &Value) -> Result<ReadChunk, String> {
    let offset = payload
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| "the server returned no offset".to_string())?;
    let total = payload
        .get("total")
        .and_then(Value::as_u64)
        .ok_or_else(|| "the server returned no total".to_string())?;
    let data = payload
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "the server returned no data".to_string())?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("chunk is not valid base64: {e}"))?;
    Ok(ReadChunk {
        offset,
        total,
        data: bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(args: serde_json::Value) -> Option<SharedFile> {
        share_from_call("attachments__attachment_share", &args)
    }

    #[test]
    fn a_share_call_is_recognised_from_its_arguments() {
        let file = share(serde_json::json!({"path": "/workspace/attachments/q3.pdf"})).unwrap();
        // No explicit name: the path's last segment, and a mime derived from it.
        assert_eq!(file.name, "q3.pdf");
        assert_eq!(file.mime, "application/pdf");
        assert_eq!(file.path, "/workspace/attachments/q3.pdf");
        // The request carries neither size nor digest; the result fills them.
        assert_eq!(file.bytes, 0);
        assert!(file.sha256.is_empty());
    }

    #[test]
    fn an_explicit_name_and_mime_win_over_the_path() {
        let file = share(serde_json::json!({
            "path": "/workspace/attachments/aa11bb22cc33dd44-report-final.pdf",
            "name": "Q3 report (final).pdf",
            "mime": "application/pdf",
        }))
        .unwrap();
        assert_eq!(file.name, "Q3 report (final).pdf");
    }

    #[test]
    fn only_the_attachments_share_tool_counts_as_a_share() {
        let args = serde_json::json!({"path": "/workspace/attachments/x.pdf"});
        assert!(share_from_call("attachments__attachment_read", &args).is_none());
        assert!(share_from_call("probefiles__attachment_share", &args).is_some());
        // A tool whose name merely ENDS with the string, with no `__`, is not ours.
        assert!(share_from_call("myattachment_share", &args).is_none());
        assert!(share_from_call("", &args).is_none());
    }

    #[test]
    fn a_share_without_a_path_is_not_a_share() {
        assert!(share(serde_json::json!({})).is_none());
        assert!(share(serde_json::json!({"path": "   "})).is_none());
    }

    #[test]
    fn the_result_fills_the_size_and_digest() {
        let mut file = share(serde_json::json!({"path": "/workspace/attachments/q3.pdf"})).unwrap();
        apply_share_result(
            &mut file,
            &serde_json::json!({"bytes": 193, "sha256": "abc123", "mime": "application/pdf"}),
        );
        assert_eq!(file.bytes, 193);
        assert_eq!(file.sha256, "abc123");
    }

    #[test]
    fn the_result_renames_a_path_derived_name_but_never_an_explicit_one() {
        // Path-derived (`<sha16>-<name>`): the server's canonical name wins.
        let mut derived = share(serde_json::json!({
            "path": "/workspace/attachments/aa11bb22cc33dd44-report.pdf"}))
        .unwrap();
        apply_share_result(&mut derived, &serde_json::json!({"name": "report.pdf"}));
        assert_eq!(derived.name, "report.pdf");

        // Explicit: the sharer's own words stand, whatever the server says.
        let mut explicit = share(serde_json::json!({
            "path": "/workspace/attachments/aa11bb22cc33dd44-report.pdf",
            "name": "Q3 final.pdf"}))
        .unwrap();
        apply_share_result(&mut explicit, &serde_json::json!({"name": "report.pdf"}));
        assert_eq!(explicit.name, "Q3 final.pdf");
    }

    #[test]
    fn a_bad_share_result_changes_nothing() {
        let mut file = share(serde_json::json!({"path": "/workspace/attachments/q3.pdf"})).unwrap();
        let before = file.clone();
        apply_share_result(&mut file, &serde_json::json!({"unexpected": true}));
        assert_eq!(file, before);
    }

    #[test]
    fn a_downloaded_name_is_safe_to_write() {
        assert_eq!(safe_file_name("Q3 report (final).pdf"), "Q3 report (final).pdf");
        assert_eq!(safe_file_name("../../etc/passwd"), "etc-passwd");
        assert_eq!(safe_file_name(".."), "file");
        assert_eq!(safe_file_name(""), "file");
        // Bounded: a 400-character server name cannot become a 400-character path.
        assert!(safe_file_name(&"a".repeat(400)).len() <= 120);
    }

    #[test]
    fn a_read_reply_decodes_to_its_window() {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(b"hello");
        let chunk = parse_read(&serde_json::json!({
            "offset": 5, "total": 10, "data": data})).unwrap();
        assert_eq!(chunk.offset, 5);
        assert_eq!(chunk.total, 10);
        assert_eq!(chunk.data, b"hello");
    }

    #[test]
    fn a_read_reply_missing_its_offset_is_refused() {
        // Assembling by append, a chunk with no position could silently reorder
        // the file. Refuse rather than guess.
        assert!(parse_read(&serde_json::json!({"total": 3, "data": "aGk="})).is_err());
        assert!(parse_read(&serde_json::json!({"offset": 0, "total": 3})).is_err());
        assert!(parse_read(&serde_json::json!({"offset": 0, "total": 3, "data": "!!"})).is_err());
    }

    #[test]
    fn params_carry_the_extension_and_the_prefixed_tool_name() {
        let params = begin_params("wire-1", "notes.txt", "text/plain", 42);
        assert_eq!(params["extensionName"], "Attachments");
        assert_eq!(params["name"], "attachments__attachment_begin");
        assert_eq!(params["sessionId"], "wire-1");
        assert_eq!(params["arguments"]["size"], 42);
    }

    #[test]
    fn chunk_params_carry_the_offset_verbatim() {
        // The server refuses a mismatch, so the offset is the one field a
        // mistake here cannot be absorbed by the other side.
        let params = chunk_params("w", "up-1", 8_388_608, "AAAA");
        assert_eq!(params["arguments"]["offset"], 8_388_608u64);
        assert_eq!(params["arguments"]["uploadId"], "up-1");
        assert_eq!(params["name"], "attachments__attachment_chunk");
    }

    #[test]
    fn chunk_plan_covers_empty_exact_and_remainder() {
        assert_eq!(chunk_plan(0, 8), Vec::<(u64, usize)>::new());
        assert_eq!(chunk_plan(8, 8), vec![(0, 8)]);
        assert_eq!(chunk_plan(16, 8), vec![(0, 8), (8, 8)]);
        assert_eq!(chunk_plan(20, 8), vec![(0, 8), (8, 8), (16, 4)]);
        assert_eq!(chunk_plan(1, 8), vec![(0, 1)]);
    }

    #[test]
    fn chunk_plan_tiles_the_file_exactly_once() {
        let total = 12_345_678u64;
        let plan = chunk_plan(total, CHUNK_BYTES);
        assert_eq!(plan.iter().map(|(_, len)| *len as u64).sum::<u64>(), total);
        let mut expected = 0;
        for (offset, len) in plan {
            assert_eq!(offset, expected);
            expected += len as u64;
        }
    }



    #[test]
    fn file_uri_escapes_what_would_break_parsing() {
        assert_eq!(file_uri("/workspace/a.pdf"), "file:///workspace/a.pdf");
        assert_eq!(file_uri("/a b/c#1.pdf"), "file:///a%20b/c%231.pdf");
        assert_eq!(file_uri("/a%20b"), "file:///a%2520b");
    }

    /// The exact bytes goose renders, taken from a live replay of a prompt that
    /// carried two files (see the phone session 20261102_39).
    const REPLAYED: &str = "\n\n--- Resource link (not inlined) ---\n\
        {\"name\":\"document-513\",\"uri\":\"file:///workspace/attachments/d155f4940515d168-document-513\"}\n---";

    #[test]
    fn a_replayed_stub_becomes_a_file_and_leaves_no_json() {
        let (text, files) = split_links(REPLAYED);
        assert_eq!(text, "", "the stub was the whole message");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "document-513");
        assert_eq!(files[0].path, "/workspace/attachments/d155f4940515d168-document-513");
        assert_eq!(files[0].bytes, 0, "a replay knows no size -- do not invent one");
        assert_eq!(files[0].sha256, "");
    }

    #[test]
    fn a_stub_after_real_text_keeps_the_text() {
        // What a prompt with a message AND a file re-reads as.
        let raw = format!("look at this{REPLAYED}");
        let (text, files) = split_links(&raw);
        assert_eq!(text, "look at this");
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn two_stubs_in_one_message_are_two_files() {
        let raw = format!("{REPLAYED}\n\n{REPLAYED}");
        let (text, files) = split_links(&raw);
        assert_eq!(text, "", "both stubs removed, leaving nothing but whitespace");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn ordinary_text_is_untouched() {
        // The marker is what makes this safe: no client can produce the string
        // accidentally, and a message that merely MENTIONS a link keeps its
        // words (a JSON line outside a stub is not a file).
        let prose = "here is a link: file:///workspace/attachments/x-report.pdf\nand more";
        assert_eq!(split_links(prose), (prose.to_string(), Vec::new()));
        let broken = "--- Resource link (not inlined) ---\nnot json\n---";
        assert_eq!(split_links(broken), (broken.to_string(), Vec::new()));
        // A stub whose URI is not a file: path is not something we uploaded.
        let remote = "--- Resource link (not inlined) ---\n\
            {\"name\":\"a.pdf\",\"uri\":\"https://example.com/a.pdf\"}\n---";
        assert_eq!(split_links(remote), (remote.to_string(), Vec::new()));
    }

    #[test]
    fn a_recovered_name_still_classifies() {
        // The card picks its icon from the MIME, and the stub carries none: the
        // suffix is all there is, and a nameless blob would show a generic icon.
        assert_eq!(mime_for_name("document-513.pdf"), "application/pdf");
        assert_eq!(mime_for_name("Q3 report (final).XLSX"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
        assert_eq!(mime_for_name("no-suffix"), "application/octet-stream");
    }

    #[test]
    fn link_block_uses_the_file_uri_and_the_real_name() {
        let file = UploadedFile {
            name: "report final.pdf".into(),
            mime: "application/pdf".into(),
            bytes: 10,
            path: "/workspace/attachments/abc-report.pdf".into(),
            sha256: "abc".into(),
        };
        match link_block(&file) {
            PromptBlock::Link { uri, name, mime_type } => {
                assert_eq!(uri, "file:///workspace/attachments/abc-report.pdf");
                assert_eq!(name, "report final.pdf");
                assert_eq!(mime_type, "application/pdf");
            }
            other => panic!("expected a link block, got {other:?}"),
        }
    }
}
