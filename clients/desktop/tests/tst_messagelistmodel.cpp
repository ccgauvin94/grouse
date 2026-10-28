// MessageListModel: the transcript model. The behaviors that matter:
// incremental appends, immediate-but-coalesced deferred updates (the per-token
// streaming path), and a clear() that must not crash a pending commit.

#include <QtTest>
#include <QSignalSpy>

#include "messagelistmodel.h"

class TstMessageListModel : public QObject
{
    Q_OBJECT

private slots:
    void roleNamesAreExplicit();
    void appendGrowsAndSignals();
    void updateReplacesRow();
    void updateDeferredIsImmediateButCoalesces();
    void updateDeferredSeparateRowsNotifySeparately();
    void clearDuringPendingDeferredIsSafe();
    void rowAndRowsAccessors();
    void expandedStartsCollapsedAndToggles();
    void expandedFollowsTheMessageAcrossPrepend();
    void expandedFollowsTheMessageAcrossInsertAndRemove();
};

QVariantMap message(int id, const QString &role, const QString &text)
{
    return {{"id", id}, {"role", role}, {"text", text}};
}

void TstMessageListModel::roleNamesAreExplicit()
{
    MessageListModel model;
    const QHash<int, QByteArray> names = model.roleNames();
    // QML delegates read model.text / model.html / model.chartData ... — the
    // exact role names are part of the contract.
    QVERIFY2(names.key("text") != 0, "text role");
    QVERIFY2(names.key("role") != 0, "role role");
    QVERIFY2(names.key("html") != 0, "html role");
    QVERIFY2(names.key("thought") != 0, "thought role");
    QVERIFY2(names.key("chartData") != 0, "chartData role");
    QVERIFY2(names.key("toolCallId") != 0, "toolCallId role");
    QVERIFY2(names.key("calls") != 0, "calls role");
    QVERIFY2(names.key("summary") != 0, "summary role");
    QVERIFY2(names.key("stepThought") != 0, "stepThought role");
    QVERIFY2(names.key("expanded") != 0, "expanded role");
    // The rest of the set, named rather than counted: a numeric pin on the
    // total (it used to be 19) said nothing about WHICH name was missing and
    // went stale the moment a role was added.
    QVERIFY2(names.key("id") != 0, "id role");
    QVERIFY2(names.key("title") != 0, "title role");
    QVERIFY2(names.key("detail") != 0, "detail role");
    QVERIFY2(names.key("output") != 0, "output role");
    QVERIFY2(names.key("status") != 0, "status role");
    QVERIFY2(names.key("images") != 0, "images role");
    QVERIFY2(names.key("usage") != 0, "usage role");
    QVERIFY2(names.key("appHtml") != 0, "appHtml role");
    QVERIFY2(names.key("appKey") != 0, "appKey role");
    QVERIFY2(names.key("steps") != 0, "steps role");
}

void TstMessageListModel::appendGrowsAndSignals()
{
    MessageListModel model;
    QSignalSpy countSpy(&model, &MessageListModel::countChanged);
    model.append(message(1, QStringLiteral("user"), QStringLiteral("hi")));
    QCOMPARE(model.rowCount(), 1);
    QCOMPARE(countSpy.count(), 1);
    QCOMPARE(model.row(0).value("text").toString(), QStringLiteral("hi"));
    model.append(message(2, QStringLiteral("agent"), QStringLiteral("yo")));
    QCOMPARE(model.rowCount(), 2);
    QCOMPARE(model.data(model.index(1, 0), MessageListModel::RoleRole).toString(),
             QStringLiteral("agent"));
}

void TstMessageListModel::updateReplacesRow()
{
    MessageListModel model;
    model.append(message(1, QStringLiteral("agent"), QStringLiteral("a")));
    QSignalSpy changedSpy(&model, &QAbstractItemModel::dataChanged);
    model.update(0, message(1, QStringLiteral("agent"), QStringLiteral("ab")));
    QCOMPARE(changedSpy.count(), 1);
    QCOMPARE(model.row(0).value("text").toString(), QStringLiteral("ab"));
}

void TstMessageListModel::updateDeferredIsImmediateButCoalesces()
{
    MessageListModel model;
    model.append(message(1, QStringLiteral("agent"), QString()));
    QSignalSpy changedSpy(&model, &QAbstractItemModel::dataChanged);
    // Streaming: many updates per turn; each must land in the row immediately...
    model.updateDeferred(0, message(1, QStringLiteral("agent"), QStringLiteral("a")));
    QCOMPARE(model.row(0).value("text").toString(), QStringLiteral("a"));
    model.updateDeferred(0, message(1, QStringLiteral("agent"), QStringLiteral("ab")));
    model.updateDeferred(0, message(1, QStringLiteral("agent"), QStringLiteral("abc")));
    QCOMPARE(model.row(0).value("text").toString(), QStringLiteral("abc"));
    // ...but QML only gets ONE coalesced dataChanged for the burst.
    QCOMPARE(changedSpy.count(), 0);
    QTRY_COMPARE(changedSpy.count(), 1);
    // A subsequent burst coalesces into a second single emission.
    model.updateDeferred(0, message(1, QStringLiteral("agent"), QStringLiteral("abcd")));
    QTRY_COMPARE(changedSpy.count(), 2);
}

void TstMessageListModel::updateDeferredSeparateRowsNotifySeparately()
{
    MessageListModel model;
    model.append(message(1, QStringLiteral("user"), QStringLiteral("q")));
    model.append(message(2, QStringLiteral("agent"), QString()));
    QSignalSpy changedSpy(&model, &QAbstractItemModel::dataChanged);
    model.updateDeferred(0, message(1, QStringLiteral("user"), QStringLiteral("q!")));
    model.updateDeferred(1, message(2, QStringLiteral("agent"), QStringLiteral("a")));
    QTRY_COMPARE(changedSpy.count(), 2);
}

void TstMessageListModel::clearDuringPendingDeferredIsSafe()
{
    MessageListModel model;
    model.append(message(1, QStringLiteral("agent"), QString()));
    QSignalSpy changedSpy(&model, &QAbstractItemModel::dataChanged);
    model.updateDeferred(0, message(1, QStringLiteral("agent"), QStringLiteral("x")));
    model.clear();   // timer stopped, dirty rows dropped — must not crash or notify
    QTest::qWait(300);
    QCOMPARE(changedSpy.count(), 0);
    QCOMPARE(model.rowCount(), 0);
}

void TstMessageListModel::rowAndRowsAccessors()
{
    MessageListModel model;
    QCOMPARE(model.row(0), QVariantMap());
    model.append(message(1, QStringLiteral("user"), QStringLiteral("hi")));
    QCOMPARE(model.rows().size(), 1);
    QCOMPARE(model.row(0).value("id").toInt(), 1);
    QCOMPARE(model.row(99), QVariantMap());
}

void TstMessageListModel::expandedStartsCollapsedAndToggles()
{
    MessageListModel model;
    model.append(message(7, QStringLiteral("thought"), QStringLiteral("hmm")));
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
    QSignalSpy spy(&model, &QAbstractItemModel::dataChanged);
    model.toggleExpanded(0);
    QCOMPARE(spy.size(), 1);
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), true);
    model.toggleExpanded(0);
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
    // Unknown row index is a no-op, not a crash.
    model.toggleExpanded(999);
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
    // clear() resets the state.
    model.toggleExpanded(0);
    model.clear();
    model.append(message(7, QStringLiteral("thought"), QStringLiteral("hmm")));
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
}

void TstMessageListModel::expandedFollowsTheMessageAcrossPrepend()
{
    MessageListModel model;
    model.append(message(10, QStringLiteral("thought"), QStringLiteral("first")));
    model.append(message(11, QStringLiteral("thought"), QStringLiteral("second")));
    model.toggleExpanded(1);   // open the SECOND bubble
    QCOMPARE(model.data(model.index(1), MessageListModel::ExpandedRole).toBool(), true);

    // A load-older batch always arrives at the front. The open state must travel
    // with its message: keying it by row index left it on row 1, so a different
    // bubble came back expanded after every scroll-up (and nothing re-notified
    // the rows that had inherited it).
    QList<QVariantMap> older;
    for (int i = 1; i <= 3; ++i)
        older << message(100 + i, QStringLiteral("thought"), QStringLiteral("older"));
    model.prepend(older);

    QCOMPARE(model.rowCount(), 5);
    QCOMPARE(model.row(4).value("id").toInt(), 11);   // the open one, now shifted down
    QCOMPARE(model.data(model.index(4), MessageListModel::ExpandedRole).toBool(), true);
    for (int i = 0; i < 4; ++i)
        QCOMPARE(model.data(model.index(i), MessageListModel::ExpandedRole).toBool(), false);
}

void TstMessageListModel::expandedFollowsTheMessageAcrossInsertAndRemove()
{
    MessageListModel model;
    model.append({{"id", "a"}, {"role", "thought"}, {"seq", 10}});
    model.append({{"id", "b"}, {"role", "thought"}, {"seq", 20}});
    model.toggleExpanded(1);   // open "b"

    // A row that outranks "b" is inserted before it from `seq`, not appended:
    // the open state stays with "b" (now row 2) rather than with row 1.
    model.upsert("c", {{"id", "c"}, {"role", "thought"}, {"seq", 15}});
    QCOMPARE(model.rowCount(), 3);
    QCOMPARE(model.row(2).value("id").toString(), QStringLiteral("b"));
    QCOMPARE(model.data(model.index(2), MessageListModel::ExpandedRole).toBool(), true);
    QCOMPARE(model.data(model.index(1), MessageListModel::ExpandedRole).toBool(), false);

    // Removing a neighbour shifts "b" back up; the state comes with it.
    model.removeById("c");
    QCOMPARE(model.row(1).value("id").toString(), QStringLiteral("b"));
    QCOMPARE(model.data(model.index(1), MessageListModel::ExpandedRole).toBool(), true);
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
}

QTEST_MAIN(TstMessageListModel)
#include "tst_messagelistmodel.moc"
