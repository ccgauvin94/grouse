// SessionListModel: the sidebar's grouped session list. Behaviors that matter:
// group headers (project / chats / peer), recency sort, collapse/expand,
// cwdFor() (the source of cwd for session/load — never guessed), and the
// no-op guard that stops an unchanged list from resetting the ListView.

#include <QtTest>
#include <QSignalSpy>

#include "sessionlistmodel.h"

class TstSessionListModel : public QObject
{
    Q_OBJECT

private slots:
    void roleNamesUseLowercaseSessionFields();
    void groupsProjectsThenChats();
    void sortsByRecencyWithinGroup();
    void collapseHidesSessions();
    void collapseProjectsWrapperHidesProjects();
    void projectCollapseHidesOnlyItsSessions();
    void cwdForFindsCwd();
    void unknownProjectSortsLastAndFallsBackToId();
    void peerSessionsGroupLast();
    void unchangedListDoesNotResetModel();
};

static QVariantMap session(const QString &id, const QString &title,
                           const QString &updatedAt, const QString &projectId = QString(),
                           const QString &peer = QString(), const QString &cwd = QString())
{
    return {{"sessionId", id}, {"title", title}, {"updatedAt", updatedAt},
            {"lastMessageAt", updatedAt}, {"snippet", QString()}, {"messageCount", 0},
            {"projectId", projectId}, {"peer", peer}, {"cwd", cwd}, {"model", QString()}};
}

static QVariantMap project(const QString &id, const QString &name)
{
    return {{"id", id}, {"name", name}};
}

// SessionListModel exposes data() only (no row()); map the roles back to the
// field names the assertions below use.
static QVariantMap rowAt(const SessionListModel &m, int r)
{
    const QModelIndex i = m.index(r, 0);
    return {
        {"title", m.data(i, SessionListModel::TitleRole)},
        {"sessionId", m.data(i, SessionListModel::SessionIdRole)},
        {"section", m.data(i, SessionListModel::SectionRole)},
        {"header", m.data(i, SessionListModel::HeaderRole)},
        {"collapsed", m.data(i, SessionListModel::CollapsedRole)},
        {"count", m.data(i, SessionListModel::CountRole)},
    };
}

void TstSessionListModel::roleNamesUseLowercaseSessionFields()
{
    // Explicit lowercase roles fix the QML case-lowering footgun that made
    // `messageCount` silently read as undefined.
    SessionListModel model;
    const QHash<int, QByteArray> names = model.roleNames();
    QVERIFY2(names.key("sessionid") != 0, "sessionid role");
    QVERIFY2(names.key("messagecount") != 0, "messagecount role");
    QVERIFY2(names.key("projectid") != 0, "projectid role");
    QVERIFY2(names.key("header") != 0, "header role");
    QVERIFY2(names.key("collapsed") != 0, "collapsed role");
    QCOMPARE(names.size(), 11);
}

void TstSessionListModel::groupsProjectsThenChats()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("s-chat-1"), QStringLiteral("Chat one"), QStringLiteral("2026-08-01")),
        session(QStringLiteral("s-proj-1"), QStringLiteral("In project"), QStringLiteral("2026-08-02"),
                QStringLiteral("p1")),
        session(QStringLiteral("s-chat-2"), QStringLiteral("Chat two"), QStringLiteral("2026-08-03")),
    });
    model.setProjects(QVariantList{project(QStringLiteral("p1"), QStringLiteral("Alpha"))});

    // Top-level "Projects" wrapper (above Chats), then the project below it,
    // then "Chats". Projects must default to sitting above Chats.
    QCOMPARE(model.rowCount(), 6);
    const QVariantMap h0 = rowAt(model, 0);
    QCOMPARE(h0.value("header").toBool(), true);
    QCOMPARE(h0.value("section").toString(), QStringLiteral("projects"));
    QCOMPARE(h0.value("title").toString(), QStringLiteral("Projects"));
    QCOMPARE(h0.value("count").toInt(), 1);

    // The project group nestles under the Projects wrapper.
    const QVariantMap h1 = rowAt(model, 1);
    QCOMPARE(h1.value("header").toBool(), true);
    QCOMPARE(h1.value("section").toString(), QStringLiteral("proj:p1"));
    QCOMPARE(h1.value("title").toString(), QStringLiteral("Alpha"));
    QCOMPARE(h1.value("count").toInt(), 1);
    QCOMPARE(rowAt(model, 2).value("sessionId").toString(), QStringLiteral("s-proj-1"));

    const QVariantMap h3 = rowAt(model, 3);
    QCOMPARE(h3.value("section").toString(), QStringLiteral("chats"));
    QCOMPARE(h3.value("title").toString(), QStringLiteral("Chats"));
    QCOMPARE(h3.value("count").toInt(), 2);
    // Recency-sorted within the group: newest (08-03) first.
    QCOMPARE(rowAt(model, 4).value("sessionId").toString(), QStringLiteral("s-chat-2"));
    QCOMPARE(rowAt(model, 5).value("sessionId").toString(), QStringLiteral("s-chat-1"));
}

void TstSessionListModel::collapseProjectsWrapperHidesProjects()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("s-proj-1"), QStringLiteral("In project"), QStringLiteral("2026-08-02"),
                QStringLiteral("p1")),
        session(QStringLiteral("s-chat-1"), QStringLiteral("Chat one"), QStringLiteral("2026-08-01")),
    });
    model.setProjects(QVariantList{project(QStringLiteral("p1"), QStringLiteral("Alpha"))});

    QCOMPARE(model.rowCount(), 5);   // Projects + p1 + s-proj-1 + Chats + s-chat-1
    model.toggleSection(QStringLiteral("projects"));
    // Collapsing "Projects" hides the whole project group; Chats remains.
    QCOMPARE(model.rowCount(), 3);
    QCOMPARE(rowAt(model, 0).value("header").toBool(), true);
    QCOMPARE(rowAt(model, 0).value("section").toString(), QStringLiteral("projects"));
    QCOMPARE(rowAt(model, 0).value("collapsed").toBool(), true);
    QCOMPARE(rowAt(model, 1).value("section").toString(), QStringLiteral("chats"));
    QCOMPARE(rowAt(model, 2).value("sessionId").toString(), QStringLiteral("s-chat-1"));

    // Expanding restores the project group.
    model.toggleSection(QStringLiteral("projects"));
    QCOMPARE(model.rowCount(), 5);
}

void TstSessionListModel::projectCollapseHidesOnlyItsSessions()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("s-a1"), QStringLiteral("A1"), QStringLiteral("2026-08-02"),
                QStringLiteral("pa")),
        session(QStringLiteral("s-b1"), QStringLiteral("B1"), QStringLiteral("2026-08-01"),
                QStringLiteral("pb")),
    });
    model.setProjects(QVariantList{project(QStringLiteral("pa"), QStringLiteral("A")),
                                   project(QStringLiteral("pb"), QStringLiteral("B"))});

    QCOMPARE(model.rowCount(), 5);   // Projects + A + s-a1 + B + s-b1
    model.toggleSection(QStringLiteral("proj:pa"));
    // Collapsing one project leaves the Projects wrapper and the other project.
    QCOMPARE(model.rowCount(), 4);
    QCOMPARE(rowAt(model, 1).value("section").toString(), QStringLiteral("proj:pa"));
    QCOMPARE(rowAt(model, 1).value("collapsed").toBool(), true);
    QCOMPARE(rowAt(model, 2).value("section").toString(), QStringLiteral("proj:pb"));
}

void TstSessionListModel::sortsByRecencyWithinGroup()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("old"), QStringLiteral("Old"), QStringLiteral("2026-08-01")),
        session(QStringLiteral("new"), QStringLiteral("New"), QStringLiteral("2026-08-09")),
    });
    QCOMPARE(model.rowCount(), 3);
    QCOMPARE(rowAt(model, 1).value("sessionId").toString(), QStringLiteral("new"));
    QCOMPARE(rowAt(model, 2).value("sessionId").toString(), QStringLiteral("old"));
}

void TstSessionListModel::collapseHidesSessions()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("s1"), QStringLiteral("One"), QStringLiteral("2026-08-01")),
        session(QStringLiteral("s2"), QStringLiteral("Two"), QStringLiteral("2026-08-02")),
    });
    QCOMPARE(model.rowCount(), 3);
    model.toggleSection(QStringLiteral("chats"));
    QCOMPARE(model.rowCount(), 1);   // header only
    QCOMPARE(rowAt(model, 0).value("collapsed").toBool(), true);
    model.toggleSection(QStringLiteral("chats"));
    QCOMPARE(model.rowCount(), 3);   // restored
    QCOMPARE(rowAt(model, 0).value("collapsed").toBool(), false);
}

void TstSessionListModel::cwdForFindsCwd()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("s1"), QStringLiteral("One"), QStringLiteral("2026-08-01"),
                QString(), QString(), QStringLiteral("/srv/work")),
    });
    QCOMPARE(model.cwdFor(QStringLiteral("s1")), QStringLiteral("/srv/work"));
    QCOMPARE(model.cwdFor(QStringLiteral("missing")), QString());
}

void TstSessionListModel::unknownProjectSortsLastAndFallsBackToId()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("s-known"), QStringLiteral("Known"), QStringLiteral("2026-08-01"),
                QStringLiteral("p1")),
        session(QStringLiteral("s-mystery"), QStringLiteral("Mystery"), QStringLiteral("2026-08-02"),
                QStringLiteral("no-such-project")),
    });
    model.setProjects(QVariantList{project(QStringLiteral("p1"), QStringLiteral("Alpha"))});
    // Both groups sit under the Projects wrapper; the known project first, the
    // unknown id after it, titled by its id.
    QCOMPARE(model.rowCount(), 5);
    QCOMPARE(rowAt(model, 0).value("section").toString(), QStringLiteral("projects"));
    QCOMPARE(rowAt(model, 1).value("section").toString(), QStringLiteral("proj:p1"));
    QCOMPARE(rowAt(model, 2).value("sessionId").toString(), QStringLiteral("s-known"));
    QCOMPARE(rowAt(model, 3).value("section").toString(), QStringLiteral("proj:no-such-project"));
    QCOMPARE(rowAt(model, 3).value("title").toString(), QStringLiteral("no-such-project"));
    QCOMPARE(rowAt(model, 4).value("sessionId").toString(), QStringLiteral("s-mystery"));
}

void TstSessionListModel::peerSessionsGroupLast()
{
    SessionListModel model;
    model.setSessions(QVariantList{
        session(QStringLiteral("roam:alice:abc"), QStringLiteral("Remote"), QStringLiteral("2026-08-01"),
                QString(), QStringLiteral("alice")),
        session(QStringLiteral("local"), QStringLiteral("Local"), QStringLiteral("2026-08-02")),
    });
    QCOMPARE(model.rowCount(), 4);
    QCOMPARE(rowAt(model, 0).value("section").toString(), QStringLiteral("chats"));
    QCOMPARE(rowAt(model, 2).value("section").toString(), QStringLiteral("peer:alice"));
    QCOMPARE(rowAt(model, 2).value("title").toString(), QStringLiteral("alice"));
    QCOMPARE(rowAt(model, 3).value("sessionId").toString(), QStringLiteral("roam:alice:abc"));
}

void TstSessionListModel::unchangedListDoesNotResetModel()
{
    SessionListModel model;
    const QVariantList sessions{QVariantList{
        session(QStringLiteral("s1"), QStringLiteral("One"), QStringLiteral("2026-08-01")),
    }};
    model.setSessions(sessions);
    QSignalSpy resetSpy(&model, &QAbstractItemModel::modelReset);
    model.setSessions(sessions);   // identical list: must be a no-op
    QCOMPARE(resetSpy.count(), 0);
    // Any field change (even just the snippet) invalidates the cache.
    QVariantList changed = sessions;
    QVariantMap tweaked = sessions.at(0).toMap();
    tweaked.insert(QStringLiteral("snippet"), QStringLiteral("x"));
    changed[0] = tweaked;
    model.setSessions(changed);
    QCOMPARE(resetSpy.count(), 1);
}

QTEST_MAIN(TstSessionListModel)
#include "tst_sessionlistmodel.moc"
