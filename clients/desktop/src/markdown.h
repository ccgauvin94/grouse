#pragma once

#include <QString>
#include <QVariantList>

/** Markdown for agent output, rendered via Qt's built-in CommonMark parser
 *  (QTextDocument::setMarkdown -> toHtml). This is KDE-native, adds no
 *  dependency, and properly HTML-escapes text content so untrusted model text
 *  can't inject markup. Prefer it over a hand-rolled parser — a fork carries
 *  every change through every rebase forever, and a hand-rolled renderer can't
 *  keep up with goose's output (tables, task lists, fenced languages, ...). */
QString markdownToHtml(const QString &md);

/**
 * Split a transcript message into render blocks: HTML prose chunks (from
 * markdownToHtml, exactly the old single-document output) interleaved with
 * fenced code blocks as raw {type, lang, code} maps. Callers render code
 * blocks in their own horizontal-scroller (RichText TextEdit wraps everything,
 * so a long line halves the bubble's usable width — and can't be scrolled).
 *
 * Fence parsing is the CommonMark subset agents actually emit: a ``` or ~~~
 * opener with an optional info string, closed by a fence of the same
 * character at least as long. Anything a Qt parse would treat differently
 * (4-space indented code) stays inside the prose HTML default.
 */
QVariantList markdownBlocks(const QString &md);

/**
 * Escape + syntax-highlight one code block to an HTML fragment for a RichText
 * surface. The four colors are caller-supplied so the QML delegate can pick
 * them per theme (see Theme.semantic.*.code in GrouseTheme.qml); they are
 * emitted as inline styles because QTextDocument honors no external CSS.
 * Returns e.g. `<pre>...<span style="color:#..">kw</span>...</pre>`.
 */
QString highlightCode(const QString &code, const QString &lang,
                      const QString &keywordColor, const QString &stringColor,
                      const QString &commentColor, const QString &numberColor);
