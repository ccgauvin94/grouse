// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.surfaceColorAtElevation
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Check
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.delay
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

// Fenced code blocks are rendered here, not by the richtext library: in 0.20.0 the
// Markdown composable has no code-block renderer slot — it calls its own
// `CodeBlock(text)` internally, and the only customization surface (CodeBlockStyle)
// styles the block as one text run, so per-token highlighting cannot be threaded
// through it. Markdownish splits fences out and hands them to HighlightedCodeBlock;
// everything else (images, tables, lists) still renders through the library.

/** Token kinds; small enough that int constants beat an enum's verbosity here. */
private const val TOK_PLAIN = 0
private const val TOK_KEYWORD = 1
private const val TOK_STRING = 2
private const val TOK_NUMBER = 3
private const val TOK_COMMENT = 4

/** Theme-mapped palette so the highlighter itself never touches MaterialTheme. */
private data class CodePalette(
    val keyword: Color,
    val string: Color,
    val number: Color,
    val comment: Color,
    val plain: Color
)

/** Language family: keywords plus whether `#` opens a line comment. The `#` flag is
 *  per-family because C-family `#` lines are preprocessor, not comments — colouring
 *  `#include` as a comment would hide the path it names. */
private data class Family(val keywords: Set<String>, val hashComments: Boolean)

private val BASH_KEYWORDS = setOf(
    "if", "then", "else", "elif", "fi", "for", "in", "while", "until", "do", "done",
    "case", "esac", "function", "return", "local", "export", "readonly", "source",
    "echo", "cd", "set", "trap", "exit", "break", "continue", "sudo", "true", "false"
)

private val SQL_KEYWORDS = setOf(
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete",
    "join", "left", "right", "inner", "outer", "on", "group", "by", "order", "limit",
    "offset", "create", "table", "index", "view", "drop", "alter", "add", "column",
    "primary", "key", "foreign", "references", "not", "null", "default", "distinct",
    "count", "sum", "and", "or", "as", "union", "all", "having", "begin", "commit",
    "rollback", "transaction", "with", "case", "when", "then", "else", "end"
)

private val LANG_FAMILIES: Map<String, Family> = mapOf(
    "kotlin" to Family(
        setOf("as", "break", "class", "continue", "do", "else", "false", "for", "fun",
            "if", "in", "interface", "is", "null", "object", "package", "return",
            "super", "this", "throw", "true", "try", "typealias", "val", "var",
            "when", "while", "by", "catch", "companion", "override"),
        hashComments = false),
    "java" to Family(
        setOf("abstract", "assert", "boolean", "break", "byte", "case", "catch", "char",
            "class", "const", "continue", "default", "do", "double", "else", "enum",
            "extends", "false", "final", "finally", "float", "for", "if", "implements",
            "import", "instanceof", "int", "interface", "long", "native", "new",
            "null", "package", "private", "protected", "public", "return", "short",
            "static", "super", "switch", "synchronized", "this", "throw", "throws",
            "transient", "true", "try", "void", "volatile", "while", "var", "record"),
        hashComments = false),
    "c" to Family(
        setOf("auto", "bool", "break", "case", "char", "const", "continue", "default",
            "do", "double", "else", "enum", "extern", "false", "float", "for", "goto",
            "if", "inline", "int", "long", "nullptr", "register", "restrict", "return",
            "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
            "true", "union", "unsigned", "void", "volatile", "while", "NULL"),
        hashComments = false),
    "cpp" to Family(
        setOf("auto", "bool", "break", "case", "catch", "char", "class", "const",
            "constexpr", "continue", "default", "delete", "do", "double", "else",
            "enum", "explicit", "extern", "false", "float", "for", "friend", "goto",
            "if", "inline", "int", "long", "mutable", "namespace", "new", "noexcept",
            "nullptr", "operator", "private", "protected", "public", "return",
            "short", "signed", "sizeof", "static", "struct", "switch", "template",
            "this", "throw", "true", "try", "typedef", "typename", "union",
            "unsigned", "using", "virtual", "void", "volatile", "while"),
        hashComments = false),
    "js" to Family(
        setOf("async", "await", "break", "case", "catch", "class", "const", "continue",
            "debugger", "default", "delete", "do", "else", "export", "extends",
            "false", "finally", "for", "function", "if", "import", "in",
            "instanceof", "let", "new", "null", "of", "return", "static", "super",
            "switch", "this", "throw", "true", "try", "typeof", "undefined", "var",
            "void", "while", "yield"), hashComments = false),
    "python" to Family(
        setOf("and", "as", "assert", "async", "await", "break", "class", "continue",
            "def", "del", "elif", "else", "except", "false", "finally", "for", "from",
            "global", "import", "in", "is", "lambda", "none", "nonlocal", "not", "or",
            "pass", "raise", "return", "true", "try", "while", "with", "yield",
            "self", "print"), hashComments = true),
    "rust" to Family(
        setOf("as", "async", "await", "break", "const", "continue", "crate", "dyn",
            "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
            "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
            "self", "static", "struct", "super", "trait", "true", "type",
            "unsafe", "use", "where", "while", "some", "ok", "err"),
        hashComments = false),
    "go" to Family(
        setOf("break", "case", "chan", "const", "continue", "default", "defer", "else",
            "fallthrough", "false", "for", "func", "go", "goto", "if", "import",
            "interface", "map", "nil", "package", "range", "return", "select",
            "struct", "switch", "true", "type", "var"), hashComments = false),
    "bash" to Family(BASH_KEYWORDS, hashComments = true),
    "sql" to Family(SQL_KEYWORDS, hashComments = true),
    "json" to Family(setOf("true", "false", "null"), hashComments = false),
    "yaml" to Family(emptySet(), hashComments = true),
    "ruby" to Family(
        setOf("def", "do", "end", "if", "elsif", "else", "unless", "while", "until",
            "for", "in", "begin", "class", "module", "return", "yield", "case",
            "when", "then", "and", "or", "not", "nil", "true", "false", "require",
            "attr", "attr_accessor", "lambda", "proc", "raise", "rescue", "retry",
            "ensure", "super", "self"), hashComments = true)
)

private val ALIAS_TO_LANG: Map<String, String> = mapOf(
    "kotlin" to "kotlin", "kt" to "kotlin", "kts" to "kotlin",
    "java" to "java", "scala" to "java", "groovy" to "java",
    "c" to "c", "h" to "c",
    "cpp" to "cpp", "c++" to "cpp", "cc" to "cpp", "cxx" to "cpp",
    "hpp" to "cpp", "hh" to "cpp",
    "js" to "js", "javascript" to "js", "ts" to "js", "typescript" to "js",
    "jsx" to "js", "tsx" to "js",
    "python" to "python", "py" to "python", "python3" to "python",
    "rust" to "rust", "rs" to "rust", "go" to "go", "golang" to "go",
    "bash" to "bash", "sh" to "bash", "shell" to "bash", "zsh" to "bash",
    "sql" to "sql", "postgres" to "sql", "postgresql" to "sql", "mysql" to "sql",
    "json" to "json",
    "yaml" to "yaml", "yml" to "yaml",
    "ruby" to "ruby", "rb" to "ruby"
)

private val FALLBACK_FAMILY = Family(emptySet(), hashComments = true)

private fun familyFor(tag: String): Family {
    val lang = ALIAS_TO_LANG[tag.trim().lowercase()]
    return if (lang == null) FALLBACK_FAMILY else LANG_FAMILIES[lang] ?: FALLBACK_FAMILY
}

private fun tokColor(tok: Int, p: CodePalette): Color =
    when (tok) {
        TOK_KEYWORD -> p.keyword
        TOK_STRING -> p.string
        TOK_NUMBER -> p.number
        TOK_COMMENT -> p.comment
        else -> p.plain
    }

private fun isWordChar(c: Char): Boolean = c.isLetterOrDigit() || c == '_'

// The highlighter. It appends the WHOLE code text first, then walks it line by line
// adding spans at ABSOLUTE offsets (`base` = where the current line starts inside
// the builder's text); nothing is appended per token, so the text content and the
// spans can never disagree. Block comments are the only state that carries across
// lines. Per-message cost is one pass, run only on finalized messages.
private fun highlight(code: String, family: Family, p: CodePalette): AnnotatedString {
    val b = AnnotatedString.Builder()
    b.append(code)
    var off = 0
    var blockComment = false
    for (line in code.split('\n')) {
        blockComment = tokenize(line, off, family, p, b, blockComment)
        off += line.length + 1 // +1 for the '\n' that split() removed
    }
    return b.toAnnotatedString()
}

// Character scanner over one line: each rule consumes a whole run, so the per-line
// cost is linear and the logic stays flat.
private fun tokenize(
    line: String,
    base: Int,
    family: Family,
    p: CodePalette,
    out: AnnotatedString.Builder,
    blockComment: Boolean
): Boolean {
    var i = 0
    var inBlockComment = blockComment
    val n = line.length
    while (i < n) {
        // Already inside a `/* ... */` from a previous line: swallow to the closer.
        if (inBlockComment) {
            val end = line.indexOf("*/", i)
            val stop = if (end == -1) n else end + 2
            out.addStyle(SpanStyle(color = tokColor(TOK_COMMENT, p)), base + i, base + stop)
            i = stop
            if (end != -1) inBlockComment = false
            continue
        }
        val c = line[i]
        // Strings first: an embedded `#` or `//` inside a string must not be read as
        // a comment. An unterminated string swallows the rest of the line; backslash
        // escapes are honoured except in backticks.
        if (c == '"' || c == '\'' || c == '`') {
            var j = i + 1
            var closed = false
            while (j < n) {
                if (line[j] == '\\' && c != '`') {
                    j += 2
                    continue
                }
                if (line[j] == c) {
                    closed = true
                    break
                }
                j++
            }
            val stop = if (closed) j + 1 else n
            out.addStyle(SpanStyle(color = tokColor(TOK_STRING, p)), base + i, base + stop)
            i = stop
            continue
        }
        // `#` line comment (bash/py/yaml/ruby/sql families only).
        if (c == '#' && family.hashComments) {
            out.addStyle(SpanStyle(color = tokColor(TOK_COMMENT, p)), base + i, base + n)
            break
        }
        if (line.startsWith("//", i)) {
            out.addStyle(SpanStyle(color = tokColor(TOK_COMMENT, p)), base + i, base + n)
            break
        }
        if (line.startsWith("/*", i)) {
            val end = line.indexOf("*/", i + 2)
            val stop = if (end == -1) n else end + 2
            out.addStyle(SpanStyle(color = tokColor(TOK_COMMENT, p)), base + i, base + stop)
            i = stop
            if (end == -1) inBlockComment = true
            continue
        }
        // A digit opens a number only when the previous char cannot extend an
        // identifier (so `x2` reads one plain word, but `2` in `2.5e10` is a number).
        if (c.isDigit() && (i == 0 || !isWordChar(line[i - 1]))) {
            var j = i
            while (j < n && (line[j].isLetterOrDigit() || line[j] == '.' || line[j] == '_')) j++
            out.addStyle(SpanStyle(color = tokColor(TOK_NUMBER, p)), base + i, base + j)
            i = j
            continue
        }
        if (isWordChar(c)) {
            var j = i
            while (j < n && isWordChar(line[j])) j++
            val word = line.substring(i, j).lowercase()
            if (word in family.keywords) {
                out.addStyle(
                    SpanStyle(
                        color = tokColor(TOK_KEYWORD, p),
                        fontWeight = FontWeight.SemiBold
                    ),
                    base + i, base + j
                )
            } else {
                out.addStyle(SpanStyle(color = tokColor(TOK_PLAIN, p)), base + i, base + j)
            }
            i = j
            continue
        }
        // Everything else — whitespace, operators, punctuation — one plain run,
        // ending at the next char a rule above would want to own.
        var k = i
        while (k < n) {
            val ch = line[k]
            if (ch == '#' && family.hashComments) break
            if (ch == '/' && k + 1 < n && (line[k + 1] == '/' || line[k + 1] == '*')) break
            if (ch.isDigit() && (k == 0 || !isWordChar(line[k - 1]))) break
            if (isWordChar(ch)) break
            if (ch == '"' || ch == '\'' || ch == '`') break
            k++
        }
        out.addStyle(SpanStyle(color = tokColor(TOK_PLAIN, p)), base + i, base + k)
        i = k
    }
    return inBlockComment
}

/** One chunk of a message: either markdown the library should render, or a code block. */
internal sealed class Segment {
    internal data class Text(val text: String) : Segment()
    internal data class Code(val code: String, val tag: String) : Segment()
}

private val FENCE_OPEN = Regex("^(\\x60{3,}|~{3,})(\\w[\\w+#.-]*)?\\s*$")

// Line-based fence splitter — not a full markdown parse on purpose: the library
// keeps rendering everything outside the fences (images/tables/lists), and a fence
// that actually lives inside a list item or table cell (rare in chat output) is
// treated as top-level. Markers are backtick/tilde runs; the closing fence is any
// run of the same marker char at least as long (a superset of CommonMark's rule).
internal fun splitFencedCode(text: String): List<Segment> {
    val out = ArrayList<Segment>()
    val textBuf = StringBuilder()
    val lines = text.split('\n')
    var i = 0
    while (i < lines.size) {
        val open = FENCE_OPEN.matchEntire(lines[i])
        if (open == null) {
            if (textBuf.isNotEmpty() || i > 0) textBuf.append('\n')
            textBuf.append(lines[i])
            i++
            continue
        }
        val codeBuf = StringBuilder()
        i++
        while (i < lines.size && !isCloseFence(lines[i], open)) {
            if (codeBuf.isNotEmpty()) codeBuf.append('\n')
            codeBuf.append(lines[i])
            i++
        }
        if (i < lines.size) i++ // consume the closing fence (or hit EOF mid-fence)
        if (textBuf.isNotEmpty()) {
            out.add(Segment.Text(textBuf.toString().trim('\n')))
            textBuf.setLength(0)
        }
        val tag = if (open.groupValues.size > 2) open.groupValues[2] else ""
        out.add(Segment.Code(codeBuf.toString(), tag))
    }
    if (textBuf.isNotEmpty()) out.add(Segment.Text(textBuf.toString().trim('\n')))
    return out
}

private fun isCloseFence(line: String, open: MatchResult): Boolean {
    val marker = open.groupValues[1]
    val head = line.trimStart()
    return head.length >= marker.length && head.startsWith(marker) &&
        head.all { it == marker[0] || it.isWhitespace() }
}

private const val MAX_BLOCK_HEIGHT = 400

/** The chat code block: monospace, horizontally flickable lines, vertically capped. */
@Composable
internal fun HighlightedCodeBlock(code: String, tag: String) {
    val palette = CodePalette(
        keyword = MaterialTheme.colorScheme.tertiary,
        string = MaterialTheme.colorScheme.secondary,
        number = MaterialTheme.colorScheme.tertiary,
        comment = MaterialTheme.colorScheme.onSurfaceVariant,
        plain = MaterialTheme.colorScheme.onSurface
    )
    val highlighted = remember(code, tag) {
        highlight(code, familyFor(tag), palette)
    }
    val bg = MaterialTheme.colorScheme.surfaceColorAtElevation(3.dp)
    val borderColor = MaterialTheme.colorScheme.outlineVariant
    // Horizontal: each line renders softWrap=false inside a scrolling row, so long
    // lines flick instead of wrapping. Vertical: a tall block scrolls inside its own
    // cap instead of stretching the whole transcript bubble.
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .background(bg, RoundedCornerShape(8.dp))
            .border(1.dp, borderColor, RoundedCornerShape(8.dp))
            .padding(vertical = 8.dp)
            .heightIn(max = MAX_BLOCK_HEIGHT.dp)
            .verticalScroll(rememberScrollState())
    ) {
        Row(Modifier.horizontalScroll(rememberScrollState())) {
            Column(Modifier.padding(horizontal = 12.dp)) {
                Text(
                    text = highlighted,
                    style = MaterialTheme.typography.bodyMedium.copy(
                        fontFamily = FontFamily.Monospace
                    ),
                    softWrap = false,
                    maxLines = Int.MAX_VALUE
                )
            }
        }
        CodeCopyButton(code, bg)
    }
}

/** Copy affordance pinned to the block's top-right. Shows "Copied" briefly on
 *  press so the tap is visibly acknowledged inside a scrolled block (a toast
 *  would be a second channel for one word). The icon sits ABOVE the content and
 *  must not scroll away with it, which is why it is a sibling in the outer Box,
 *  not part of the scrolling column. */
@Composable
private fun BoxScope.CodeCopyButton(code: String, bg: Color) {
    val clip = LocalClipboardManager.current
    val copied = remember { mutableStateOf(false) }
    // Reset the "Copied" check after a beat so the icon goes back to Copy.
    LaunchedEffect(copied.value) {
        if (copied.value) {
            delay(2000)
            copied.value = false
        }
    }
    TextButton(
        onClick = {
            clip.setText(AnnotatedString(code))
            copied.value = true
        },
        modifier = Modifier
            .align(Alignment.TopEnd)
            .padding(4.dp)
            .height(28.dp),
        contentPadding = PaddingValues(horizontal = 8.dp, vertical = 2.dp),
        colors = ButtonDefaults.textButtonColors(
            contentColor = MaterialTheme.colorScheme.onSurfaceVariant
        )
    ) {
        Icon(
            imageVector = if (copied.value) Icons.Filled.Check else Icons.Filled.ContentCopy,
            contentDescription = if (copied.value) "Copied" else "Copy",
            modifier = Modifier.size(16.dp)
        )
    }
}
