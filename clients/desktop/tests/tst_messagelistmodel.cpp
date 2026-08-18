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
    QVERIFY2(names.key("expanded") != 0, "expanded role");
    QCOMPARE(names.size(), 17);
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
    model.toggleExpanded(7);
    QCOMPARE(spy.size(), 1);
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), true);
    model.toggleExpanded(7);
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
    // Unknown id is a no-op, not a crash.
    model.toggleExpanded(999);
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
    // clear() resets the state.
    model.toggleExpanded(7);
    model.clear();
    model.append(message(7, QStringLiteral("thought"), QStringLiteral("hmm")));
    QCOMPARE(model.data(model.index(0), MessageListModel::ExpandedRole).toBool(), false);
}

QTEST_MAIN(TstMessageListModel)
#include "tst_messagelistmodel.moc"