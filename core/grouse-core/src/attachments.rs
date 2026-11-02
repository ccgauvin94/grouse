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

#[cfg(test)]
mod tests {
    use super::*;

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
