#include "markdown.h"

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
