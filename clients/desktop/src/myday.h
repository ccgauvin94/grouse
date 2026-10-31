#pragma once

// SPDX-License-Identifier: AGPL-3.0-or-later

#include <QDate>
#include <QDateTime>
#include <QString>
#include <QStringList>
#include <QVariantList>
#include <QVariantMap>

/**
 * My Day's pure half: one `on_tasks` document in, the rows a page paints out.
 *
 * The envelope itself is the core's (`{"open": [...], "done": [...],
 * "tasks": [...], "errors": {...}}`, CONTRACT §3.6) and the rows inside it are
 * already projected there — id, kind, state, title, detail, due_at, resolution,
 * snooze_until, thread. This translation is the ONLY place that knows the wire
 * spells them snake_case, and it is a free function over a string so it can be
 * unit-tested without a core, a wire, or a window (tests/tst_myday.cpp).
 *
 * Everything here is deliberately pure: the clock and the day are arguments, not
 * `QDate::currentDate()`. The due-date sign bug that read a three-days-away item
 * as "overdue" is exactly what a function that reads the clock internally cannot
 * be tested for.
 */
namespace MyDay {

/** One decoded envelope, as the QML binds it. */
struct Lists {
    /// Open items; a snoozed one is still in here (server-side it is open, just
    /// quiet) — `isSnoozed` is what separates them.
    QVariantList open;
    /// The closed tail, newest first as the server sends it.
    QVariantList done;
    /// Work handed over: assistant-state `task` rows, `running` or `review`.
    QVariantList handed;
    /// Per-section failure lines ("tasks: no such tool"), joined; empty when all
    /// three succeeded. "Nothing to do" and "could not ask" must not look alike.
    QString error;
};

/// Decode `on_tasks`. A document that will not parse yields empty lists and a
/// non-empty `error` rather than throwing a half-filled page.
Lists parse(const QString &json);

/// The thread label as a reader wants it: `chami-billing` → "Chami billing".
/// A label is all a thread is, so there is nothing to look up; blank stays blank.
QString threadName(const QString &label);

/// The session title a thread chat is renamed to ("Thread: Chami billing"), so it
/// reads as the area rather than as the slug it was created from.
QString threadTitle(const QString &label);

/// "overdue 2w" / "due today" / "due in 7d" / "due Nov 6" for a `due_at` that is
/// either a bare date or a naive datetime. `today` is injected (see above).
QString dueLabel(const QString &iso, const QDate &today);

/// "wakes in 6h" / "wakes tomorrow 05:00" for a snooze's `until`.
QString wakeLabel(const QString &iso, const QDateTime &now);

/// Whether a row is asleep right now. A snooze whose `until` has passed reads as
/// open — the server wakes it on its next rank pass — and an unparseable stamp
/// reads as snoozed, because keeping a row quiet is the safe direction.
bool isSnoozed(const QVariantMap &row, const QDateTime &now);

/// The next 05:00 local: quiet overnight, back before the 06:00 digest. A flat
/// "+24h" from an evening tap lands AFTER that briefing and silently skips a day.
QDateTime nextMorning(const QDateTime &now);

/// "2 files · deck.pdf" — a count plus the first name: a task's value is usually a
/// file, and the full path list belongs in the chat, not in a meta line.
QString artifactsLabel(const QStringList &paths);

/// The one-line meta strip under a row: wake time (when snoozed), thread, due,
/// kind. Empty segments are dropped rather than joined into " ·  · ".
QString metaLine(const QVariantMap &row, bool snoozed, const QDate &today,
                 const QDateTime &now);

} // namespace MyDay
