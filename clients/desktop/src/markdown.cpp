#include "markdown.h"

#include <QHash>
#include <QRegularExpression>
#include <QSet>
#include <QStringList>
#include <QTextDocument>

QString markdownToHtml(const QString &md)
{
    // Qt's Markdown importer implements CommonMark + a GFM subset (tables,
    // task lists, strikethrough, ...). It HTML-escapes all non-markup text, so
    // raw model output can't inject markup the way a hand-rolled renderer might.
    // We render into a throwaway document and ship the HTML to the QML delegate.
    QTextDocument doc;
    doc.setMarkdown(md);
    return doc.toHtml();
}

// ---------------------------------------------------------------------------
// Fenced code blocks + a small dependency-free highlighter. Deliberately NOT
// an editor-grade parser: one pass, no tree, no unicode states. The goal is
// marking up the obvious strings/comments/keywords/numbers so code reads at a
// glance, while never mis-escaping anything — every emitted chunk passes
// through escapeHtml, so a fence can't smuggle markup into a RichText view.
// ---------------------------------------------------------------------------

namespace {

QString escapeHtml(const QString &in)
{
    QString out;
    out.reserve(in.size() + 12);
    for (const QChar &ch : in) {
        const char16_t c = ch.unicode();
        if (c == u'&') out += QStringLiteral("&amp;");
        else if (c == u'<') out += QStringLiteral("&lt;");
        else if (c == u'>' ) out += QStringLiteral("&gt;");
        else out += ch;
    }
    return out;
}

/** Comment grammar for the languages agents emit. An unknown tag gets the
 *  safest default: strings and numbers only, because a wrong comment marker
 *  (say `#` in a CSS file) paints half a block as a comment. */
enum class Family { None, Hash, SlashStar, Sql };

Family familyFor(const QString &langLower)
{
    static const QHash<QString, Family> map = {
        {QStringLiteral("python"), Family::Hash},
        {QStringLiteral("py"), Family::Hash},
        {QStringLiteral("bash"), Family::Hash},
        {QStringLiteral("sh"), Family::Hash},
        {QStringLiteral("shell"), Family::Hash},
        {QStringLiteral("zsh"), Family::Hash},
        {QStringLiteral("fish"), Family::Hash},
        {QStringLiteral("yaml"), Family::Hash},
        {QStringLiteral("yml"), Family::Hash},
        {QStringLiteral("toml"), Family::Hash},
        {QStringLiteral("ini"), Family::Hash},
        {QStringLiteral("conf"), Family::Hash},
        {QStringLiteral("ruby"), Family::Hash},
        {QStringLiteral("rb"), Family::Hash},
        {QStringLiteral("perl"), Family::Hash},
        {QStringLiteral("r"), Family::Hash},
        {QStringLiteral("cmake"), Family::Hash},
        {QStringLiteral("dockerfile"), Family::Hash},
        {QStringLiteral("c"), Family::SlashStar},
        {QStringLiteral("cpp"), Family::SlashStar},
        {QStringLiteral("h"), Family::SlashStar},
        {QStringLiteral("hpp"), Family::SlashStar},
        {QStringLiteral("cxx"), Family::SlashStar},
        {QStringLiteral("java"), Family::SlashStar},
        {QStringLiteral("kt"), Family::SlashStar},
        {QStringLiteral("kotlin"), Family::SlashStar},
        {QStringLiteral("javascript"), Family::SlashStar},
        {QStringLiteral("js"), Family::SlashStar},
        {QStringLiteral("typescript"), Family::SlashStar},
        {QStringLiteral("ts"), Family::SlashStar},
        {QStringLiteral("jsonc"), Family::SlashStar},
        {QStringLiteral("go"), Family::SlashStar},
        {QStringLiteral("golang"), Family::SlashStar},
        {QStringLiteral("rust"), Family::SlashStar},
        {QStringLiteral("rs"), Family::SlashStar},
        {QStringLiteral("swift"), Family::SlashStar},
        {QStringLiteral("csharp"), Family::SlashStar},
        {QStringLiteral("cs"), Family::SlashStar},
        {QStringLiteral("php"), Family::SlashStar},
        {QStringLiteral("dart"), Family::SlashStar},
        {QStringLiteral("objc"), Family::SlashStar},
        {QStringLiteral("sql"), Family::Sql},
        {QStringLiteral("sqlite"), Family::Sql},
        {QStringLiteral("postgres"), Family::Sql},
        {QStringLiteral("mysql"), Family::Sql},
    };
    const auto it = map.constFind(langLower);
    return it == map.cend() ? Family::None : it.value();
}

QSet<QString> keywordSet(const char *words)
{
    QSet<QString> set;
    const QStringList split = QString::fromLatin1(words).split(u' ', Qt::SkipEmptyParts);
    for (const QString &w : split)
        set.insert(w);
    return set;
}

QSet<QString> keywordsFor(Family family)
{
    static const QSet<QString> hashLike = keywordSet(
        "and assert as async await break case class continue def def del dict do"
        " done echo elif else end except fi finally for from function global if"
        " import in is lambda let local mod module mut nil none nonlocal not or"
        " pass print pub puts raise require rescue return self set struct super"
        " then this true false try use var wait where while with yield");
    static const QSet<QString> slashLike = keywordSet(
        "abstract and as async await base bool break byte case catch char class"
        " const constexpr continue debugger decimal default delegate delete do"
        " double else enum event explicit export extern false finally fixed"
        " float for foreach function get goto if implicit import in inline int"
        " interface internal is let lock long match mod mut namespace new"
        " noexcept null object operator or out override package params partial"
        " private protected public range readonly ref return sealed set short"
        " signed sizeof static string struct super switch template this throw"
        " throws trait true try typedef typeof union unsigned unsafe ushort"
        " using val var virtual void volatile when where while yield");
    static const QSet<QString> sqlLike = keywordSet(
        "all alter and any as asc begin between by case column commit constraint"
        " create cross database default delete desc distinct drop else end"
        " except exists false for foreign from full grant group having if in"
        " index inner insert intersect into is join key left like limit not null"
        " offset on or order outer primary references replace right rollback"
        " select set table then to true union unique update using values view"
        " where with");
    switch (family) {
    case Family::Hash: return hashLike;
    case Family::SlashStar: return slashLike;
    case Family::Sql: return sqlLike;
    case Family::None: return {};
    }
    return {};
}

bool isWordChar(const QChar &c)
{
    return c.isLetterOrNumber() || c == u'_';
}

void appendEscaped(QString &out, const QString &raw)
{
    out += escapeHtml(raw);
}

void appendSpan(QString &out, const QString &raw, const QString &color)
{
    if (color.isEmpty() || raw.isEmpty()) {
        appendEscaped(out, raw);
        return;
    }
    out += QStringLiteral("<span style=\"color:%1\">").arg(color);
    out += escapeHtml(raw);
    out += QStringLiteral("</span>");
}

/** Drain `raw` into `out` as an escaped plain run, then clear it. Callers
 *  accumulate plain characters in `raw` so spans interrupt runs exactly once. */
void flushPlain(QString &out, QString &raw)
{
    if (!raw.isEmpty()) {
        appendEscaped(out, raw);
        raw.clear();
    }
}

void flushSpan(QString &out, const QString &raw, const QString &color)
{
    appendSpan(out, raw, color);
}

} // namespace

// The four colors are caller-supplied so the QML delegate picks them per theme
// (Theme.semantic.*.code in GrouseTheme.qml); QTextDocument honors no external
// CSS, so each token becomes an inline-styled span. Empty color = no markup
// for that role.
QString highlightCode(const QString &code, const QString &lang,
                      const QString &keywordColor, const QString &stringColor,
                      const QString &commentColor, const QString &numberColor)
{
    const Family family = familyFor(lang.toLower());
    const QSet<QString> keywords = keywordsFor(family);
    const bool hashComments = family == Family::Hash || family == Family::Sql;
    const bool slashComments = family == Family::SlashStar;
    const bool dashComments = family == Family::Sql;

    QString out = QStringLiteral("<pre>");
    bool inBlockComment = false;

    const QStringList lines = code.split(u'\n');
    for (int lineNo = 0; lineNo < lines.size(); ++lineNo) {
        const QString &line = lines.at(lineNo);
        QString plain;
        qsizetype i = 0;
        const qsizetype n = line.size();

        while (i < n) {
            // A block comment continues across lines until its closer.
            if (inBlockComment) {
                const qsizetype close = line.indexOf(QStringLiteral("*/"), i);
                if (close < 0) {
                    flushSpan(out, line.mid(i), commentColor);
                    i = n;
                } else {
                    flushSpan(out, line.mid(i, close + 2 - i), commentColor);
                    i = close + 2;
                    inBlockComment = false;
                }
                break;
            }

            const QChar c = line.at(i);
            const QChar next = i + 1 < n ? line.at(i + 1) : QChar();
            const QChar prev = i > 0 ? line.at(i - 1) : QChar();

            if (hashComments && c == u'#') {
                flushPlain(out, plain);
                flushSpan(out, line.mid(i), commentColor);
                break; // comment runs to end of line
            }
            if (dashComments && c == u'-' && next == u'-') {
                flushPlain(out, plain);
                flushSpan(out, line.mid(i), commentColor);
                break;
            }
            if (slashComments && c == u'/' && next == u'/') {
                flushPlain(out, plain);
                flushSpan(out, line.mid(i), commentColor);
                break;
            }
            if (slashComments && c == u'/' && next == u'*') {
                flushPlain(out, plain);
                const qsizetype close = line.indexOf(QStringLiteral("*/"), i + 2);
                if (close < 0) {
                    flushSpan(out, line.mid(i), commentColor);
                    inBlockComment = true;
                    break;
                }
                flushSpan(out, line.mid(i, close + 2 - i), commentColor);
                i = close + 2;
                continue;
            }

            if (c == u'\'' || c == u'"' || c == u'`') {
                // A string: scan to the matching quote honouring backslash
                // escapes; unterminated strings stay strings to end of line so
                // a stray opener can't paint the rest of the file colorless.
                flushPlain(out, plain);
                qsizetype end = i + 1;
                while (end < n) {
                    if (line.at(end) == u'\\') {
                        end += 2;
                        continue;
                    }
                    if (line.at(end) == c) {
                        end++;
                        break;
                    }
                    end++;
                }
                flushSpan(out, line.mid(i, qMin(end, n) - i), stringColor);
                i = qMin(end, n);
                continue;
            }

            if (c.isDigit() && !(i > 0 && isWordChar(prev))) {
                // A number starts after a non-word char (so `x2` stays a
                // plain identifier tail); consume digits, letters/underscore,
                // and dots so hex (`0xff`), suffixes (`100u`) and decimals fit.
                flushPlain(out, plain);
                qsizetype end = i + 1;
                while (end < n && (line.at(end).isLetterOrNumber()
                                   || line.at(end) == u'_'
                                   || line.at(end) == u'.'))
                    end++;
                flushSpan(out, line.mid(i, end - i), numberColor);
                i = end;
                continue;
            }

            if (isWordChar(c)) {
                // A word: flush what came before, then test the whole word
                // against the keyword set once.
                qsizetype end = i + 1;
                while (end < n && isWordChar(line.at(end)))
                    end++;
                const QString word = line.mid(i, end - i);
                if (keywords.contains(word)) {
                    flushPlain(out, plain);
                    flushSpan(out, word, keywordColor);
                } else {
                    plain += word;
                }
                i = end;
                continue;
            }

            plain += c;
            i++;
        }
        // Trailing unterminated comment-token state (already flushed above)
        // or leftover plain text for the line.
        flushPlain(out, plain);
        if (lineNo < lines.size() - 1)
            out += u'\n';
    }
    out += QStringLiteral("</pre>");
    return out;
}

// CommonMark-fence subset agents emit: a ``` or ~~~ opener with an optional
// info word, closed by a run of the same character at least as long, on a line
// of nothing else. Qt's own parser handles the trickier edge cases, which is
// why anything the splitter doesn't recognise for fencing stays in prose.
static const QRegularExpression fenceOpener(
    QStringLiteral("^( {0,3})(`{3,}|~{3,})[ \\t]*([^\\s`]*)"),
    QRegularExpression::MultilineOption);

// The fence state `markdownBlocks` accumulates between fence lines. It was referenced
// but never declared, so the desktop did not compile at all (`'Fence' was not declared
// in this scope`). Only `lang` and `code` are touched anywhere in the file, and
// `open = {}` needs it to stay an aggregate.
struct Fence
{
    QString lang;
    QString code;
};

QVariantList markdownBlocks(const QString &md)
{
    QVariantList blocks;

    Fence open;             // the currently open fence
    QString prose;          // the accumulation between fences
    QChar openChar = u'\0'; // '`' or '~' while one is open
    qsizetype openLen = 0;

    const auto flushProse = [&](bool final) {
        if (prose.isEmpty() && (final || openChar == u'\0'))
            return;
        if (!prose.isEmpty())
            blocks.append(QVariantMap{{"type", "html"},
                                      {"html", markdownToHtml(prose)}});
        prose.clear();
    };
    const auto flushCode = [&]() {
        blocks.append(QVariantMap{{"type", "code"},
                                  {"lang", open.lang},
                                  {"code", open.code}});
        open = {};
        openChar = u'\0';
    };

    const QStringList lines = md.split(u'\n');
    for (const QString &line : lines) {
        if (openChar != u'\0') {
            // Closing fence: optional leading spaces, then a run of the opener
            // character at least as long, then nothing but optional spaces.
            qsizetype i = 0;
            while (i < line.size() && line.at(i) == u' ')
                i++;
            if (i < line.size() && QChar(line.at(i)) == openChar) {
                qsizetype run = 0;
                while (i < line.size() && line.at(i) == openChar) {
                    run++;
                    i++;
                }
                if (run >= openLen) {
                    while (i < line.size() && line.at(i) == u' ')
                        i++;
                    if (i == line.size()) {
                        flushCode();
                        continue;
                    }
                }
            }
            open.code += line + u'\n';
            continue;
        }
        const auto match = fenceOpener.match(line);
        if (match.hasMatch()) {
            prose += u'\n'; // fence breaks the running prose paragraph
            flushProse(false);
            openChar = match.captured(2).at(0);
            openLen = match.captured(2).length();
            open.lang = match.captured(3);
            continue;
        }
        prose += line + u'\n';
    }
    flushProse(true);
    // An unclosed fence renders as code — never leave it dangling as prose.
    if (openChar != u'\0')
        flushCode();
    return blocks;
}
