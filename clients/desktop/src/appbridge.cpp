// SPDX-License-Identifier: AGPL-3.0-or-later
#include "appbridge.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QHostAddress>
#include <QLocale>
#include <QRandomGenerator>
#include <QStringList>
#include <QTcpServer>
#include <QTcpSocket>
#include <QTimer>
#include <QUrl>

namespace {

// Per-connection request accumulator, parented to the socket so an abandoned
// connection frees it. `done` stops a late readyRead re-handling the request.
struct ReqBuf : public QObject {
    explicit ReqBuf(QObject *p = nullptr) : QObject(p) {}
    QByteArray data;
    bool done = false;
};

// The host shim, injected into <head> so its capture-phase listener exists
// before any app script runs. Why a shim is even needed: an app's scripts call
// window.parent.postMessage and wait for the HOST to answer. In a top-level
// browser tab parent === self, so every request ECHOES back to the app's own
// listener and either resolves with nothing (the button silently no-ops) or
// times out. The shim stops that echo (capture + stopImmediatePropagation on
// any message carrying a `method`), answers the documented lifecycle calls
// itself, and relays ui/message to us over HTTP. Replies are tagged
// __grouseHost so the shim lets them bubble to the app's real listeners.
QByteArray injectShim(const QString &html, const QString &token)
{
    QString shim = QString::fromLatin1(R"JS(<script>
(function(){
var TK=)JS");
    shim += "\"" + token + "\"" + QString::fromLatin1(R"JS(;
var ctx=null;
function host(o){o.__grouseHost=1;window.postMessage(o,'*');}
function reply(id,result){host({jsonrpc:'2.0',id:id,result:result||{}});}
function state(cb){
  if(ctx){cb(ctx);return;}
  fetch('/state/'+TK).then(function(r){return r.json();}).then(function(j){ctx=j;cb(ctx);})
  .catch(function(){});
}
window.addEventListener('message',function(ev){
  var m=ev.data;
  if(!m||m.__grouseHost||m.jsonrpc!=='2.0')return;
  if(!m.method)return;
  ev.stopImmediatePropagation();
  if(m.method==='ui/initialize'){
    state(function(c){
      reply(m.id,{hostContext:c.hostContext});
      if(c.toolInput!==undefined)host({jsonrpc:'2.0',method:'ui/notifications/tool-input',params:{arguments:c.toolInput}});
      if(c.toolResult!==undefined)host({jsonrpc:'2.0',method:'ui/notifications/tool-result',params:c.toolResult});
    });
    return;
  }
  if(m.method==='ui/message'){
    var parts=(m.params&&m.params.content)||[];var text='';
    for(var i=0;i<parts.length;i++)if(parts[i]&&parts[i].type==='text')text+=(text?"\n":"")+parts[i].text;
    fetch('/message/'+TK,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({text:text})})
      .then(function(){reply(m.id,{accepted:true});})
      .catch(function(){if(m.id!=null)reply(m.id,{accepted:false});});
    return;
  }
  if(m.method==='ui/open-link'){
    var u=(m.params&&m.params.url)||'';if(/^https?:/.test(u))window.open(u,'_blank','noopener');
    if(m.id!=null)reply(m.id,{});
    return;
  }
  if(String(m.method).indexOf('ui/notifications/')===0){
    if(m.method==='ui/notifications/size-changed'){
      var h=(m.params&&m.params.height)||0;
      if(h>0)document.documentElement.setAttribute('data-grouse-height',String(Math.round(h)));
    }
    return;
  }
  if(m.id!=null)host({jsonrpc:'2.0',id:m.id,error:{code:-32601,message:'Method not found'}});
},true);
})();
</script>)JS");

    int head = html.indexOf("<head>");
    int headCi = head < 0 ? html.indexOf("<HEAD>") : head;
    if (headCi >= 0)
        return (html.left(headCi + 6) + shim + html.mid(headCi + 6)).toUtf8();
    return (shim + html).toUtf8();
}

/// The server binds 127.0.0.1, but a listening socket alone is not an access
/// control: any page in the user's browser can request a loopback URL. Check the
/// Host the browser actually used (no DNS-rebinding aliases) and the Origin it
/// claims, when it supplies one. A raw HTTP client (the unit tests, a CLI) has
/// no Origin; a BROWSER always sends Sec-Fetch-Site, so require that one to say
/// same-origin/none when present.
bool isLoopbackHost(const QString &raw, quint16 port)
{
    const QString h = raw.trimmed();
    QString host = h;
    quint16 hostPort = 0;
    if (h.startsWith(QLatin1Char('['))) {           // [::1]:1234
        const int end = h.indexOf(QLatin1Char(']'));
        if (end < 0)
            return false;
        host = h.mid(1, end - 1);
        if (h.size() > end + 1) {
            if (h.at(end + 1) != QLatin1Char(':'))
                return false;
            bool ok = false;
            hostPort = quint16(h.mid(end + 2).toUInt(&ok));
            if (!ok)
                return false;
        }
    } else {
        // QUrl::host() hands back a bare IPv6 literal ("::1"), which must not
        // be split at its last colon as if it carried a port.
        const QHostAddress bare(h);
        if (!bare.isNull())
            return bare.isLoopback();
        const int colon = h.lastIndexOf(QLatin1Char(':'));
        if (colon >= 0) {
            bool ok = false;
            hostPort = quint16(h.mid(colon + 1).toUInt(&ok));
            if (!ok)
                return false;
            host = h.left(colon);
        }
    }
    if (hostPort != 0 && hostPort != port)
        return false;
    if (host.compare(QLatin1String("localhost"), Qt::CaseInsensitive) == 0)
        return true;
    const QHostAddress addr(host);
    return !addr.isNull() && addr.isLoopback();
}

bool isLoopbackOrigin(const QString &raw, quint16 port)
{
    const QUrl u(raw.trimmed());
    if (!u.isValid() || u.scheme() != QLatin1String("http")
        || (!u.host().isEmpty() && !isLoopbackHost(u.host(), 0)))
        return false;
    return u.port() == -1 || quint16(u.port()) == port;
}

/// Length-prefixing the session makes the scope unambiguous even if an app key
/// itself contains a separator. The session is the security boundary: a token
/// minted for one chat must never answer another chat's ui/message.
QString scopeKey(const QString &sessionId, const QString &appKey)
{
    return QString::number(sessionId.size()) + QLatin1Char(':') + sessionId + appKey;
}

} // namespace

AppBridgeServer::AppBridgeServer(QObject *parent)
    : QObject(parent)
{
}

bool AppBridgeServer::start()
{
    if (m_server)
        return true;
    m_server = new QTcpServer(this);
    connect(m_server, &QTcpServer::newConnection, this, &AppBridgeServer::onConnection);
    // Loopback only — and the flatpak shares the host's network namespace
    // (share=network), so the user's real browser can reach this.
    if (!m_server->listen(QHostAddress::LocalHost, 0)) {
        m_server->deleteLater();
        m_server = nullptr;
        return false;
    }
    return true;
}

bool AppBridgeServer::running() const
{
    return m_server && m_server->isListening();
}

quint16 AppBridgeServer::port() const
{
    return m_server ? m_server->serverPort() : 0;
}

QString AppBridgeServer::registerApp(const QString &appKey, const QString &sessionId,
                                     const QString &html, const QString &toolInput,
                                     const QString &toolResult, const QString &theme)
{
    pruneExpired();
    const QString scope = scopeKey(sessionId, appKey);
    // A refetched template must invalidate the URL handed to the browser: the
    // old page would otherwise keep a live capability into this chat forever.
    if (m_scopeTokens.contains(scope))
        revokeToken(m_scopeTokens.value(scope));

    // 128 bits of OS entropy, not a counter: the token is a capability on this
    // loopback HTTP surface, so a neighbouring tab must not guess the URL. The
    // same opaque string is baked into the shim and into every route it fetches.
    QByteArray tokenBytes(16, Qt::Uninitialized);
    for (int i = 0; i < tokenBytes.size(); ++i)
        tokenBytes[i] = static_cast<char>(QRandomGenerator::system()->bounded(0x100));
    const QString token = QString::fromLatin1(tokenBytes.toHex());

    Ctx ctx;
    ctx.sessionId = sessionId;
    ctx.html = html;
    ctx.toolInput = toolInput;
    ctx.toolResult = toolResult;
    ctx.theme = theme;
    ctx.lastSeen.start();
    m_apps.insert(token, ctx);
    m_scopeTokens.insert(scope, token);
    return token;
}

void AppBridgeServer::revokeToken(const QString &token)
{
    if (!m_apps.remove(token))
        return;
    for (auto it = m_scopeTokens.begin(); it != m_scopeTokens.end();) {
        if (it.value() == token)
            it = m_scopeTokens.erase(it);
        else
            ++it;
    }
}

bool AppBridgeServer::isTokenValid(const QString &token) const
{
    auto it = m_apps.constFind(token);
    return it != m_apps.cend() && it->lastSeen.elapsed() <= m_tokenTtlMs;
}

void AppBridgeServer::setTokenTtlMs(qint64 ms)
{
    m_tokenTtlMs = qMax<qint64>(1, ms);
}

int AppBridgeServer::activeTokenCount() const
{
    return m_apps.size();
}

void AppBridgeServer::pruneExpired()
{
    QStringList stale;
    for (auto it = m_apps.constBegin(); it != m_apps.constEnd(); ++it) {
        if (it->lastSeen.elapsed() > m_tokenTtlMs)
            stale << it.key();
    }
    for (const QString &token : stale)
        revokeToken(token);
}

QString AppBridgeServer::urlFor(const QString &token) const
{
    if (!m_server)
        return {};
    return QStringLiteral("http://127.0.0.1:%1/page/%2").arg(port()).arg(token);
}

void AppBridgeServer::onConnection()
{
    while (m_server->hasPendingConnections()) {
        QTcpSocket *sock = m_server->nextPendingConnection();
        // Accumulate until the request is complete (headers, then a POST body of
        // Content-Length bytes) — a single readyRead is not guaranteed to carry it
        // all. A ReqBuf parented to the socket owns the buffer's lifetime, so an
        // abandoned connection can neither leak nor double-free it.
        auto *buf = new ReqBuf(sock);
        connect(sock, &QTcpSocket::readyRead, sock, [this, sock, buf] {
            if (buf->done)
                return;
            buf->data += sock->readAll();
            const int hdrEnd = buf->data.indexOf("\r\n\r\n");
            if (hdrEnd < 0)
                return;                                  // headers still arriving
            int contentLen = 0;
            for (const QByteArray &line : buf->data.left(hdrEnd).split('\n')) {
                if (line.toLower().startsWith("content-length:"))
                    contentLen = line.mid(15).trimmed().toInt();
            }
            if (buf->data.size() < hdrEnd + 4 + contentLen)
                return;                                  // body still arriving
            buf->done = true;
            handleRequest(sock, buf->data);
        });
        connect(sock, &QTcpSocket::disconnected, sock, &QTcpSocket::deleteLater);
    }
}

void AppBridgeServer::respond(QTcpSocket *sock, int code, const QByteArray &type, const QByteArray &body)
{
    QByteArray head = "HTTP/1.1 " + QByteArray::number(code)
        + (code == 200 ? " OK" : " Err") + "\r\n"
        + "Content-Type: " + type + "\r\n"
        + "Content-Length: " + QByteArray::number(body.size()) + "\r\n"
        + "Cache-Control: no-store\r\n"
        + "Connection: close\r\n\r\n";
    sock->write(head + body);
    sock->flush();
    // close only AFTER Qt reports the bytes written. waitForBytesWritten(3000)
    // blocked the GUI thread for up to 3 s on a browser that stopped reading;
    // bytesWritten is the same FIN-race fix without ever waiting synchronously.
    if (sock->bytesToWrite() == 0) {
        QTimer::singleShot(0, sock, [sock] { sock->disconnectFromHost(); });
        return;
    }
    QObject::connect(sock, &QTcpSocket::bytesWritten, sock, [sock] {
        if (sock->bytesToWrite() == 0)
            sock->disconnectFromHost();
    });
    // A browser that never drains the socket must not pin a connection (or a
    // socket object) forever. Still asynchronous: the GUI thread never waits.
    QTimer::singleShot(5000, sock, [sock] { sock->disconnectFromHost(); });
}

void AppBridgeServer::handleRequest(QTcpSocket *sock, const QByteArray &all)
{
    const int hdrEnd = all.indexOf("\r\n\r\n");
    if (hdrEnd < 0) {                       // headers incomplete; this mini server
        respond(sock, 400, "text/plain", "bad request");  // only ever gets small ones
        return;
    }
    const QByteArray reqHead = all.left(hdrEnd);
    const QByteArray body = all.mid(hdrEnd + 4);
    const QList<QByteArray> lines = reqHead.split('\n');
    if (lines.isEmpty()) {
        respond(sock, 400, "text/plain", "bad request");
        return;
    }
    const QList<QByteArray> first = lines.first().trimmed().split(' ');
    const QByteArray method = first.value(0);
    const QByteArray path = first.value(1);

    QHash<QByteArray, QByteArray> headers;
    for (int i = 1; i < lines.size(); ++i) {
        const int colon = lines.at(i).indexOf(':');
        if (colon > 0)
            headers.insert(lines.at(i).left(colon).trimmed().toLower(),
                           lines.at(i).mid(colon + 1).trimmed());
    }
    // The token is a capability, not a substitute for checking WHO is asking.
    // Host is mandatory; Origin/Sec-Fetch-Site are browser-supplied and checked
    // when present. Rejecting a cross-origin site here means a malicious local
    // tab cannot inject a prompt into the user's chat even if it learns a token.
    if (!isLoopbackHost(QString::fromLatin1(headers.value("host")), port())) {
        respond(sock, 403, "text/plain", "loopback host required");
        return;
    }
    if (headers.contains("origin")
        && !isLoopbackOrigin(QString::fromLatin1(headers.value("origin")), port())) {
        respond(sock, 403, "text/plain", "loopback origin required");
        return;
    }
    const QByteArray fetchSite = headers.value("sec-fetch-site").toLower();
    if (!fetchSite.isEmpty() && fetchSite != "same-origin" && fetchSite != "none") {
        respond(sock, 403, "text/plain", "same-origin requests only");
        return;
    }
    if (method == "POST") {
        // A cross-site <form> cannot set application/json without a CORS
        // preflight — which this server never answers favourably. Enforce the
        // same rule server-side so a raw client cannot slip text/plain through.
        if (!headers.value("content-type").toLower().startsWith("application/json")) {
            respond(sock, 415, "text/plain", "application/json required");
            return;
        }
    }

    // token = last path segment. Checked only AFTER the request-level origin
    // checks: a rejected probe must not slide a stolen token's TTL.
    const int slash = path.lastIndexOf('/');
    const QString token = QString::fromUtf8(path.mid(slash + 1));
    auto it = m_apps.find(token);
    if (it == m_apps.end() || it->lastSeen.elapsed() > m_tokenTtlMs) {
        if (it != m_apps.end())
            revokeToken(token);
        respond(sock, 404, "text/plain", "no such app");
        return;
    }
    it->lastSeen.restart();

    if (path.startsWith("/page/") && method == "GET") {
        QByteArray html = injectShim(it->html, token);
        respond(sock, 200, "text/html; charset=utf-8", html);
        return;
    }
    if (path.startsWith("/state/") && method == "GET") {
        QJsonObject o;
        QJsonObject hc;
        hc["theme"] = it->theme;                 // "dark" | "light" — app applies immediately
        hc["displayMode"] = QStringLiteral("inline");
        hc["platform"] = QStringLiteral("web");
        hc["locale"] = QLocale::system().name();
        o["hostContext"] = hc;
        if (!it->toolInput.isEmpty()) {
            const QJsonDocument doc = QJsonDocument::fromJson(it->toolInput.toUtf8());
            o["toolInput"] = doc.isObject() ? QJsonValue(doc.object()) : QJsonValue(it->toolInput);
        }
        if (!it->toolResult.isEmpty()) {
            QJsonObject res;
            res["content"] = QJsonArray{QJsonObject{{"type", "text"}, {"text", it->toolResult}}};
            o["toolResult"] = res;
        }
        respond(sock, 200, "application/json", QJsonDocument(o).toJson(QJsonDocument::Compact));
        return;
    }
    if (path.startsWith("/message/") && method == "POST") {
        const QJsonObject o = QJsonDocument::fromJson(body).object();
        const QString text = o.value("text").toString();
        if (!text.isEmpty())
            emit appMessage(it->sessionId, text);
        respond(sock, 200, "application/json", "{\"ok\":true}");
        return;
    }
    respond(sock, 404, "text/plain", "not found");
}
