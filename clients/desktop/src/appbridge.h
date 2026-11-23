// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

#include <QElapsedTimer>
#include <QHash>
#include <QObject>
#include <QString>

class QTcpServer;
class QTcpSocket;

/// Loopback host for MCP Apps on the desktop. The org.kde.Platform flatpak
/// runtime ships no QtWebEngine, so an app template (a full HTML+JS page that
/// speaks JSON-RPC over postMessage to its iframe parent) cannot render
/// in-page. But it CAN run in the user's own browser — if something plays the
/// parent. This is that something: a tiny 127.0.0.1 HTTP server that serves
/// the template with a host shim injected, answers the app's `ui/initialize`
/// (theme/display context) and delivers `ui/message` back into the chat the
/// app came from — exactly what Android's WebView host page does locally.
///
/// Per-app tokens (128 bits of OS entropy, opaque path segments) scope every
/// route; nothing is served without one. Host must be loopback, any browser-
/// supplied Origin/Sec-Fetch-Site must agree, and POSTs must be JSON — a token
/// alone is not an access-control decision. Tokens are also bound to the
/// (session, app) pair that registered them: two chats showing the same app can
/// never deliver each other's `ui/message`, and a token expires after a sliding
/// idle TTL or can be revoked outright when its template changes. The page and
/// its shim are server-supplied content — treated as untrusted, isolated by the
/// browser's own origin sandbox.
class AppBridgeServer : public QObject
{
    Q_OBJECT
public:
    explicit AppBridgeServer(QObject *parent = nullptr);

    /// Bind 127.0.0.1 on an ephemeral port. False on failure (the caller then
    /// opens nothing — there is deliberately no file:// handoff for untrusted
    /// server HTML).
    bool start();
    bool running() const;
    /// Ephemeral loopback port (0 before start()).
    quint16 port() const;

    /// Register (or replace) the app an open tab is serving. Returns a fresh
    /// 128-bit opaque token; any older token for the SAME (sessionId, appKey)
    /// scope is revoked, while other chats' tokens for the same app stay live.
    /// toolInput/toolResult are replayed to the app as
    /// ui/notifications/tool-input|tool-result, which the autovisualiser-style
    /// bridges wait for.
    QString registerApp(const QString &appKey, const QString &sessionId, const QString &html,
                        const QString &toolInput, const QString &toolResult, const QString &theme);
    /// Forget a token immediately (template refetch, invisible view, ...).
    void revokeToken(const QString &token);
    /// False once a token was revoked or its idle TTL elapsed. Lets the owner
    /// mint a fresh URL for an expired token instead of handing out a dead one.
    bool isTokenValid(const QString &token) const;
    /// Idle TTL in ms; every successful request slides it. The seam exists for
    /// tests — the default is 30 minutes, long enough for an open dashboard.
    void setTokenTtlMs(qint64 ms);
    /// Live tokens only (expired ones are pruned on register/request).
    int activeTokenCount() const;

    QString urlFor(const QString &token) const;

signals:
    /// A `ui/message` arrived from the app: text to place into `sessionId`'s chat.
    void appMessage(const QString &sessionId, const QString &text);

private slots:
    void onConnection();

private:
    struct Ctx {
        QString sessionId, html, toolInput, toolResult, theme;
        QElapsedTimer lastSeen;
    };
    void handleRequest(QTcpSocket *sock, const QByteArray &all);
    void respond(QTcpSocket *sock, int code, const QByteArray &type, const QByteArray &body);
    void pruneExpired();

    QTcpServer *m_server = nullptr;
    QHash<QString, Ctx> m_apps;      // token -> context
    QHash<QString, QString> m_scopeTokens; // length-prefixed session/app scope -> token
    qint64 m_tokenTtlMs = 30 * 60 * 1000;
};
