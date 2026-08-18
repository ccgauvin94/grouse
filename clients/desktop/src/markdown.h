#pragma once

#include <QString>

/** Markdown for agent output, rendered via Qt's built-in CommonMark parser
 *  (QTextDocument::setMarkdown -> toHtml). This is KDE-native, adds no
 *  dependency, and properly HTML-escapes text content so untrusted model text
 *  can't inject markup. Prefer it over a hand-rolled parser — a fork carries
 *  every change through every rebase forever, and a hand-rolled renderer can't
 *  keep up with goose's output (tables, task lists, fenced languages, ...). */
QString markdownToHtml(const QString &md);
