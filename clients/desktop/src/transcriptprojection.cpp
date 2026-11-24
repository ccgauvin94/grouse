// SPDX-License-Identifier: AGPL-3.0-or-later

#include "transcriptprojection.h"
#include "manager.h"
#include "coreapi.h"
#include "messagelistmodel.h"
#include "markdown.h"

#include <QCryptographicHash>

#include <cmath>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QMimeDatabase>
#include <QSaveFile>
#include <QStandardPaths>
#include <QTimer>
#include <QUrl>

TranscriptProjection::TranscriptProjection(Manager &manager)
    : m_manager(manager)
    , m_bridge(manager.m_bridge)
    , m_messageModel(manager.m_messageModel)
    , m_appHtml(manager.m_appHtml)
    , m_currentSessionId(manager.m_currentSessionId)
    , m_currentIndex(manager.m_currentIndex)
    , m_painting(manager.m_painting)
    , m_paintRows(manager.m_paintRows)
    , m_paintTimer(manager.m_paintTimer)
    , m_updateTimer(manager.m_updateTimer)
    , m_tailTimer(manager.m_tailTimer)
    , m_tailRendered(manager.m_tailRendered)
    , m_itemOldestId(manager.m_itemOldestId)
    , m_itemHasOlder(manager.m_itemHasOlder)
    , m_olderPending(manager.m_olderPending)
    , m_loadingOlder(manager.m_loadingOlder)
    , m_pendingAttachments(manager.m_pendingAttachments)
    , m_attachmentsByRow(manager.m_attachmentsByRow)
    , m_openClock(manager.m_openClock)
    , m_openFor(manager.m_openFor)
    , m_paintGap(manager.m_paintGap)
{
}

void TranscriptProjection::timingLog(const QString &line) { CoreApi::timingLog(line); }
void TranscriptProjection::refreshActivity() { m_manager.refreshActivity(); }
void TranscriptProjection::onError(const QString &text, bool background) { m_manager.onError(text, background); }
QString TranscriptProjection::scopedKey(const QString &sid, const QString &key) const { return Manager::scopedKey(sid, key); }
QString TranscriptProjection::sessionPeerLabel(const QString &sid) const { return m_manager.sessionPeerLabel(sid); }
QString TranscriptProjection::fileCardKeyFor(const QString &sid, const QString &path) const { return fileCardKey(sid, path); }

QString TranscriptProjection::fileCardKey(const QString &sessionId, const QString &path)
{
    return sessionId + QLatin1Char('|') + path;
}

/// The kind of a file, for the second line of an attachment card. MIME first
/// (authoritative), the name's suffix second — the stub recovered from a replay
/// carries a name only. Must agree with the Android client's mapping, which is
/// why it is a table rather than a per-call decision.
namespace {
QString attachmentKind(const QString &mime, const QString &name)
{
    const QString m = mime.toLower();
    const QString ext = name.section(QLatin1Char('.'), -1).toLower();
    if (m == QLatin1String("application/pdf") || ext == QLatin1String("pdf"))
        return QStringLiteral("PDF");
    if (m.contains(QLatin1String("wordprocessingml")) || m == QLatin1String("application/msword")
        || m.contains(QLatin1String("opendocument.text"))
        || QStringList{"doc", "docx", "odt", "rtf", "pages"}.contains(ext))
        return QStringLiteral("Document");
    if (m.contains(QLatin1String("spreadsheetml")) || m == QLatin1String("application/vnd.ms-excel")
        || m.contains(QLatin1String("opendocument.spreadsheet")) || m == QLatin1String("text/csv")
        || QStringList{"xls", "xlsx", "ods", "csv", "numbers"}.contains(ext))
        return QStringLiteral("Spreadsheet");
    if (m.contains(QLatin1String("presentationml")) || m == QLatin1String("application/vnd.ms-powerpoint")
        || m.contains(QLatin1String("opendocument.presentation"))
        || QStringList{"ppt", "pptx", "odp", "key"}.contains(ext))
        return QStringLiteral("Presentation");
    if (m.startsWith(QLatin1String("image/")))
        return QStringLiteral("Image");
    if (m.startsWith(QLatin1String("audio/")))
        return QStringLiteral("Audio");
    if (m.startsWith(QLatin1String("video/")))
        return QStringLiteral("Video");
    if (m.contains(QLatin1String("zip")) || m.contains(QLatin1String("gzip"))
        || m.contains(QLatin1String("tar"))
        || QStringList{"zip", "gz", "tar", "7z", "rar", "bz2", "xz"}.contains(ext))
        return QStringLiteral("Archive");
    if (m.contains(QLatin1String("json")) || m.contains(QLatin1String("javascript"))
        || m.contains(QLatin1String("xml")) || m.contains(QLatin1String("html"))
        || QStringList{"json", "js", "ts", "xml", "html", "css", "py", "rs", "cpp", "sh", "yaml", "yml"}.contains(ext))
        return QStringLiteral("Code");
    if (m.startsWith(QLatin1String("text/")) || QStringList{"txt", "md", "log"}.contains(ext))
        return QStringLiteral("Text");
    return QStringLiteral("File");
}

/// The Freedesktop icon for the same classification (Kirigami.Icon takes theme names).
QString attachmentIcon(const QString &mime, const QString &name)
{
    const QString kind = attachmentKind(mime, name);
    if (kind == QLatin1String("PDF"))
        return QStringLiteral("application-pdf");
    if (kind == QLatin1String("Document"))
        return QStringLiteral("x-office-document");
    if (kind == QLatin1String("Spreadsheet"))
        return QStringLiteral("x-office-spreadsheet");
    if (kind == QLatin1String("Presentation"))
        return QStringLiteral("x-office-presentation");
    if (kind == QLatin1String("Image"))
        return QStringLiteral("image-x-generic");
    if (kind == QLatin1String("Audio"))
        return QStringLiteral("audio-x-generic");
    if (kind == QLatin1String("Video"))
        return QStringLiteral("video-x-generic");
    if (kind == QLatin1String("Archive"))
        return QStringLiteral("application-x-archive");
    if (kind == QLatin1String("Code"))
        return QStringLiteral("text-x-script");
    if (kind == QLatin1String("Text"))
        return QStringLiteral("text-x-generic");
    return QStringLiteral("application-octet-stream");
}

/// "812 B" / "106 KB" / "25.3 MB", rounding the way the phone does: one decimal
/// only while it says something. `<= 0` is "unknown" — a replayed stub knows no
/// size, and a card that prints 0 B for a 26 MB deck is worse than one that
/// prints nothing.
QString humanBytes(qint64 bytes)
{
    if (bytes <= 0)
        return QString();
    if (bytes < 1024)
        return QStringLiteral("%1 B").arg(bytes);
    static const char *units[] = {"KB", "MB", "GB", "TB"};
    double value = bytes / 1024.0;
    int unit = 0;
    while (value >= 1024.0 && unit < 3) {
        value /= 1024.0;
        unit++;
    }
    const bool whole = qFuzzyCompare(value, std::floor(value));
    const QString number = (value >= 100.0 || whole) ? QString::number(value, 'f', 0)
                                                     : QString::number(value, 'f', 1);
    return QStringLiteral("%1 %2").arg(number, QString::fromLatin1(units[unit]));
}

/// One file as the card needs it: name, kind, icon and size label.
QVariantMap fileCard(const QString &name, const QString &mime, qint64 bytes)
{
    const QString kind = attachmentKind(mime, name);
    const QString size = humanBytes(bytes);
    return QVariantMap{{"name", name},
                       {"kind", kind},
                       {"icon", attachmentIcon(mime, name)},
                       {"size", size},
                       // "PDF · 106 KB", or the kind alone when the size is unknown
                       {"subtitle", size.isEmpty() ? kind : QStringLiteral("%1 · %2").arg(kind, size)}};
}

} // namespace

namespace {
QJsonObject parseObj(const QString &json)
{
    return QJsonDocument::fromJson(json.toUtf8()).object();
}

/// Whether a paint would change what is already on screen.
///
/// The core ends a MERGE with a repaint on purpose — a client that appended the replayed
/// tail keeps a stale suffix the anchor dropped store-side, so one Reset is how it
/// reconciles. But when the replay re-supplies exactly the rows the client already has
/// (the usual case for a chat that has not moved since it was last opened), the Reset
/// rebuilds every delegate and re-pins the view to show the SAME transcript: the "it
/// redraws while loading even when nothing changed" report (2026-11-21).
///
/// Compared on the fields that decide a row's identity and size, not the whole
/// QVariantMap: `seq` is re-stamped by a wholesale paint and carries no visible meaning.
bool samePaintedRows(const QList<QVariantMap> &a, const QList<QVariantMap> &b)
{
    if (a.size() != b.size())
        return false;
    for (int i = 0; i < a.size(); ++i) {
        const QVariantMap &x = a.at(i);
        const QVariantMap &y = b.at(i);
        if (x.value("id") != y.value("id")
            || x.value("text") != y.value("text")
            || x.value("output") != y.value("output")
            || x.value("status") != y.value("status")
            || x.value("title") != y.value("title")
            || x.value("calls") != y.value("calls"))
            return false;
    }
    return true;
}
} // namespace

namespace {

/// A prompt's image reaches the row as a `data:` URI: the core hands the client the
/// PIXELS, not a path, so the row can show what was sent without the file still being
/// there. QML's Image does accept those, but a 2-4 MB base64 string is parsed on every
/// repaint and — measured on the first desktop build that carried user images — painted
/// nothing at all. Every other picture this client draws comes from a FILE (an agent's
/// shared image is downloaded first), so decode once here and hand QML a file URL: the
/// user's own image then takes exactly the path that is known to work.
QString materialiseChatImage(const QString &uri)
{
    if (!uri.startsWith(QLatin1String("data:")))
        return uri;
    const int comma = uri.indexOf(QLatin1Char(','));
    if (comma < 0)
        return uri;
    const QString header = uri.mid(5, comma - 5);   // "image/png;base64"
    if (!header.endsWith(QLatin1String(";base64")))
        return uri;
    const QString mime = header.left(header.size() - 7);
    const QByteArray bytes = QByteArray::fromBase64(uri.mid(comma + 1).toLatin1());
    if (bytes.isEmpty())
        return uri;
    // Content-addressed, so the same picture sent twice (or replayed) is written once.
    const QString digest = QString::fromLatin1(
        QCryptographicHash::hash(bytes, QCryptographicHash::Sha1).toHex().left(16));
    QString suffix = QMimeDatabase().mimeTypeForName(mime).preferredSuffix();
    if (suffix.isEmpty())
        suffix = QStringLiteral("img");
    QDir dir(QStandardPaths::writableLocation(QStandardPaths::CacheLocation)
             + QStringLiteral("/chat-images"));
    if (!dir.exists() && !dir.mkpath(QStringLiteral(".")))
        return uri;
    const QString path = dir.filePath(digest + QLatin1Char('.') + suffix);
    if (!QFileInfo::exists(path)) {
        QSaveFile out(path);
        if (!out.open(QIODevice::WriteOnly) || out.write(bytes) != bytes.size() || !out.commit())
            return uri;   // fall back to the URI: a broken cache must not hide the picture
    }
    return QUrl::fromLocalFile(path).toString();
}

// True for mime types that are really text even though they live under
// application/* (code, config, data). Everything else that isn't an image is
// sent as a binary blob.
bool isTextishMime(const QString &mime)
{
    if (mime.startsWith(QLatin1String("text/")))
        return true;
    static const QStringList textish = {
        QStringLiteral("application/json"), QStringLiteral("application/xml"),
        QStringLiteral("application/x-yaml"), QStringLiteral("application/yaml"),
        QStringLiteral("application/toml"), QStringLiteral("application/javascript"),
        QStringLiteral("application/x-javascript"), QStringLiteral("application/x-shellscript"),
        QStringLiteral("application/x-sh"), QStringLiteral("application/x-python"),
        QStringLiteral("application/x-tex"), QStringLiteral("application/sql"),
        QStringLiteral("application/x-diff"), QStringLiteral("application/graphql"),
    };
    return textish.contains(mime);
}
} // namespace

namespace {
/// Fill a text row's `html` from its `text`. The core finalizes each streamed
/// row with an authoritative Upsert, so this runs once per completed message —
/// not per chunk (the deltas deliberately leave `html` empty).
void ensureRowHtml(QVariantMap &row)
{
    const QString role = row.value("role").toString();
    if (role != QLatin1String("user") && role != QLatin1String("agent")
        && role != QLatin1String("error"))
        return;
    if (!row.value("html").toString().isEmpty())
        return;
    const QString text = row.value("text").toString();
    if (role == QLatin1String("error"))
        row["html"] = QStringLiteral("<div>") + text + QStringLiteral("</div>");
    else {
        // Fenced code becomes blocks: the bubble delegate renders each code
        // chunk in its own horizontal scroller (RichText wraps everything, so
        // a long line squeezed the whole bubble into a third of its width).
        // Plain messages keep the single-document html path unchanged.
        const QVariantList blocks = markdownBlocks(text);
        bool hasCode = false;
        for (const QVariant &b : blocks)
            if (b.toMap().value("type").toString() == QLatin1String("code"))
                hasCode = true;
        if (hasCode)
            row["blocks"] = blocks;
        else
            row["html"] = markdownToHtml(text);
    }
}
} // namespace

void TranscriptProjection::flushPaint()
{
    if (!m_painting)
        return;
    m_painting = false;
    m_paintTimer->stop();
    // A paint that matches what the model already holds is not a paint. Skipping it keeps
    // the delegates, the content height and the scroll position exactly where they are.
    if (!m_paintRows.isEmpty() && samePaintedRows(m_messageModel->rows(), m_paintRows)) {
        timingLog(QStringLiteral("paint skipped: %1 rows, the window already matches")
                      .arg(m_paintRows.size()));
        m_paintRows.clear();
        m_currentIndex = -1;
        return;
    }
    {
        GROUSE_PHASE("phase replaceAll");
        m_messageModel->replaceAll(m_paintRows);
    }
    m_paintRows.clear();
    m_currentIndex = -1;
    requestMessagesUpdate();
}

void TranscriptProjection::requestMessagesUpdate()
{
    if (!m_updateTimer->isActive())
        m_updateTimer->start();
}

QVariantList TranscriptProjection::attachmentsForItem(const QString &id, const QString &text, int count)
{
    if (count <= 0)
        return {};
    if (m_attachmentsByRow.contains(id))
        return m_attachmentsByRow.value(id);
    // The queue is scoped to the chat that SENT the prompt: two chats can type the
    // identical line, and a path/text-only key let one chat consume the other's
    // thumbnails. The row is being painted for the chat on screen.
    auto it = m_pendingAttachments.find(scopedKey(m_currentSessionId, text));
    if (it == m_pendingAttachments.end() || it->isEmpty())
        return {};
    const QVariantList images = it->takeFirst();
    if (it->isEmpty())
        m_pendingAttachments.erase(it);
    m_attachmentsByRow.insert(id, images);
    if (m_attachmentsByRow.size() > 512)
        m_attachmentsByRow.erase(m_attachmentsByRow.begin());
    return images;
}

QVariantMap TranscriptProjection::rowFromItem(const QJsonObject &item)
{
    const QString kind = item.value(QStringLiteral("kind")).toString();
    const QString id = item.value(QStringLiteral("id")).toString();
    const QString text = item.value(QStringLiteral("text")).toString();
    const QString detail = item.value(QStringLiteral("detail")).toString();
    const QString output = item.value(QStringLiteral("output")).toString();
    const QString status = item.value(QStringLiteral("status")).toString();
    // The core's transcript position: the model inserts by it, not by arrival
    // (docs/TRANSCRIPT_MODEL.md). Every row carries its own.
    const int seq = item.value(QStringLiteral("seq")).toInt();
    // A delegate row carries what its SUBAGENT actually did — goose keeps child
    // sessions off ACP and forwards their tool calls onto this row instead.
    auto stepsOf = [](const QJsonArray &arr) {
        QVariantList out;
        for (const QJsonValue &sv : arr) {
            const QJsonObject s = sv.toObject();
            out << QVariantMap{{"name", s.value(QStringLiteral("name")).toString()},
                               {"detail", s.value(QStringLiteral("detail")).toString()}};
        }
        return out;
    };
    // A tool run carries the model's own framing of it (issue #13): the thinking
    // that preceded the run, absorbed by the core, and the one-line label drawn
    // from it. Both are empty for a run with nothing to draw on, and the chip
    // then falls back to the call titles. Each call also carries the thinking
    // that preceded IT, which is what puts the loop back in order when expanded.
    const QString summary = item.value(QStringLiteral("summary")).toString();
    const QString stepThought = item.value(QStringLiteral("thought")).toString();
    if (kind == QLatin1String("Tool")) {
        return QVariantMap{{"id", id}, {"role", "tool"}, {"text", ""}, {"html", ""},
                           {"title", text}, {"detail", detail}, {"output", output},
                           {"status", status}, {"toolCallId", id},
                           {"summary", summary}, {"stepThought", stepThought},
                           {"steps", stepsOf(item.value(QStringLiteral("steps")).toArray())},
                           {"seq", seq}};
    }
    if (kind == QLatin1String("ToolGroup")) {
        QVariantList calls;
        for (const QJsonValue &cv : item.value(QStringLiteral("calls")).toArray()) {
            const QJsonObject c = cv.toObject();
            calls << QVariantMap{{"title", c.value("title").toString()},
                                 {"detail", c.value("detail").toString()},
                                 {"output", c.value("output").toString()},
                                 {"status", c.value("status").toString()},
                                 {"thought", c.value("thought").toString()},
                                 {"toolCallId", c.value("id").toString()},
                                 {"steps", stepsOf(c.value("steps").toArray())}};
        }
        return QVariantMap{{"id", id}, {"role", "toolgroup"}, {"text", ""},
                           {"html", ""}, {"calls", calls}, {"status", status},
                           {"summary", summary}, {"stepThought", stepThought},
                           {"seq", seq}};
    }
    if (kind == QLatin1String("File")) {
        // A file the SERVER shared: a reference (name, mime, size, path, digest)
        // and never the bytes — the card fetches them on tap (`fetchFile`), so the
        // transcript, the cache and every replay stay small.
        QVariantList files;
        for (const QJsonValue &fv : item.value(QStringLiteral("files")).toArray()) {
            const QJsonObject f = fv.toObject();
            files << QVariantMap{
                {"name", f.value(QStringLiteral("name")).toString()},
                {"mime", f.value(QStringLiteral("mime")).toString()},
                {"kind", "file"},
                // qreal on purpose: a byte count must not be abbreviated by the
                // delegate, and a QVariantMap round-trip keeps it exact to 2^53.
                {"size", f.value(QStringLiteral("bytes")).toDouble()},
                {"path", f.value(QStringLiteral("path")).toString()},
                {"sha256", f.value(QStringLiteral("sha256")).toString()}};
        }
        return QVariantMap{{"id", id}, {"role", "file"}, {"text", text},
                           {"html", ""}, {"detail", detail}, {"files", files},
                           {"status", status}, {"seq", seq}};
    }
    if (kind == QLatin1String("Chart")) {
        return QVariantMap{{"id", id}, {"role", "chart"}, {"text", ""},
                           {"title", text}, {"chartData", detail},
                           {"toolCallId", id}, {"status", status}, {"seq", seq}};
    }
    if (kind == QLatin1String("McpApp")) {
        const QString appKey = item.value(QStringLiteral("app_key")).toString();
        // The app's html is fetched once and cached in memory; a re-delivered
        // item (replay, reopen) reuses it.
        if (!appKey.isEmpty() && !m_appHtml.contains(appKey) && m_bridge) {
            const int bar = appKey.indexOf(QLatin1Char('|'));
            if (bar > 0) {
                const QByteArray ext = appKey.left(bar).toUtf8();
                const QByteArray uri = appKey.mid(bar + 1).toUtf8();
                const QByteArray sid = m_currentSessionId.toUtf8();
                m_bridge->api().grouse_unstable_resources_read(m_bridge->handle(),
                                                               sid.constData(),
                                                               uri.constData(), ext.constData());
            }
        }
        return QVariantMap{{"id", id}, {"role", "mcpapp"}, {"text", ""},
                           {"title", text}, {"detail", detail}, {"appKey", appKey},
                           {"appHtml", m_appHtml.value(appKey)},
                           {"toolCallId", id}, {"status", status}, {"seq", seq}};
    }
    const QString role = kind == QLatin1String("User")     ? QStringLiteral("user")
                         : kind == QLatin1String("Thought") ? QStringLiteral("thought")
                         : kind == QLatin1String("Error")   ? QStringLiteral("error")
                                                            : QStringLiteral("agent");
    // The plan sentinel is a SIGNAL, not content: it tells the client a plan is ready to
    // approve, and showing it would read as debris in the reply.
    const QString shown = text;
    QVariantMap row{{"id", id}, {"role", role}, {"text", shown}, {"output", output},
                    {"html", QString()}, {"seq", seq}};
    if (role == QLatin1String("thought"))
        row["thought"] = true;
    // The core carries the pixels now (Item::images, data URIs): the row
    // itself is authoritative. The text-matching pixel queue
    // (attachmentsForItem) is only the fallback for an older core library —
    // gated on the count, because a later text-only prompt can repeat an
    // earlier attachment prompt's text and must not inherit its thumbnails.
    if (role == QLatin1String("user")) {
        // ONE consumption of the send-time queue per row (it is keyed by prompt
        // text and consumed in order, so asking twice would starve the next
        // row): used only for what the core did not record.
        const auto queued = attachmentsForItem(
            id, text, item.value(QStringLiteral("attachments")).toInt());

        const auto uris = item.value(QStringLiteral("images")).toArray();
        QVariantList images;
        if (!uris.isEmpty()) {
            for (const auto &uri : uris)
                images << QVariantMap{{"image", true},
                                      {"url", materialiseChatImage(uri.toString())}};
        } else {
            // The older-core fallback carries the same `data:` URIs, so it takes the
            // same decode: one shape reaches QML, whichever path produced it.
            for (const auto &v : queued) {
                const QVariantMap q = v.toMap();
                if (!q.value("image").toBool())
                    continue;
                images << QVariantMap{{"image", true},
                                      {"url", materialiseChatImage(q.value("url").toString())}};
            }
        }
        if (!images.isEmpty())
            row["images"] = images;

        // The files that rode this prompt, from the CORE's record (name, MIME,
        // bytes) — not from the send-time queue, which is per-session state a
        // reload does not have. The queue is the fallback for an older core
        // library and for a row that predates the record.
        QVariantList cards;
        const auto recorded = item.value(QStringLiteral("files")).toArray();
        for (const auto &f : recorded) {
            const QJsonObject o = f.toObject();
            cards << fileCard(o.value(QStringLiteral("name")).toString(),
                              o.value(QStringLiteral("mime")).toString(),
                              static_cast<qint64>(o.value(QStringLiteral("bytes")).toDouble()));
        }
        if (cards.isEmpty()) {
            for (const auto &v : queued) {
                const QVariantMap m = v.toMap();
                if (m.value("image").toBool() || !m.contains("mime"))
                    continue;
                cards << fileCard(m.value("name").toString(), m.value("mime").toString(),
                                  m.value("bytes").toLongLong());
            }
        }
        if (!cards.isEmpty())
            row["files"] = cards;
    }
    ensureRowHtml(row);
    return row;
}

void TranscriptProjection::applyItem(const QString &json)
{
    const QJsonObject root = parseObj(json);
    if (root.contains(QStringLiteral("Reset"))) {
        // NOT `m_messageModel->clear()`: a paint is Reset + rows + Window, and the
        // blank-then-rebuild was the jump. Buffer; flushPaint lands it in one reset.
        // A second Reset (the merge's end-of-replay repaint) simply supersedes the
        // buffer, which is right — the newer paint is the one that describes the
        // store.
        m_painting = true;
        m_paintRows.clear();
        m_paintTimer->start();
        m_olderPending.clear();
        m_loadingOlder = false;
        m_currentIndex = -1;
        m_tailTimer->stop();
        m_tailRendered.clear();
        if (!m_itemOldestId.isEmpty() || m_itemHasOlder) {
            m_itemOldestId.clear();
            m_itemHasOlder = false;
            emit m_manager.itemWindowChanged();
        }
        requestMessagesUpdate();
        refreshActivity();   // the transcript was replaced: the lanes with it
        return;
    }
    if (root.contains(QStringLiteral("Window"))) {
        // End of a paint: land it as one model reset (see flushPaint).
        flushPaint();
        const QJsonObject w = root.value(QStringLiteral("Window")).toObject();
        const QString oldest = w.value(QStringLiteral("oldest_id")).toString();
        const bool hasOlder = w.value(QStringLiteral("has_older")).toBool();
        if (m_loadingOlder) {
            // The core emits the older items oldest-first, then this Window: one
            // model insert for the whole batch keeps the viewport stable.
            const int n = m_olderPending.size();
            {
                GROUSE_PHASE("phase prependOlder");
                m_messageModel->prepend(m_olderPending);
            }
            m_olderPending.clear();
            m_loadingOlder = false;
            emit m_manager.olderRowsPrepended(n);
            requestMessagesUpdate();
        }
        if (oldest != m_itemOldestId || hasOlder != m_itemHasOlder) {
            m_itemOldestId = oldest;
            m_itemHasOlder = hasOlder;
            emit m_manager.itemWindowChanged();
        }
        // A paint that lands long after the previous one is the background fill, not
        // the open: the core keeps paging the older history in after the newest window
        // is on screen. Naming it takes one line of log and settles whether the residual
        // 2.5s stall belongs to the fill or to something else (2026-11-21).
        if (m_paintGap.isValid() && m_paintGap.elapsed() > 1000) {
            timingLog(QStringLiteral("late paint %1ms after the previous one (%2 rows, older=%3)")
                          .arg(m_paintGap.elapsed())
                          .arg(m_messageModel->rowCount())
                          .arg(hasOlder ? QStringLiteral("yes") : QStringLiteral("no")));
        }
        m_paintGap.restart();
        // A Window means "these rows were painted", NOT "the open succeeded": the
        // background history fill and load-older both send one. The positive answer
        // is coreOnSessionOpened; a rejected load is coreOnSessionOpenFailed. Using
        // the Window here also mistimed a roam open (no main-wire Ready ever came)
        // and could declare success for a paint that still had a turn in flight.
        // The first window after an open is the moment the chat is PAINTED. One line
        // per switch: the synchronous cost (cache parse + any core probe) against the
        // wait for a paint, which is the split that decides where a "hang" lives.
        if (!m_openFor.isEmpty() && m_openFor == m_currentSessionId) {
            timingLog(QStringLiteral("open %1 painted in %2ms (%3 rows)")
                                     .arg(m_openFor)
                                     .arg(m_openClock.elapsed())
                                     .arg(m_messageModel->rowCount()));
            m_openFor.clear();
        }
        refreshActivity();   // the chat is painted: its lanes are what the panel shows
        return;
    }
    if (root.contains(QStringLiteral("Upsert"))) {
        const QJsonObject item = root.value(QStringLiteral("Upsert")).toObject()
                                     .value(QStringLiteral("item")).toObject();
        const QVariantMap row = rowFromItem(item);
        if (m_painting) {
            m_paintRows << row;
            return;
        }
        if (m_loadingOlder) {
            m_olderPending << row;
            return;
        }
        m_messageModel->upsert(item.value(QStringLiteral("id")).toString(), row);
        m_currentIndex = -1;
        requestMessagesUpdate();
        // A tool row changed: its status or its sub-agent steps are what the activity
        // panel draws, and there is no separate event for either (CONTRACT §3.7). A
        // no-op refresh is dropped inside refreshActivity.
        const QString itemKind = item.value(QStringLiteral("kind")).toString();
        if (itemKind == QLatin1String("Tool") || itemKind == QLatin1String("ToolGroup"))
            refreshActivity();
        return;
    }
    if (root.contains(QStringLiteral("AppendText"))) {
        // A delta belongs to rows the paint has not landed yet: land it first.
        flushPaint();
        const QJsonObject o = root.value(QStringLiteral("AppendText")).toObject();
        m_messageModel->appendText(o.value(QStringLiteral("id")).toString(),
                                   o.value(QStringLiteral("chunk")).toString());
        m_tailTimer->start();   // restart: fires only once the reply goes quiet
        requestMessagesUpdate();
        return;
    }
    if (root.contains(QStringLiteral("AppendOutput"))) {
        flushPaint();
        const QJsonObject o = root.value(QStringLiteral("AppendOutput")).toObject();
        m_messageModel->appendOutput(o.value(QStringLiteral("id")).toString(),
                                     o.value(QStringLiteral("chunk")).toString());
        m_tailTimer->start();
        requestMessagesUpdate();
        return;
    }
    if (root.contains(QStringLiteral("Remove"))) {
        flushPaint();
        const QJsonObject o = root.value(QStringLiteral("Remove")).toObject();
        m_messageModel->removeById(o.value(QStringLiteral("id")).toString());
        requestMessagesUpdate();
    }
}

void TranscriptProjection::finishTail(int lookback)
{
    const int n = m_messageModel->count();
    bool touched = false;
    for (int i = n - 1; i >= 0 && i >= n - lookback; --i) {
        QVariantMap row = m_messageModel->row(i);
        const QString role = row.value("role").toString();
        if (role != QLatin1String("user") && role != QLatin1String("agent")
            && role != QLatin1String("error"))
            continue;
        if (!row.value("html").toString().isEmpty())
            continue;
        if (!row.value("blocks").toList().isEmpty())
            continue;
        if (row.value("text").toString().trimmed().isEmpty())
            continue;
        ensureRowHtml(row);
        // update() (not updateDeferred): this row is final, so the last frame
        // paints immediately instead of waiting on the coalescing timer.
        m_messageModel->update(i, row);
        touched = true;
    }
    if (touched)
        requestMessagesUpdate();
}

void TranscriptProjection::renderTail()
{
    const int n = m_messageModel->count();
    if (n <= 0)
        return;
    // Only the tail can be waiting on a seal: every row above it was sealed when
    // the stream moved on to the next one.
    const int i = n - 1;
    QVariantMap row = m_messageModel->row(i);
    const QString role = row.value("role").toString();
    if (role != QLatin1String("user") && role != QLatin1String("agent")
        && role != QLatin1String("error"))
        return;
    if (!row.value("html").toString().isEmpty())
        return;
    if (!row.value("blocks").toList().isEmpty())
        return;
    const QString text = row.value("text").toString();
    if (text.trimmed().isEmpty())
        return;
    // One parse per quiet period: a stall in the middle of a long reply must not
    // re-parse the same prefix on every pause. `appendText` clears html, so text
    // that keeps growing earns a fresh render at the next quiet period.
    const QString sig = row.value("id").toString() + QLatin1Char(':')
        + QString::number(text.size());
    if (sig == m_tailRendered)
        return;
    m_tailRendered = sig;
    ensureRowHtml(row);
    m_messageModel->update(i, row);
    requestMessagesUpdate();
}

void TranscriptProjection::loadOlder(int count)
{
    // The core owns the window (docs/TRANSCRIPT_MODEL.md): it emits the older
    // items as Upserts and a refreshed Window, which coreOnItem then prepends in
    // one batch. Nothing is buffered client-side.
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    if (!m_itemHasOlder || m_loadingOlder)
        return;
    m_loadingOlder = true;
    m_bridge->api().grouse_load_older(m_bridge->handle(),
                                      count <= 0 ? 40u : static_cast<unsigned>(count));
}

void TranscriptProjection::applyFile(const QString &json)
{
    const QJsonObject root = parseObj(json);
    // FileEvent is an externally-tagged enum; the fields live inside the variant.
    // Every variant now carries the DISPLAY session id (roam ids included), so key
    // the card by owner + path, never by a bare path two chats can share.
    QJsonObject fields;
    for (const auto &variant : {QStringLiteral("Progress"), QStringLiteral("Saved"),
                                QStringLiteral("Failed")}) {
        if (root.contains(variant)) {
            fields = root.value(variant).toObject();
            break;
        }
    }
    const QString path = fields.value(QStringLiteral("path")).toString();
    QString sessionId = fields.value(QStringLiteral("session_id")).toString();
    if (sessionId.isEmpty())
        sessionId = root.value(QStringLiteral("session_id")).toString();
    if (sessionId.isEmpty()) {
        // An older core has no session in the event: the fetch was started from
        // the chat on screen, which is the only honest fallback we have.
        sessionId = m_currentSessionId;
    } else if (!sessionId.startsWith(QLatin1String("roam:"))
               && m_currentSessionId.endsWith(QStringLiteral(":") + sessionId)
               && !sessionPeerLabel(m_currentSessionId).isEmpty()) {
        // A peer's raw wire id in an older event: normalize to the display id the
        // row is keyed by (the core's DISPLAY id already matches and skips this).
        sessionId = m_currentSessionId;
    }
    QJsonObject out = root;
    out.insert(QStringLiteral("session_id"), sessionId);
    out.insert(QStringLiteral("key"), fileCardKey(sessionId, path));
    emit m_manager.fileEvent(QString::fromUtf8(QJsonDocument(out).toJson(QJsonDocument::Compact)));
}

void TranscriptProjection::fetchFile(const QString &sessionId, const QString &path,
                        const QString &name, const QString &mime,
                        const QString &sha256)
{
    if (!m_bridge || !m_bridge->isAvailable())
        return;
    if (path.isEmpty()) {
        onError(QStringLiteral("this file has no server path to fetch"), false);
        return;
    }
    const QByteArray sid = sessionId.toUtf8();
    const QByteArray p = path.toUtf8();
    const QByteArray n = name.toUtf8();
    const QByteArray mt = mime.toUtf8();
    const QByteArray sha = sha256.toUtf8();
    char *err = nullptr;
    m_bridge->api().grouse_fetch_file(m_bridge->handle(), sid.constData(), p.constData(),
                                      n.constData(), mt.constData(), sha.constData(), &err);
    if (err) {
        const QString message = QString::fromUtf8(err);
        m_bridge->api().grouse_string_free(err);
        // Not a background failure: the user just tapped this card.
        onError(message, false);
    }
}

QVariantMap TranscriptProjection::attachmentCard(const QString &path) const
{
    const QFileInfo info(path);
    if (!info.exists())
        return QVariantMap{{"name", info.fileName()},
                           {"kind", QStringLiteral("File")},
                           {"icon", QStringLiteral("application-octet-stream")},
                           {"size", QString()},
                           {"subtitle", QStringLiteral("File")}};
    return fileCard(info.fileName(), QMimeDatabase().mimeTypeForFile(path).name(), info.size());
}

QVariantMap TranscriptProjection::serverFileCard(const QString &name, const QString &mime, qint64 bytes) const
{
    return fileCard(name, mime, bytes);
}
