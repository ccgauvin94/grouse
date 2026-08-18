#include <QtTest>

#include "roamlistmodel.h"

/** Sidebar model for the Roam tab: endpoint header rows with expandable
 *  session rows beneath. Guards the header/session row layout, the drop-down
 *  toggle, and the roles QML binds (title/snippet/cwd routing). */
class TstRoamListModel : public QObject
{
    Q_OBJECT
private slots:
    void headerThenSessions();
    void toggleExpandsAndCollapses();
    void sessionRolesCarryCwd();
    void removePeerClearsRows();
    void statusUpdatesByIdentity();
};

QVariant rowValue(const RoamListModel &m, int row, int role)
{
    return m.data(m.index(row), role);
}

void TstRoamListModel::headerThenSessions()
{
    RoamListModel m;
    QCOMPARE(m.rowCount(), 0);
    m.addPeer(QStringLiteral("ws"));
    m.setPeerSessions(QStringLiteral("ws"), QVariantList{
        QVariantMap{{"sessionId", "s1"}, {"title", "Chat one"}, {"cwd", "/srv/a"}},
        QVariantMap{{"sessionId", "s2"}, {"title", "Chat two"}},
    });
    // Collapsed: header only.
    QCOMPARE(m.rowCount(), 1);
    QCOMPARE(rowValue(m, 0, RoamListModel::RowTypeRole).toString(), QStringLiteral("header"));
    QCOMPARE(rowValue(m, 0, RoamListModel::LabelRole).toString(), QStringLiteral("ws"));
    m.togglePeer(QStringLiteral("ws"));
    // Expanded: header + 2 sessions.
    QCOMPARE(m.rowCount(), 3);
    QCOMPARE(rowValue(m, 1, RoamListModel::RowTypeRole).toString(), QStringLiteral("session"));
    QCOMPARE(rowValue(m, 1, RoamListModel::TitleRole).toString(), QStringLiteral("Chat one"));
    QCOMPARE(rowValue(m, 2, RoamListModel::SessionIdRole).toString(), QStringLiteral("s2"));
    QCOMPARE(m.headerRow(QStringLiteral("ws")), 0);
}

void TstRoamListModel::toggleExpandsAndCollapses()
{
    RoamListModel m;
    m.addPeer(QStringLiteral("ws"));
    m.setPeerSessions(QStringLiteral("ws"), QVariantList{QVariantMap{{"sessionId", "s1"}}});
    m.togglePeer(QStringLiteral("ws"));
    QCOMPARE(m.rowCount(), 2);
    QVERIFY(rowValue(m, 0, RoamListModel::ExpandedRole).toBool());
    m.togglePeer(QStringLiteral("ws"));
    QCOMPARE(m.rowCount(), 1);
}

void TstRoamListModel::sessionRolesCarryCwd()
{
    RoamListModel m;
    m.addPeer(QStringLiteral("ws"));
    m.setPeerSessions(QStringLiteral("ws"), QVariantList{
        QVariantMap{{"sessionId", "s1"}, {"title", "T"}, {"cwd", "/srv/work"},
                    {"snippet", "hi"}, {"messageCount", 7}}});
    m.togglePeer(QStringLiteral("ws"));
    QCOMPARE(rowValue(m, 1, RoamListModel::CwdRole).toString(), QStringLiteral("/srv/work"));
    QCOMPARE(rowValue(m, 1, RoamListModel::SnippetRole).toString(), QStringLiteral("hi"));
    QCOMPARE(rowValue(m, 1, RoamListModel::MessageCountRole).toInt(), 7);
    QCOMPARE(rowValue(m, 1, RoamListModel::LabelRole).toString(), QStringLiteral("ws"));
}

void TstRoamListModel::removePeerClearsRows()
{
    RoamListModel m;
    m.addPeer(QStringLiteral("a"));
    m.addPeer(QStringLiteral("b"));
    m.setPeerSessions(QStringLiteral("b"), QVariantList{QVariantMap{{"sessionId", "s1"}}});
    m.togglePeer(QStringLiteral("b"));
    QCOMPARE(m.rowCount(), 3);
    m.removePeer(QStringLiteral("b"));
    QCOMPARE(m.rowCount(), 1);
    QCOMPARE(rowValue(m, 0, RoamListModel::LabelRole).toString(), QStringLiteral("a"));
}

void TstRoamListModel::statusUpdatesByIdentity()
{
    RoamListModel m;
    m.addPeer(QStringLiteral("ws"));
    m.setPeerStatus(QStringLiteral("ws"), QStringLiteral("ready"), true);
    QVERIFY(rowValue(m, 0, RoamListModel::ConnectedRole).toBool());
    m.setPeerStatus(QStringLiteral("ws"), QStringLiteral("error: boom"), false);
    QVERIFY(!rowValue(m, 0, RoamListModel::ConnectedRole).toBool());
    QCOMPARE(rowValue(m, 0, RoamListModel::StatusRole).toString(), QStringLiteral("error: boom"));
}

QTEST_GUILESS_MAIN(TstRoamListModel)
#include "tst_roamlistmodel.moc"
