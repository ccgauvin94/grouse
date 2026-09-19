// SPDX-License-Identifier: AGPL-3.0-or-later
#pragma once

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
/// Per-app tokens (opaque path segments) scope every route; nothing is served
/// without one. The page and its shim are server-supplied content — treated as
/// untrusted, isolated by the browser's own origin sandbox.
class AppBridgeServer : public QObject
{
    Q_OBJECT
public:
    explicit AppBridgeServer(QObject *parent = nullptr);

    /// Bind 127.0.0.1 on an ephemeral port. False on failure (the caller then
    /// falls back to plain file handoff).
    bool start();
    bool running() const;
    /// Ephemeral loopback port (0 before start()).
    quint16 port() const;

    /// Register (or replace) the app an open tab is serving. toolInput/toolResult
    /// are replayed to the app as ui/notifications/tool-input|tool-result, which
    /// the autovisualiser-style bridges wait for.
    void registerApp(const QString &token, const QString &sessionId, const QString &html,
                     const QString &toolInput, const QString &toolResult, const QString &theme);
    QString urlFor(const QString &token) const;

signals:
    /// A `ui/message` arrived from the app: text to place into `sessionId`'s chat.
    void appMessage(const QString &sessionId, const QString &text);

private slots:
    void onConnection();

private:
    struct Ctx { QString sessionId, html, toolInput, toolResult, theme; };
    void handleRequest(QTcpSocket *sock, const QByteArray &all);
    void respond(QTcpSocket *sock, int code, const QByteArray &type, const QByteArray &body);

    QTcpServer *m_server = nullptr;
    QHash<QString, Ctx> m_apps;   // token -> context
};
