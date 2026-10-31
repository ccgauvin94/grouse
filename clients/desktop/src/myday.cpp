// SPDX-License-Identifier: AGPL-3.0-or-later

#include "myday.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLocale>
#include <QTime>

namespace MyDay {

namespace {

/// One string field, tolerating a missing/null value: the server omits blanks
/// rather than sending empty strings, and a row must not carry Qt's "(null)".
QString str(const QJsonObject &o, const char *key)
{
    return o.value(QLatin1String(key)).toString();
}

QVariantMap itemRow(const QJsonObject &o)
{
    QVariantMap m;
    m["id"] = str(o, "id");
    m["kind"] = str(o, "kind");
    m["state"] = str(o, "state");
    m["title"] = str(o, "title");
    m["detail"] = str(o, "detail");
    // `due_at` is a bare date or a naive datetime — passed through verbatim and
    // formatted at paint time by dueLabel (which is where the sign bug lived).
    m["dueAt"] = str(o, "due_at");
    m["resolution"] = str(o, "resolution");
    m["snoozeUntil"] = str(o, "snooze_until");
    m["thread"] = str(o, "thread");
    return m;
}

QVariantMap taskRow(const QJsonObject &o)
{
    QVariantMap m;
    m["id"] = str(o, "id");
    m["title"] = str(o, "title");
    m["state"] = str(o, "state");
    m["thread"] = str(o, "thread");
    m["sessionId"] = str(o, "session_id");
    m["request"] = str(o, "request");
    m["result"] = str(o, "result");
    QStringList artifacts;
    for (const auto &el : o.value(QLatin1String("artifacts")).toArray()) {
        // Declared as `[{path, ...}]`; a bare string is tolerated so a future
        // server that flattens the list does not silently lose the files.
        const QString p = el.isObject() ? el.toObject().value(QLatin1String("path")).toString()
                                        : el.toString();
        if (!p.isEmpty())
            artifacts << p;
    }
    m["artifacts"] = artifacts;
    return m;
}

QVariantList rows(const QJsonValue &v, bool task)
{
    QVariantList out;
    for (const auto &el : v.toArray()) {
        const QJsonObject o = el.toObject();
        // A row without an id cannot be keyed or acted on; the phone drops those
        // too rather than painting them under a made-up key.
        if (str(o, "id").isEmpty())
            continue;
        out << (task ? taskRow(o) : itemRow(o));
    }
    return out;
}

} // namespace

Lists parse(const QString &json)
{
    Lists out;
    QJsonParseError err{};
    const QJsonDocument doc = QJsonDocument::fromJson(json.toUtf8(), &err);
    if (err.error != QJsonParseError::NoError || !doc.isObject()) {
        out.error = QStringLiteral("My Day: unreadable reply from the server");
        return out;
    }
    const QJsonObject root = doc.object();
    out.open = rows(root.value(QLatin1String("open")), false);
    out.done = rows(root.value(QLatin1String("done")), false);
    out.handed = rows(root.value(QLatin1String("tasks")), true);

    // `errors` is per section and omitted entirely when all three succeeded, so
    // an absent map means "nothing to report", not "no sections".
    QStringList failures;
    const QJsonObject errors = root.value(QLatin1String("errors")).toObject();
    for (auto it = errors.begin(); it != errors.end(); ++it) {
        const QString msg = it.value().isString() ? it.value().toString()
                                                  : QStringLiteral("failed");
        failures << QStringLiteral("%1: %2").arg(it.key(), msg);
    }
    out.error = failures.join(QLatin1Char('\n'));
    return out;
}

QString threadName(const QString &label)
{
    QString words = label.trimmed();
    words.replace(QLatin1Char('-'), QLatin1Char(' ')).replace(QLatin1Char('_'), QLatin1Char(' '));
    words = words.simplified();
    if (words.isEmpty())
        return QString();
    return words.left(1).toUpper() + words.mid(1);
}

QString threadTitle(const QString &label)
{
    const QString name = threadName(label);
    return name.isEmpty() ? QString() : QStringLiteral("Thread: ") + name;
}

QString dueLabel(const QString &iso, const QDate &today)
{
    const QString raw = iso.trimmed();
    if (raw.isEmpty())
        return QString();
    const QDate date = QDate::fromString(raw.left(10), Qt::ISODate);
    if (!date.isValid())
        return QStringLiteral("due %1").arg(raw.left(10));

    // `daysTo(a)` is a - *this, so TODAY has to be the receiver for a future due
    // date to come out positive. Called the other way round (as it was) every
    // branch inverts: an item due in a week read "overdue 7d" and a flu shot 15
    // days past read "due in 15d".
    const int days = today.daysTo(date);
    if (days <= -14)
        return QStringLiteral("overdue %1w").arg(-days / 7);
    if (days < 0)
        return QStringLiteral("overdue %1d").arg(-days);
    if (days == 0)
        return QStringLiteral("due today");
    if (days == 1)
        return QStringLiteral("due tomorrow");
    if (days <= 30)
        return QStringLiteral("due in %1d").arg(days);
    return QStringLiteral("due %1").arg(QLocale().toString(date, QStringLiteral("MMM d")));
}

QString wakeLabel(const QString &iso, const QDateTime &now)
{
    if (iso.trimmed().isEmpty())
        return QString();
    const QDateTime until = QDateTime::fromString(iso, Qt::ISODate);
    if (!until.isValid())
        return QString();
    const qint64 mins = now.msecsTo(until) / 60000;
    if (mins <= 0)
        return QStringLiteral("wakes now");
    const QDateTime local = until.toLocalTime();
    if (mins < 90)
        return QStringLiteral("wakes in %1m").arg(mins);
    if (mins < 12 * 60)
        return QStringLiteral("wakes in %1h").arg(mins / 60);
    const QString clock = local.time().toString(QStringLiteral("H:mm"));
    return now.date() == local.date() ? QStringLiteral("wakes %1").arg(clock)
                                      : QStringLiteral("wakes tomorrow %1").arg(clock);
}

bool isSnoozed(const QVariantMap &row, const QDateTime &now)
{
    if (row.value(QStringLiteral("state")).toString() != QLatin1String("snoozed"))
        return false;
    const QDateTime until =
        QDateTime::fromString(row.value(QStringLiteral("snoozeUntil")).toString(), Qt::ISODate);
    if (!until.isValid())
        return true;
    return until > now;
}

QDateTime nextMorning(const QDateTime &now)
{
    QDateTime wake(now.date(), QTime(5, 0));
    // At least an hour ahead, so a tap at 04:30 does not "snooze" an item into
    // wakefulness a minute from now.
    if (wake < now.addSecs(3600))
        wake = wake.addDays(1);
    return wake;
}

QString artifactsLabel(const QStringList &paths)
{
    if (paths.isEmpty())
        return QString();
    const QString first = paths.first().section(QLatin1Char('/'), -1);
    return paths.size() == 1 ? first
                             : QStringLiteral("%1 files · %2").arg(paths.size()).arg(first);
}

QString metaLine(const QVariantMap &row, bool snoozed, const QDate &today,
                 const QDateTime &now)
{
    QStringList parts;
    if (snoozed) {
        const QString wake = wakeLabel(row.value(QStringLiteral("snoozeUntil")).toString(), now);
        if (!wake.isEmpty())
            parts << wake;
    }
    const QString thread = threadName(row.value(QStringLiteral("thread")).toString());
    if (!thread.isEmpty())
        parts << thread;
    const QString due = dueLabel(row.value(QStringLiteral("dueAt")).toString(), today);
    if (!due.isEmpty())
        parts << due;
    // "commitment" is the default kind, so naming it on every row says nothing.
    const QString kind = row.value(QStringLiteral("kind")).toString();
    if (!kind.isEmpty() && kind != QLatin1String("commitment"))
        parts << kind;
    return parts.join(QStringLiteral(" · "));
}

} // namespace MyDay
