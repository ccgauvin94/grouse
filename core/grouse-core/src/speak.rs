// SPDX-License-Identifier: AGPL-3.0-or-later

//! The spoken-reply stream (docs/TRANSCRIPT_MODEL.md): turns the assistant's
//! raw markdown into sanitized, sentence-sized chunks a client can hand to a
//! TTS engine. Pure text — NO network, NO uniffi exports.
//!
//! The conversation loop's SPEAKING state wants speech to start *before* the
//! reply finishes, so text is chunked at sentence boundaries as it streams,
//! not once at the end. It also wants *prose*: code fences, inline code, links
//! and markdown markers read badly aloud, so they are stripped here — once, in
//! the core — instead of re-implemented in every client.

/// Accumulates raw assistant markdown and yields speakable sentences.
///
/// Text is consumed up to the next *boundary* — a sentence terminator (`. ! ?`
/// followed by whitespace) or a newline — and each unit is sanitized into a
/// prose buffer that is flushed as a sentence at a terminator (or a blank
/// line). A terminator at the very end of the buffer is held back until more
/// text arrives, so `e.g.` mid-stream does not split. Fenced code (```/~~~) is
/// tracked line-wise and never spoken. A trailing partial unit is emitted by
/// [`SpeakStream::flush`].
#[derive(Debug, Default)]
pub struct SpeakStream {
    /// Raw text not yet consumed up to a boundary.
    raw: String,
    /// Sanitized prose not yet emitted (waiting for a sentence boundary).
    pending: String,
    /// Inside a ``` / ~~~ fenced code block: its contents are not spoken.
    in_code: bool,
}

impl SpeakStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw assistant text chunk; returns any complete sentences now
    /// ready to speak (empty when the chunk only added partial content).
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.raw.push_str(text);
        let mut out = Vec::new();
        while let Some((end, is_sentence)) = next_boundary(&self.raw) {
            let unit: String = self.raw.drain(..end).collect();
            self.consume_unit(&unit, is_sentence, &mut out);
        }
        out
    }

    /// Flush the remaining prose (turn end): the buffered sentence, then any
    /// trailing partial unit. Never speaks buffered code.
    pub fn flush(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.raw.is_empty() {
            let unit = std::mem::take(&mut self.raw);
            self.consume_unit(&unit, true, &mut out);
        }
        self.flush_pending(&mut out);
        out
    }

    /// Drop all buffered text (session change / transcript reset).
    pub fn reset(&mut self) {
        self.raw.clear();
        self.pending.clear();
        self.in_code = false;
    }

    /// Sanitize one boundary unit into `pending` (or toggle the fence state).
    fn consume_unit(&mut self, unit: &str, is_sentence: bool, out: &mut Vec<String>) {
        let t = unit.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            // A fence is a hard break: end the current utterance before the
            // code, and never speak the code itself.
            self.flush_pending(out);
            self.in_code = !self.in_code;
            return;
        }
        if self.in_code {
            return;
        }
        let prose = sanitize_line(t);
        if prose.is_empty() {
            // A blank line is a paragraph break.
            self.flush_pending(out);
            return;
        }
        if !self.pending.is_empty() && !self.pending.ends_with(char::is_whitespace) {
            self.pending.push(' ');
        }
        self.pending.push_str(&prose);
        if is_sentence {
            self.flush_pending(out);
        }
    }

    fn flush_pending(&mut self, out: &mut Vec<String>) {
        let s = self.pending.trim().to_string();
        self.pending.clear();
        if !s.is_empty() {
            out.push(s);
        }
    }
}

/// The end of the next boundary unit in `s`, and whether it is a sentence end.
///
/// Boundaries: a newline (line break, not a sentence end) or `.`/`!`/`?`
/// followed by whitespace (a sentence end; trailing closers are absorbed). A
/// terminator at the very end of the buffer returns `None` — more text may
/// follow. Abbreviations ("Dr.") are a known v1 limitation and split early.
fn next_boundary(s: &str) -> Option<(usize, bool)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            return Some((i + 1, false));
        }
        if c == b'.' || c == b'!' || c == b'?' {
            // An ordered-list marker ("1. ") is not a sentence end. `s` always
            // starts at a line start (we drain up to each newline), so the
            // marker is the whole prefix before the dot.
            let marker = c == b'.' && i > 0 && bytes[..i].iter().all(u8::is_ascii_digit);
            if !marker {
                let mut j = i + 1;
                while j < bytes.len() && matches!(bytes[j], b'"' | b'\'' | b')' | b']') {
                    j += 1;
                }
                if j >= bytes.len() {
                    return None; // terminator at the buffer end: wait for more
                }
                if bytes[j].is_ascii_whitespace() {
                    return Some((j, true));
                }
            }
        }
        i += 1;
    }
    None
}

/// Strip the markdown that reads badly aloud from one prose unit.
fn sanitize_line(line: &str) -> String {
    // Leading heading / blockquote markers, then a list marker.
    let s = line.trim_start_matches(['#', '>', ' ', '\t']);
    let s = strip_list_marker(s);
    let s = strip_links(s);
    let s = strip_inline_code(&s);
    // Emphasis / strike markers carry no spoken value.
    let s: String = s.chars().filter(|c| !matches!(c, '*' | '_' | '`' | '~')).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop a leading `- ` / `* ` / `+ ` / `1. ` list marker.
fn strip_list_marker(s: &str) -> &str {
    let t = s.trim_start();
    if let Some(rest) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    {
        return rest;
    }
    // Ordered: digits then ". ".
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && t[digits..].starts_with(". ") {
        return &t[digits + 2..];
    }
    t
}

/// `[text](url)` → `text`; `![alt](url)` → `alt`.
fn strip_links(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let is_image = c == '!' && chars.peek() == Some(&'[');
        if is_image {
            chars.next(); // consume '['
            out.push_str(&take_until(&mut chars, ']'));
            skip_paren(&mut chars);
        } else if c == '[' {
            out.push_str(&take_until(&mut chars, ']'));
            skip_paren(&mut chars);
        } else {
            out.push(c);
        }
    }
    out
}

/// Collect chars up to (not including) `end`, consuming it.
fn take_until(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, end: char) -> String {
    let mut out = String::new();
    for c in chars.by_ref() {
        if c == end {
            break;
        }
        out.push(c);
    }
    out
}

/// Consume a following `(url)` if present.
fn skip_paren(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    if chars.peek() != Some(&'(') {
        return;
    }
    chars.next();
    for c in chars.by_ref() {
        if c == ')' {
            break;
        }
    }
}

/// Remove backtick-delimited spans entirely (code is not spoken).
fn strip_inline_code(s: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for c in s.chars() {
        if c == '`' {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::SpeakStream;

    /// Everything a stream yields for `text`, including the final flush.
    fn speak_all(stream: &mut SpeakStream, text: &str) -> Vec<String> {
        let mut out = stream.push(text);
        out.extend(stream.flush());
        out
    }

    #[test]
    fn splits_into_sentences_and_flushes_the_remainder() {
        let mut s = SpeakStream::new();
        assert_eq!(
            speak_all(&mut s, "Hello there. How are you? I am fine"),
            vec!["Hello there.", "How are you?", "I am fine"]
        );
    }

    #[test]
    fn sentences_stream_before_the_reply_ends() {
        let mut s = SpeakStream::new();
        assert!(s.push("The answer is ").is_empty());
        // The first sentence is emitted as soon as its terminator lands — not
        // held until the turn ends.
        assert_eq!(s.push("42. And "), vec!["The answer is 42."]);
        assert!(s.push("more").is_empty());
        assert_eq!(s.flush(), vec!["And more"]);
    }

    #[test]
    fn a_terminator_at_the_buffer_end_waits_for_whitespace() {
        let mut s = SpeakStream::new();
        // "e.g." style: a lone trailing dot is not treated as final yet.
        assert!(s.push("Call me").is_empty());
        assert!(s.push(".").is_empty());
        assert_eq!(s.flush(), vec!["Call me."]);
    }

    #[test]
    fn fenced_code_is_never_spoken() {
        let mut s = SpeakStream::new();
        let out = speak_all(
            &mut s,
            "Run this:\n```\nnpm install left-pad\n```\nThen continue.",
        );
        assert_eq!(out, vec!["Run this:", "Then continue."]);
        assert!(!out.iter().any(|l| l.contains("left-pad")));
    }

    #[test]
    fn inline_code_is_removed() {
        let mut s = SpeakStream::new();
        assert_eq!(
            speak_all(&mut s, "Call the `foo()` helper now."),
            vec!["Call the helper now."]
        );
    }

    #[test]
    fn links_read_their_text_and_images_their_alt() {
        let mut s = SpeakStream::new();
        assert_eq!(
            speak_all(
                &mut s,
                "See [the docs](https://example.com/x) and ![a chart](c.png) for detail.",
            ),
            vec!["See the docs and a chart for detail."]
        );
    }

    #[test]
    fn headings_bullets_and_emphasis_are_stripped() {
        let mut s = SpeakStream::new();
        assert_eq!(
            speak_all(&mut s, "## Plan\n- **First** step\n1. Second step"),
            vec!["Plan First step Second step"]
        );
    }

    #[test]
    fn blank_lines_break_sentences() {
        let mut s = SpeakStream::new();
        assert_eq!(
            speak_all(&mut s, "First paragraph\n\nSecond paragraph"),
            vec!["First paragraph", "Second paragraph"]
        );
    }

    #[test]
    fn reset_drops_buffered_text() {
        let mut s = SpeakStream::new();
        assert!(s.push("half a sentence").is_empty());
        s.reset();
        assert!(s.flush().is_empty());
    }
}
