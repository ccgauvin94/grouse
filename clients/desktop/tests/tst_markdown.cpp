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

QTEST_MAIN(TstMarkdown)
#include "tst_markdown.moc"
