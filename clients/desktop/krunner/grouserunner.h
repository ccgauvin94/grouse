#pragma once

#include <KRunner/AbstractRunner>

class GrouseRunner : public KRunner::AbstractRunner
{
    Q_OBJECT
public:
    explicit GrouseRunner(QObject *parent, const KPluginMetaData &metaData);

    void match(KRunner::RunnerContext &context) override;
    void run(const KRunner::RunnerContext &context, const KRunner::QueryMatch &match) override;

private:
    void launchGrouse();
};
