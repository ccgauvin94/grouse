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
    }

    QVariantList recipes() const { return m_recipes; }
    QVariantList schedules() const { return m_schedules; }
    QVariantList config() const { return m_config; }
    int refreshCount() const { return m_refreshCount; }
    QString lastCall() const { return m_lastCall; }

    Q_INVOKABLE void refreshRecipes() { ++m_refreshCount; emit refreshCountChanged(); }
    Q_INVOKABLE void runRecipe(const QString &id) { record(QStringLiteral("runRecipe:%1").arg(id)); }
    Q_INVOKABLE void runScheduleNow(const QString &id) { record(QStringLiteral("runScheduleNow:%1").arg(id)); }
    Q_INVOKABLE void setSchedulePaused(const QString &id, bool paused)
    { record(QStringLiteral("setSchedulePaused:%1:%2").arg(id).arg(paused)); }
    Q_INVOKABLE void scheduleRecipe(const QString &id, const QString &cron)
    { record(QStringLiteral("scheduleRecipe:%1:%2").arg(id).arg(cron)); }
    Q_INVOKABLE void saveRecipe(const QString &id, const QString &dto)
    { record(QStringLiteral("saveRecipe:%1").arg(id)); Q_UNUSED(dto); }
    Q_INVOKABLE void deleteRecipe(const QString &id) { record(QStringLiteral("deleteRecipe:%1").arg(id)); }

signals:
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
    }
};

QUICK_TEST_MAIN_WITH_SETUP(qmltests, Setup)

// Q_OBJECT classes live in this .cpp — AUTOMOC requires the generated moc.
#include "tst_qml.moc"
