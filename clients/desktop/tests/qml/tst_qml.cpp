// QtQuickTest runner entry point. The actual tests live in tst_*.qml files in
// this directory (QUICK_TEST_SOURCE_DIR), e.g. tst_chartbubble.qml.
//
// Setup injects a context property `Mgr` (a stub FakeGoose) into every test's
// QQML context: the app's dialogs reference Mgr unqualified, and dialogs that
// create delegates/panels lazily (RecipesDialog) need real-shaped data to
// exercise those paths — a binding TypeError at delegate instantiation is the
// class of bug static lint cannot see.

#include <QtQuickTest/quicktest.h>

#include <QQmlContext>
#include <QQmlEngine>
#include <QtQml>
#include <QVariantList>
#include <QVariantMap>

class FakeGoose : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QVariantList recipes READ recipes NOTIFY recipesChanged)
    Q_PROPERTY(QVariantList schedules READ schedules NOTIFY schedulesChanged)
    Q_PROPERTY(QVariantList config READ config NOTIFY configChanged)
    Q_PROPERTY(int refreshCount READ refreshCount NOTIFY refreshCountChanged)
    Q_PROPERTY(QString lastCall READ lastCall NOTIFY lastCallChanged)
    // My Day (CONTRACT §3.6). The page groups and paints these; the real formatters
    // are C++ (src/myday.cpp) and pinned by tst_myday, so the stub here answers with
    // the SHAPE the page expects and nothing more.
    Q_PROPERTY(bool myDayVisible READ myDayVisible NOTIFY myDayVisibleChanged)
    Q_PROPERTY(QVariantList myDayOpen READ myDayOpen NOTIFY tasksChanged)
    Q_PROPERTY(QVariantList myDayDone READ myDayDone NOTIFY tasksChanged)
    Q_PROPERTY(QVariantList myDayHanded READ myDayHanded NOTIFY tasksChanged)
    Q_PROPERTY(QString tasksError READ tasksError NOTIFY tasksChanged)
    Q_PROPERTY(bool tasksLoading READ tasksLoading NOTIFY tasksChanged)
    Q_PROPERTY(QString currentThreadLabel READ currentThreadLabel NOTIFY threadLabelChanged)
public:
    explicit FakeGoose(QObject *parent = nullptr) : QObject(parent)
    {
        QVariantMap r;
        r[QStringLiteral("id")] = QStringLiteral("r1");
        // schedule_cron deliberately EMPTY: the cron lives on the matched job
        // (the production shape that made the picker open blank).
        r[QStringLiteral("file_path")] = QStringLiteral("/x/daily.md");
        r[QStringLiteral("schedule_cron")] = QString();
        QVariantMap recipe;
        recipe[QStringLiteral("title")] = QStringLiteral("Daily Digest");
        recipe[QStringLiteral("description")] = QStringLiteral("deliver");
        recipe[QStringLiteral("prompt")] = QStringLiteral("hi");
        recipe[QStringLiteral("instructions")] = QStringLiteral("be brief");
        QVariantMap settings;
        settings[QStringLiteral("goose_model")] = QStringLiteral("m1");
        settings[QStringLiteral("goose_provider")] = QStringLiteral("meta");
        recipe[QStringLiteral("settings")] = settings;
        r[QStringLiteral("recipe")] = recipe;
        m_recipes << r;

        QVariantMap job;
        job[QStringLiteral("id")] = QStringLiteral("j1");
        job[QStringLiteral("cron")] = QStringLiteral("0 0 6 * * *");
        // Library-prefixed source matching the recipe's file_path by suffix.
        job[QStringLiteral("source")] = QStringLiteral("/lib/x/daily.md");
        job[QStringLiteral("paused")] = false;
        job[QStringLiteral("currentlyRunning")] = false;
        job[QStringLiteral("lastRun")] = QString();
        m_schedules << job;

        QVariantMap prov;
        prov[QStringLiteral("id")] = QStringLiteral("provider");
        prov[QStringLiteral("currentValue")] = QStringLiteral("meta");
        QVariantList provChoices;
        QVariantMap openai;
        openai[QStringLiteral("value")] = QStringLiteral("openai");
        openai[QStringLiteral("name")] = QStringLiteral("OpenAI");
        provChoices << openai;
        // In the catalog but NOT configured: configured-only must hide it.
        QVariantMap anthropic;
        anthropic[QStringLiteral("value")] = QStringLiteral("anthropic");
        anthropic[QStringLiteral("name")] = QStringLiteral("Anthropic");
        provChoices << anthropic;
        prov[QStringLiteral("choices")] = provChoices;
        m_config << prov;
        QVariantMap model;
        model[QStringLiteral("id")] = QStringLiteral("model");
        model[QStringLiteral("currentValue")] = QStringLiteral("gpt");
        QVariantList modelChoices;
        QVariantMap gpt;
        gpt[QStringLiteral("value")] = QStringLiteral("gpt");
        gpt[QStringLiteral("name")] = QStringLiteral("GPT");
        modelChoices << gpt;
        model[QStringLiteral("choices")] = modelChoices;
        m_config << model;

        setMyDay();
    }

    // A thread with two entries (so it gets a header), an unlabelled item (which
    // never does), and one handed-over task in the same thread.
    void setMyDay() {
        m_myDayOpen.clear();
        m_myDayDone.clear();
        m_myDayHanded.clear();
        QVariantMap a;
        a["id"] = "itm_a";
        a["title"] = "Renew the domain";
        a["thread"] = "chami-billing";
        a["kind"] = "commitment";
        a["state"] = "open";
        a["dueAt"] = "2026-11-06";
        m_myDayOpen << a;
        QVariantMap b;
        b["id"] = "itm_b";
        b["title"] = "Call the insurer";
        b["thread"] = "chami-billing";
        b["state"] = "open";
        m_myDayOpen << b;
        QVariantMap lone;
        lone["id"] = "itm_c";
        lone["title"] = "A lone loop";
        lone["thread"] = "";
        lone["state"] = "open";
        m_myDayOpen << lone;
        QVariantMap t;
        t["id"] = "tsk_a";
        t["title"] = "Draft the deck";
        t["state"] = "review";
        t["thread"] = "chami-billing";
        t["sessionId"] = "20261030_1";
        t["result"] = "wrote 9 slides";
        t["artifacts"] = QStringList{"/workspace/deck.pdf"};
        m_myDayHanded << t;
        QVariantMap d;
        d["id"] = "itm_d";
        d["title"] = "Book the flight";
        d["state"] = "done";
        d["resolution"] = "booked it";
        m_myDayDone << d;
        emit tasksChanged();
    }

    bool myDayVisible() const { return true; }
    QVariantList myDayOpen() const { return m_myDayOpen; }
    QVariantList myDayDone() const { return m_myDayDone; }
    QVariantList myDayHanded() const { return m_myDayHanded; }
    QString tasksError() const { return m_tasksError; }
    bool tasksLoading() const { return false; }
    QString currentThreadLabel() const { return QString(); }

    QVariantList recipes() const { return m_recipes; }
    QVariantList schedules() const { return m_schedules; }
    QVariantList config() const { return m_config; }
    int refreshCount() const { return m_refreshCount; }
    QString lastCall() const { return m_lastCall; }

    Q_INVOKABLE void refreshRecipes() { ++m_refreshCount; emit refreshCountChanged(); }

    // ---- My Day surface the page calls ------------------------------------
    Q_INVOKABLE void refreshTasks() { record(QStringLiteral("refreshTasks")); }
    Q_INVOKABLE void closeMyDay() { record(QStringLiteral("closeMyDay")); }
    Q_INVOKABLE QString threadName(const QString &label) const
    {
        QString w = label;
        w.replace(QLatin1Char('-'), QLatin1Char(' '));
        return w.isEmpty() ? w : w.left(1).toUpper() + w.mid(1);
    }
    Q_INVOKABLE QString artifactsLabel(const QStringList &paths) const
    {
        return paths.isEmpty() ? QString() : paths.first().section(QLatin1Char('/'), -1);
    }
    Q_INVOKABLE bool rowSnoozed(const QVariantMap &row) const
    { return row.value(QStringLiteral("state")).toString() == QLatin1String("snoozed"); }
    Q_INVOKABLE QString rowMeta(const QVariantMap &row, bool snoozed) const
    {
        Q_UNUSED(snoozed);
        return row.value(QStringLiteral("dueAt")).toString();
    }
    Q_INVOKABLE void openThreadChat(const QString &label)
    { record(QStringLiteral("openThreadChat:%1").arg(label)); }
    Q_INVOKABLE void openAssistant() { record(QStringLiteral("openAssistant")); }
    Q_INVOKABLE void setDraft(const QString &text) { record(QStringLiteral("setDraft:%1").arg(text)); }
    Q_INVOKABLE void snoozeTaskToMorning(const QString &id)
    { record(QStringLiteral("snooze:%1").arg(id)); }
    Q_INVOKABLE void wakeTask(const QString &id) { record(QStringLiteral("wake:%1").arg(id)); }
    Q_INVOKABLE void closeTask(const QString &id, const QString &state, const QString &note)
    { record(QStringLiteral("closeTask:%1:%2:%3").arg(id, state, note)); }
    Q_INVOKABLE void askAssistant(const QString &text)
    { record(QStringLiteral("askAssistant:%1").arg(text)); }
    Q_INVOKABLE void openTaskSession(const QString &sessionId, const QString &thread)
    { record(QStringLiteral("openTaskSession:%1:%2").arg(sessionId, thread)); }
    Q_INVOKABLE void runRecipe(const QString &id) { record(QStringLiteral("runRecipe:%1").arg(id)); }
    Q_INVOKABLE void runScheduleNow(const QString &id) { record(QStringLiteral("runScheduleNow:%1").arg(id)); }
    Q_INVOKABLE void setSchedulePaused(const QString &id, bool paused)
    { record(QStringLiteral("setSchedulePaused:%1:%2").arg(id).arg(paused)); }
    Q_INVOKABLE void scheduleRecipe(const QString &id, const QString &cron)
    { record(QStringLiteral("scheduleRecipe:%1:%2").arg(id).arg(cron)); }
    Q_INVOKABLE void saveRecipe(const QString &id, const QString &dto)
    { record(QStringLiteral("saveRecipe:%1").arg(id)); Q_UNUSED(dto); }
    Q_INVOKABLE void deleteRecipe(const QString &id) { record(QStringLiteral("deleteRecipe:%1").arg(id)); }
    // Stand-in for Manager::filterConfiguredProviders: configured-only with a
    // fixed inventory, so the QML wiring (not the rule) is what these tests cover.
    Q_INVOKABLE QVariantList filterConfiguredProviders(const QVariantList &choices,
                                                       const QString &current) const
    {
        QVariantList keep;
        for (const QVariant &c : choices) {
            const QString v = c.toMap().value(QStringLiteral("value")).toString();
            if (v == current || v == QStringLiteral("openai"))
                keep.append(c);
        }
        return keep.isEmpty() ? choices : keep;
    }

signals:
    void myDayVisibleChanged();
    void tasksChanged();
    void threadLabelChanged();
    void recipesChanged();
    void schedulesChanged();
    void configChanged();
    void refreshCountChanged();
    void lastCallChanged();

private:
    void record(const QString &call) { m_lastCall = call; emit lastCallChanged(); }
    QVariantList m_recipes;
    QVariantList m_schedules;
    QVariantList m_config;
    QVariantList m_myDayOpen;
    QVariantList m_myDayDone;
    QVariantList m_myDayHanded;
    QString m_tasksError;
    int m_refreshCount = 0;
    QString m_lastCall;
};

class Setup : public QObject
{
    Q_OBJECT
public slots:
    // QuickTest (6.10) probes the setup object for this exact signature and
    // calls it per test with its engine. (The older qmlContext(QQmlContext*)
    // hook no longer exists — verified against the lib's indexOfMethod
    // literals.) The app's dialogs reference Mgr as a context property, so
    // inject the stub here.
    void qmlEngineAvailable(QQmlEngine *engine)
    {
        engine->rootContext()->setContextProperty(QStringLiteral("Mgr"), new FakeGoose(engine));
#ifdef GROUSE_THEME_URL
        // main.cpp registers this from the qrc; a test has no qrc, so register the
        // same FILE. Pages that consume the design tokens (`import Grouse 1.0`)
        // refuse to load without it.
        qmlRegisterSingletonType(QUrl(QStringLiteral(GROUSE_THEME_URL)), "Grouse", 1, 0, "Theme");
#endif
    }
};

QUICK_TEST_MAIN_WITH_SETUP(qmltests, Setup)

// Q_OBJECT classes live in this .cpp — AUTOMOC requires the generated moc.
#include "tst_qml.moc"
