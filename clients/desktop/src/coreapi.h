// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <QByteArray>
#include <QElapsedTimer>
#include <QJsonObject>
#include <QScopeGuard>
#include <QSet>
#include <QString>
#include <QVariantList>
#include <QVariantMap>

#include <type_traits>

#include "corebridge.h"

class Manager;

// A main-thread phase that runs long enough to be felt. Kept next to the timing
// helpers it feeds; the class that uses it supplies a `timingLog(const QString&)`
// member (Manager forwards to CoreApi, the extracted controllers forward likewise).
#define GROUSE_PHASE(name) \
    QElapsedTimer _phaseTimer; _phaseTimer.start(); \
    const auto _phaseGuard = qScopeGuard([this, _phaseTimer]() { \
        const qint64 _ms = _phaseTimer.elapsed(); \
        if (_ms >= 20) timingLog(QStringLiteral(name " %1ms").arg(_ms)); \
    })

/** The C-ABI boundary's mechanical work: symbol-shape shims, malloc'd
 *  error/string pointers, JSON shape helpers and the open-timing log.
 *
 *  No QML-visible state lives here; Manager and the extracted controllers call
 *  these statics so one ABI spelling (and one error-pointer rule) is shared.
 */
class CoreApi
{
public:
    static QString timingLogPath();
    static void timingLog(const QString &line);

    // ABI 20 moved the request id to the FRONT of every respond intent and added
    // the roam pin getters. The if-constexpr shims keep this translation unit
    // compilable while the C ABI header lands in the same wave; an ABI-20 .so is
    // required at runtime, so the legacy branch is never reached in production.
    template <typename Fn>
    static void respondRecipeIntent(Fn fn, void *h, const QByteArray &rid,
                                    const QByteArray &action, const QByteArray &payload)
    {
        if constexpr (std::is_invocable_v<Fn, void *, const char *, const char *, const char *>)
            fn(h, rid.constData(), action.constData(), payload.constData());
        else
            fn(h, action.constData(), payload.constData());
    }

    template <typename Fn>
    static void respondElicitationIntent(Fn fn, void *h, const QByteArray &rid,
                                         const QByteArray &action, const QByteArray &payload)
    {
        if constexpr (std::is_invocable_v<Fn, void *, const char *, const char *, const char *>)
            fn(h, rid.constData(), action.constData(), payload.constData());
        else
            fn(h, action.constData(), payload.constData());
    }

    // A getter returning a MALLOC'd JSON string. Optional label for the singular
    // pin getter; the plural one takes no label.
    template <typename Fn>
    static QString pinGetter(CoreBridge *bridge, Fn fn, void *h, const QByteArray *label)
    {
        if constexpr (std::is_invocable_v<Fn, void *, const char *, char **>) {
            if (!label)
                return QString();
            char *err = nullptr;
            const QString out = bridge->takeString(fn(h, label->constData(), &err));
            if (err) {
                bridge->takeString(err);
                return QString();
            }
            return out;
        } else if constexpr (std::is_invocable_v<Fn, void *, const char *>) {
            if (!label)
                return QString();
            return bridge->takeString(fn(h, label->constData()));
        } else if constexpr (std::is_invocable_v<Fn, void *, char **>) {
            Q_UNUSED(label);
            char *err = nullptr;
            const QString out = bridge->takeString(fn(h, &err));
            if (err) {
                bridge->takeString(err);
                return QString();
            }
            return out;
        } else {
            Q_UNUSED(label);
            return bridge->takeString(fn(h));
        }
    }

    template <typename Fn>
    static bool pinClearIntent(CoreBridge *bridge, Fn fn, void *h, const QByteArray &label,
                               QString *error)
    {
        if constexpr (std::is_invocable_v<Fn, void *, const char *, char **>) {
            char *err = nullptr;
            using R = std::invoke_result_t<Fn, void *, const char *, char **>;
            if constexpr (std::is_same_v<R, char *>)
                bridge->takeString(fn(h, label.constData(), &err));
            else
                fn(h, label.constData(), &err);
            if (err) {
                if (error)
                    *error = bridge->takeString(err);
                return false;
            }
            return true;
        } else if constexpr (std::is_invocable_v<Fn, void *, const char *>) {
            using R = std::invoke_result_t<Fn, void *, const char *>;
            if constexpr (std::is_same_v<R, char *>)
                bridge->takeString(fn(h, label.constData()));
            else
                fn(h, label.constData());
            return true;
        } else {
            return false;
        }
    }

    /** Copy and free a malloc'd error pointer from the C ABI. */
    static QString takeError(CoreBridge *bridge, char *err)
    {
        if (!err)
            return QString();
        const QString text = bridge->takeString(err);
        return text;
    }

    static QVariantMap pinMapFromJson(const QString &raw);
    static QString pinStringFromJson(const QString &raw);

    static QVariantMap parsePermissionRequest(const QString &json);
    static int requestQueueIndex(const QVariantList &queue, const QString &requestId);
    static QString requestOwnerLabel(const QVariant &owner, const QString &sessionId);
    static QVariantList formatCardPins(const QVariantMap &pins);
    static QVariantMap parseCardPinRefusal(const QString &status,
                                           const QString &peerLabel = QString());

    /** serde Prompt JSON (`grouse_send_prompt`) and ServerConfig JSON (`grouse_connect`). */
    static QString promptJson(const QString &text, const QVariantList &blocks);
    static QString serverConfigJson(Manager &manager);

    /** Keep the METHOD in the log: the UI shows only the human message. */
    static void reportError(const QString &method, const QString &message);
};
