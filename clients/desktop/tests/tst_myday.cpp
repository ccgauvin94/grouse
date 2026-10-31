// SPDX-License-Identifier: AGPL-3.0-or-later

// My Day's pure half (src/myday.cpp): the `on_tasks` envelope decode and the date
// formatting the page paints. Both are the kind of code that fails quietly — a
// mis-signed day difference painted "overdue 7d" on an item due next week for
// months — so the clock and the day are ARGUMENTS here and every branch is pinned.

#include "myday.h"

#include <QTest>

class TestMyDay : public QObject
{
    Q_OBJECT

private slots:
    // ---- the envelope ----------------------------------------------------
    void parsesThreeSections();
    void joinsPerSectionErrors();
    void unreadableReplyIsAnErrorNotAPartialPage();
    void dropsRowsWithoutAnId();
    void artifactsArePathsAndBareStrings();

    // ---- due dates -------------------------------------------------------
    void dueLabel_countsForwardFromToday();
    void dueLabel_readsPastDatesAsOverdue();
    void dueLabel_handlesTheFarEdges();
    void dueLabel_neverInventsADate();

    // ---- snooze ----------------------------------------------------------
    void isSnoozed_usesTheWakeTime();
    void isSnoozed_keepsAnUnparseableRowQuiet();
    void wakeLabel_countsDown();
    void nextMorning_isTomorrowWhenTodayHasPassed();

    // ---- labels + meta ---------------------------------------------------
    void threadName_flattensSlugs();
    void threadTitle_prefixesTheThread();
    void artifactsLabel_countsAndNamesOne();
    void metaLine_dropsEmptySegments();
};

namespace {
const char *kEnvelope = R"({
  "open": [
    {"id": "itm_1", "kind": "commitment", "state": "open",
     "title": "Renew the domain", "detail": "expires soon",
     "due_at": "2026-11-06", "thread": "chami-billing", "salience": 0.7},
    {"id": "itm_2", "state": "snoozed", "title": "Ring the vet",
     "snooze_until": "2099-01-01T05:00:00Z"},
    {"title": "no id at all"}
  ],
  "done": [
    {"id": "itm_3", "state": "done", "title": "Book the flight",
     "resolution": "booked it"}
  ],
  "tasks": [
    {"id": "tsk_1", "title": "Draft the deck", "state": "review",
     "thread": "chami-billing", "session_id": "20261030_1",
     "request": "make a deck", "result": "wrote 9 slides",
     "artifacts": [{"path": "/workspace/deck.pdf"}, "/workspace/model.xlsx"]}
  ]
})";
}

void TestMyDay::parsesThreeSections()
{
    const MyDay::Lists l = MyDay::parse(QString::fromUtf8(kEnvelope));
    QVERIFY2(l.error.isEmpty(), qPrintable(l.error));
    QCOMPARE(l.open.size(), 2);      // the id-less row is dropped
    QCOMPARE(l.done.size(), 1);
    QCOMPARE(l.handed.size(), 1);

    const QVariantMap first = l.open.at(0).toMap();
    QCOMPARE(first.value("id").toString(), QStringLiteral("itm_1"));
    QCOMPARE(first.value("dueAt").toString(), QStringLiteral("2026-11-06"));
    QCOMPARE(first.value("thread").toString(), QStringLiteral("chami-billing"));

    const QVariantMap task = l.handed.at(0).toMap();
    QCOMPARE(task.value("state").toString(), QStringLiteral("review"));
    QCOMPARE(task.value("sessionId").toString(), QStringLiteral("20261030_1"));
    QCOMPARE(task.value("result").toString(), QStringLiteral("wrote 9 slides"));
}

void TestMyDay::joinsPerSectionErrors()
{
    // A server older than assistant-state v4 has no `task_list`; the section must
    // say so rather than reading as "nothing was handed over".
    const MyDay::Lists l = MyDay::parse(QStringLiteral(
        R"({"open": [], "done": [], "errors": {"tasks": "unknown tool"}})"));
    QCOMPARE(l.handed.size(), 0);
    QCOMPARE(l.error, QStringLiteral("tasks: unknown tool"));
}

void TestMyDay::unreadableReplyIsAnErrorNotAPartialPage()
{
    const MyDay::Lists l = MyDay::parse(QStringLiteral("not json"));
    QVERIFY(!l.error.isEmpty());
    QCOMPARE(l.open.size(), 0);
    QCOMPARE(l.done.size(), 0);
    QCOMPARE(l.handed.size(), 0);
}

void TestMyDay::dropsRowsWithoutAnId()
{
    // A row with no id cannot be keyed, closed or snoozed, so it is not painted.
    const MyDay::Lists l = MyDay::parse(QStringLiteral(R"({"open": [{"title": "x"}, {"id": ""}]})"));
    QCOMPARE(l.open.size(), 0);
}

void TestMyDay::artifactsArePathsAndBareStrings()
{
    const MyDay::Lists l = MyDay::parse(QString::fromUtf8(kEnvelope));
    const QStringList paths = l.handed.at(0).toMap().value("artifacts").toStringList();
    QCOMPARE(paths, QStringList({QStringLiteral("/workspace/deck.pdf"),
                                 QStringLiteral("/workspace/model.xlsx")}));
}

void TestMyDay::dueLabel_countsForwardFromToday()
{
    // The regression this pins: `DAYS.between(a, b)` (and QDate::daysTo) is b - a,
    // so with the arguments reversed an item due in a week read "overdue 7d" and a
    // shot 15 days past read "due in 15d" — both exactly backwards.
    const QDate today(2026, 10, 30);
    QCOMPARE(MyDay::dueLabel("2026-11-06", today), QStringLiteral("due in 7d"));
    QCOMPARE(MyDay::dueLabel("2026-10-15", today), QStringLiteral("overdue 2w"));
    QCOMPARE(MyDay::dueLabel("2026-10-30", today), QStringLiteral("due today"));
    QCOMPARE(MyDay::dueLabel("2026-10-31", today), QStringLiteral("due tomorrow"));
    // A naive datetime (no zone) is the shape goose writes for a timed due date.
    QCOMPARE(MyDay::dueLabel("2026-11-02T15:30", today), QStringLiteral("due in 3d"));
}

void TestMyDay::dueLabel_readsPastDatesAsOverdue()
{
    const QDate today(2026, 10, 30);
    QCOMPARE(MyDay::dueLabel("2026-10-29", today), QStringLiteral("overdue 1d"));
    QCOMPARE(MyDay::dueLabel("2026-10-16", today), QStringLiteral("overdue 2w"));
    // 13 days is still days; the weeks form starts at exactly two weeks.
    QCOMPARE(MyDay::dueLabel("2026-10-17", today), QStringLiteral("overdue 13d"));
    QCOMPARE(MyDay::dueLabel("2026-10-15", today), QStringLiteral("overdue 2w"));
}

void TestMyDay::dueLabel_handlesTheFarEdges()
{
    const QDate today(2026, 10, 30);
    QCOMPARE(MyDay::dueLabel("2026-11-29", today), QStringLiteral("due in 30d"));
    // Past a month the exact day matters more than a count of weeks.
    QCOMPARE(MyDay::dueLabel("2026-12-25", today), QStringLiteral("due Dec 25"));
}

void TestMyDay::dueLabel_neverInventsADate()
{
    const QDate today(2026, 10, 30);
    QCOMPARE(MyDay::dueLabel("", today), QString());
    QCOMPARE(MyDay::dueLabel("   ", today), QString());
    // Junk is echoed as the raw day rather than silently becoming "due today".
    QCOMPARE(MyDay::dueLabel("soon-ish", today), QStringLiteral("due soon-ish"));
}

void TestMyDay::isSnoozed_usesTheWakeTime()
{
    const QDateTime now = QDateTime::fromString(QStringLiteral("2026-10-30T12:00:00Z"),
                                                Qt::ISODate);
    QVariantMap row{{"state", "snoozed"}, {"snoozeUntil", "2026-10-30T18:00:00Z"}};
    QVERIFY(MyDay::isSnoozed(row, now));
    // A snooze whose `until` has passed reads as OPEN: the server wakes it on its
    // next rank pass, and until then the timestamp is the only truth.
    row["snoozeUntil"] = QStringLiteral("2026-10-30T06:00:00Z");
    QVERIFY(!MyDay::isSnoozed(row, now));
    // Not snoozed at all, whatever the stamp says.
    row["state"] = QStringLiteral("open");
    row["snoozeUntil"] = QStringLiteral("2026-10-30T18:00:00Z");
    QVERIFY(!MyDay::isSnoozed(row, now));
}

void TestMyDay::isSnoozed_keepsAnUnparseableRowQuiet()
{
    // Keeping it quiet is the safe direction: the user asked not to see it.
    const QVariantMap row{{"state", "snoozed"}, {"snoozeUntil", "tomorrow-ish"}};
    QVERIFY(MyDay::isSnoozed(row, QDateTime::currentDateTime()));
}

void TestMyDay::wakeLabel_countsDown()
{
    const QDateTime now = QDateTime::fromString(QStringLiteral("2026-10-30T12:00:00Z"),
                                                Qt::ISODate);
    QCOMPARE(MyDay::wakeLabel("2026-10-30T12:30:00Z", now), QStringLiteral("wakes in 30m"));
    QCOMPARE(MyDay::wakeLabel("2026-10-30T18:00:00Z", now), QStringLiteral("wakes in 6h"));
    QCOMPARE(MyDay::wakeLabel("2026-10-30T11:00:00Z", now), QStringLiteral("wakes now"));
    QCOMPARE(MyDay::wakeLabel("", now), QString());
}

void TestMyDay::nextMorning_isTomorrowWhenTodayHasPassed()
{
    // 05:00 is the point: quiet overnight, back before the 06:00 digest. A tap in
    // the evening must NOT land after that briefing.
    const QDateTime evening(QDate(2026, 10, 30), QTime(21, 0));
    const QDateTime wake = MyDay::nextMorning(evening);
    QCOMPARE(wake.date(), QDate(2026, 10, 31));
    QCOMPARE(wake.time(), QTime(5, 0));

    // A tap just after midnight still wakes the same morning.
    const QDateTime night(QDate(2026, 10, 31), QTime(1, 0));
    QCOMPARE(MyDay::nextMorning(night).date(), QDate(2026, 10, 31));

    // ...but one inside the last hour before it rolls to tomorrow, so the snooze
    // is never shorter than an hour (a 04:30 tap would otherwise wake in 30 min).
    const QDateTime almost(QDate(2026, 10, 31), QTime(4, 30));
    QCOMPARE(MyDay::nextMorning(almost).date(), QDate(2026, 11, 1));
}

void TestMyDay::threadName_flattensSlugs()
{
    QCOMPARE(MyDay::threadName("chami-billing"), QStringLiteral("Chami billing"));
    QCOMPARE(MyDay::threadName("msu_cryoarm-contract"), QStringLiteral("Msu cryoarm contract"));
    QCOMPARE(MyDay::threadName("  "), QString());
    QCOMPARE(MyDay::threadName(""), QString());
}

void TestMyDay::threadTitle_prefixesTheThread()
{
    QCOMPARE(MyDay::threadTitle("chami-billing"), QStringLiteral("Thread: Chami billing"));
    QCOMPARE(MyDay::threadTitle(""), QString());
}

void TestMyDay::artifactsLabel_countsAndNamesOne()
{
    QCOMPARE(MyDay::artifactsLabel({}), QString());
    QCOMPARE(MyDay::artifactsLabel({QStringLiteral("/workspace/deck.pdf")}),
             QStringLiteral("deck.pdf"));
    QCOMPARE(MyDay::artifactsLabel({QStringLiteral("/a/deck.pdf"), QStringLiteral("/a/b.xlsx")}),
             QStringLiteral("2 files · deck.pdf"));
}

void TestMyDay::metaLine_dropsEmptySegments()
{
    const QDate today(2026, 10, 30);
    const QDateTime now(QDate(2026, 10, 30), QTime(12, 0, 0));
    // "commitment" is the default kind, so naming it says nothing.
    const QVariantMap plain{{"kind", "commitment"}, {"thread", ""}, {"dueAt", ""}};
    QCOMPARE(MyDay::metaLine(plain, false, today, now), QString());

    const QVariantMap full{{"kind", "waiting_on"}, {"thread", "chami-billing"},
                           {"dueAt", "2026-11-06"}, {"snoozeUntil", ""}};
    QCOMPARE(MyDay::metaLine(full, false, today, now),
             QStringLiteral("Chami billing · due in 7d · waiting_on"));

    QVariantMap asleep = full;
    asleep["snoozeUntil"] = QStringLiteral("2026-10-30T18:00:00Z");
    QCOMPARE(MyDay::metaLine(asleep, true, today, now),
             QStringLiteral("wakes in 6h · Chami billing · due in 7d · waiting_on"));
}

QTEST_MAIN(TestMyDay)
#include "tst_myday.moc"
