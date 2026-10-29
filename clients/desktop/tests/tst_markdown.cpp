// markdownToHtml(): CommonMark + GFM via QTextDocument, HTML-escaped input.
// The contract that matters: model text can't inject markup.

#include <QtTest>

#include "markdown.h"

class TstMarkdown : public QObject
{
    Q_OBJECT

private slots:
    void emptyInput();
    void plainTextRoundTrips();
    void htmlIsEscaped();
    void headings();
    void emphasis();
    void codeFence();
    void tables();
    void links();
    // markdownBlocks() — the fence splitter feeding the blocks render path
    void blocksPlainTextStaysOneHtmlBlock();
    void blocksExtractsFenceWithLang();
    void blocksUnclosedFenceRendersAsCode();
    void blocksTildeFence();
    void blocksInfoStringCount();
    // highlightCode() — escaping + token spans
    void highlightEscapesHtml();
    void highlightKeywordStringCommentNumber();
    void highlightUnknownLangIsSafe();
    void highlightNumberNeedsNonWordPrev();
    void highlightUnterminatedStringStaysString();
};

void TstMarkdown::emptyInput()
{
    const QString html = markdownToHtml(QString());
    QVERIFY(html.contains(QStringLiteral("<body")));
}

void TstMarkdown::plainTextRoundTrips()
{
    const QString html = markdownToHtml(QStringLiteral("hello world"));
    QVERIFY(html.contains(QStringLiteral("hello world")));
}

void TstMarkdown::htmlIsEscaped()
{
    // Raw HTML in model output must never survive into the rendered document.
    // (Qt's CommonMark importer drops unsupported tags wholesale.)
    const QString html = markdownToHtml(
        QStringLiteral("<script>alert(1)</script> <img src=x onerror=y>"));
    QVERIFY(!html.contains(QStringLiteral("<script")));
    QVERIFY(!html.contains(QStringLiteral("<img")));
    QVERIFY(!html.contains(QStringLiteral("alert(1)")));
    // Literal angle brackets in plain text come out escaped.
    const QString escaped = markdownToHtml(QStringLiteral("a < b > c"));
    QVERIFY(escaped.contains(QStringLiteral("&lt;")));
    QVERIFY(escaped.contains(QStringLiteral("&gt;")));
}

void TstMarkdown::headings()
{
    QVERIFY(markdownToHtml(QStringLiteral("# Top")).contains(QStringLiteral("<h1")));
    QVERIFY(markdownToHtml(QStringLiteral("## Sub")).contains(QStringLiteral("<h2")));
}

void TstMarkdown::emphasis()
{
    const QString html = markdownToHtml(QStringLiteral("**bold** and *italic*"));
    QVERIFY(!html.contains(QStringLiteral("**bold**")));
    QVERIFY(html.contains(QStringLiteral("bold")));
}

void TstMarkdown::codeFence()
{
    const QString html = markdownToHtml(QStringLiteral("```cpp\nint x = 1;\n```"));
    QVERIFY(html.contains(QStringLiteral("int x = 1;")));
    QVERIFY(html.contains(QStringLiteral("monospace"))
            || html.contains(QStringLiteral("font-family"))
            || html.contains(QStringLiteral("pre")));
}

void TstMarkdown::tables()
{
    // GFM tables are a documented Qt markdown feature.
    const QString html = markdownToHtml(
        QStringLiteral("| a | b |\n|---|---|\n| 1 | 2 |"));
    QVERIFY(html.contains(QStringLiteral("<table")));
    QVERIFY(html.contains(QStringLiteral("1")));
}

void TstMarkdown::links()
{
    const QString html = markdownToHtml(QStringLiteral("[site](https://example.com)"));
    QVERIFY(html.contains(QStringLiteral("href")));
}

// ---------------------------------------------------------------------------
// markdownBlocks()
// ---------------------------------------------------------------------------

static QList<QVariantMap> asBlocks(const QVariantList &in)
{
    QList<QVariantMap> out;
    for (const QVariant &v : in)
        out.append(v.toMap());
    return out;
}

void TstMarkdown::blocksPlainTextStaysOneHtmlBlock()
{
    // A message without fenced code takes the single-document path: no `code`
    // chunk at all, so the delegate stays on the flat RichText bubble.
    const auto blocks = asBlocks(markdownBlocks(QStringLiteral("just prose")));
    QVERIFY(blocks.isEmpty());
}
void TstMarkdown::blocksExtractsFenceWithLang()
{
    const auto blocks = asBlocks(markdownBlocks(
        QStringLiteral("Text before.\n```python\nx = 1\n```\nText after.\n")));
    QCOMPARE(blocks.size(), 3);
    QVERIFY(blocks[0]["type"].toString() == QLatin1String("html"));
    QVERIFY(blocks[0]["html"].toString().contains(QStringLiteral("Text before")));
    QVERIFY(blocks[1]["type"].toString() == QLatin1String("code"));
    QCOMPARE(blocks[1]["lang"].toString(), QStringLiteral("python"));
    QVERIFY(blocks[1]["code"].toString().contains(QStringLiteral("x = 1")));
    QVERIFY(blocks[2]["type"].toString() == QLatin1String("html"));
    QVERIFY(blocks[2]["html"].toString().contains(QStringLiteral("Text after")));
}

void TstMarkdown::blocksUnclosedFenceRendersAsCode()
{
    // A stream cut mid-fence still renders as code — never dangling prose.
    const auto blocks = asBlocks(markdownBlocks(
        QStringLiteral("intro\n```rust\nlet x = 1;")));
    QCOMPARE(blocks.size(), 2);
    QVERIFY(blocks[1]["type"].toString() == QLatin1String("code"));
    QCOMPARE(blocks[1]["lang"].toString(), QStringLiteral("rust"));
}

void TstMarkdown::blocksTildeFence()
{
    const auto blocks = asBlocks(markdownBlocks(
        QStringLiteral("~~~\nplain code\n~~~\n")));
    QCOMPARE(blocks.size(), 1);
    QVERIFY(blocks[0]["type"].toString() == QLatin1String("code"));
}

void TstMarkdown::blocksInfoStringCount()
{
    // Closer = run of the SAME marker char at least as long: ``` cannot
    // terminate a ```` block.
    const auto blocks = asBlocks(markdownBlocks(
        QStringLiteral("````json\n{}\n```\n```not-a-closer-yet\n````\npast\n")));
    QCOMPARE(blocks.size(), 2);
    QVERIFY(blocks[0]["type"].toString() == QLatin1String("code"));
    QVERIFY(blocks[0]["code"].toString().contains(QStringLiteral("not-a-closer-yet")));
    QVERIFY(blocks[1]["type"].toString() == QLatin1String("html"));
    QVERIFY(blocks[1]["html"].toString().contains(QStringLiteral("past")));
}

void TstMarkdown::highlightEscapesHtml()
{
    // Same injection contract as markdownToHtml: highlight runs content
    // through escapeHtml, so <script> never survives as markup.
    const QString html = highlightCode(QStringLiteral("<script>x</script>"),
                                       QStringLiteral("cpp"),
                                       QStringLiteral("#111"), QStringLiteral("#222"),
                                       QStringLiteral("#333"), QStringLiteral("#444"));
    QVERIFY(!html.contains(QStringLiteral("<script>")));
    QVERIFY(html.contains(QStringLiteral("&lt;script&gt;")));
}

void TstMarkdown::highlightKeywordStringCommentNumber()
{
    const QString html = highlightCode(
        QStringLiteral("let x = \"str\" // note 42\n"),
        QStringLiteral("ts"),
        QStringLiteral("kw"), QStringLiteral("st"), QStringLiteral("cm"),
        QStringLiteral("nm"));
    QVERIFY(html.contains(QStringLiteral("<span style=\"color:kw\">let</span>")));
    QVERIFY(html.contains(QStringLiteral("<span style=\"color:st\">\"str\"</span>")));
    QVERIFY(html.contains(QStringLiteral("<span style=\"color:cm\">// note 42</span>")));
}

void TstMarkdown::highlightUnknownLangIsSafe()
{
    // An unknown tag falls back to strings + numbers only, and the
    // highlighter must never crash or emit raw markup.
    const QString html = highlightCode(QStringLiteral("fn = 1 \"s\""), QStringLiteral(""),
                                       QString(), QStringLiteral("st"), QString(), QString());
    QVERIFY(html.contains(QStringLiteral("fn"))); // plain text untouched
    QVERIFY(html.contains(QStringLiteral("<span style=\"color:st\">\"s\"</span>")));
}

void TstMarkdown::highlightNumberNeedsNonWordPrev()
{
    // x2 is an identifier tail; 3.5 (after a space) gets the number span.
    const QString html = highlightCode(QStringLiteral("x2 3.5\n"), QStringLiteral(""),
                                       QString(), QString(), QString(),
                                       QStringLiteral("num"));
    QVERIFY(!html.contains(QStringLiteral("<span style=\"color:num\">x2</span>")));
    QVERIFY(html.contains(QStringLiteral("<span style=\"color:num\">3.5</span>")));
}

void TstMarkdown::highlightUnterminatedStringStaysString()
{
    // A stray opener cannot paint the rest of the block: it stays string to
    // EOL, and the next line starts clean.
    const QString html = highlightCode(QStringLiteral("var a = \"oops\nlet b = 2\n"),
                                       QStringLiteral("ts"),
                                       QStringLiteral("kw"), QStringLiteral("st"),
                                       QString(), QString());
    QVERIFY(html.contains(QStringLiteral("<span style=\"color:st\">\"oops</span>")));
    QVERIFY(html.contains(QStringLiteral("<span style=\"color:kw\">let</span>")));
}

QTEST_MAIN(TstMarkdown)
#include "tst_markdown.moc"
