// SPDX-License-Identifier: AGPL-3.0-or-later
#include "appbridge.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QLocale>
#include <QTcpServer>
#include <QTcpSocket>

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
  if(String(m.method).indexOf('ui/notifications/')===0)return;
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

void AppBridgeServer::registerApp(const QString &token, const QString &sessionId,
                                  const QString &html, const QString &toolInput,
                                  const QString &toolResult, const QString &theme)
{
    m_apps.insert(token, Ctx{sessionId, html, toolInput, toolResult, theme});
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
    sock->write(head);
    sock->write(body);
    sock->flush();
    // Give the client a chance to read before closing, or the response can be
    // lost in the FIN race (observed: empty reads under waitForReadyRead).
    sock->waitForBytesWritten(3000);
    sock->disconnectFromHost();
    sock->deleteLater();
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

    // token = last path segment
    const int slash = path.lastIndexOf('/');
    const QString token = QString::fromUtf8(path.mid(slash + 1));
    auto it = m_apps.find(token);
    if (it == m_apps.end()) {
        respond(sock, 404, "text/plain", "no such app");
        return;
    }

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
