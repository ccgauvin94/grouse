#include "acpclient.h"
#include "acptransport.h"
#include "roamtransport.h"
#include "websockettransport.h"

#include <QJsonArray>
#include <QJsonDocument>
#include <QUrl>

// --- Qt type registration (needed for the struct metatypes used in lists) ---
namespace {
struct Registrar {
    Registrar() {
        qRegisterMetaType<ConfigChoice>();
        qRegisterMetaType<ConfigOption>();
        qRegisterMetaType<SessionInfo>();
    }
} registrar;

QJsonValue jv(const QJsonObject &o, const QString &key)
{
    return o.value(key);
}

QString str(const QJsonObject &o, const QString &key)
{
    return o.value(key).toString();
}

QJsonObject obj(const QJsonObject &o, const QString &key)
{
    return o.value(key).toObject();
}

QJsonArray arr(const QJsonObject &o, const QString &key)
{
    return o.value(key).toArray();
}

QVariantMap choiceToMap(const QJsonValue &v)
{
    const QJsonObject c = v.toObject();
    return {{"value", str(c, "value")}, {"name", str(c, "name")}};
}
} // namespace

AcpClient::AcpClient(QObject *parent)
    : QObject(parent)
{
}

void AcpClient::connectTo(const QString &url, const QString &secretKey)
{
    // Fresh wire: drop state that belonged to the previous socket. Pending
    // requests die with it (no reply will ever come), so forgetting them also
    // stops the request-id table from growing across reconnects.
    m_activeRunId.clear();
    m_sessionId.clear();
    m_replaying = false;
    m_pending.clear();
    if (m_transport)
        m_transport->deleteLater();
    auto *transport = new WebSocketTransport(QUrl(url), secretKey, this);
    wireTransport(transport);
    transport->open();
    emit statusChanged(QStringLiteral("connecting…"));
}

void AcpClient::connectRoam(const QString &secret, const QString &card, const QString &label)
{
    if (m_transport)
        m_transport->deleteLater();
    auto *transport = new RoamTransport(secret, card, label, this);
    wireTransport(transport);
    transport->open();
    emit statusChanged(QStringLiteral("roam connecting…"));
}

void AcpClient::wireTransport(AcpTransport *transport)
{
    m_transport = transport;
    connect(transport, &AcpTransport::opened, this, &AcpClient::onOpen);
    connect(transport, &AcpTransport::textReceived, this,
            [this](const QString &msg) { onMessage(msg.toUtf8()); });
    connect(transport, &AcpTransport::closed, this,
            [this](const QString &reason) {
                if (!reason.isEmpty())
                    emit statusChanged(QStringLiteral("disconnected: ") + reason);
                else
                    emit statusChanged(QStringLiteral("disconnected"));
            });
    connect(transport, &AcpTransport::error, this,
            [this](const QString &message) {
                emit statusChanged(QStringLiteral("error: ") + message);
            });
}

void AcpClient::close()
{
    if (m_transport)
        m_transport->close();
}

int AcpClient::rpc(const QString &method, const QJsonObject &params, const QString &tag)
{
    const int id = m_nextId++;
    // `tag` lets one method serve two features (sources/list backs projects AND skills);
    // the reply dispatch keys on the tag, defaulting to the method name.
    m_pending.insert(id, tag.isEmpty() ? method : tag);
    send(makeRequest(method, params, id));
    return id;
}

QJsonObject AcpClient::makeRequest(const QString &method, const QJsonObject &params, int id)
{
    return {
        {"jsonrpc", "2.0"},
        {"id", id},
        {"method", method},
        {"params", params},
    };
}

void AcpClient::send(const QJsonObject &frame)
{
    if (!m_transport)
        return;
    m_transport->sendText(QString::fromUtf8(QJsonDocument(frame).toJson(QJsonDocument::Compact)));
}

void AcpClient::onOpen()
{
    emit statusChanged(QStringLiteral("connected — initializing"));
    QJsonObject caps;
    caps.insert("protocolVersion", 1);
    QJsonObject clientCaps;
    QJsonObject fs;
    fs.insert("readTextFile", false);
    fs.insert("writeTextFile", false);
    clientCaps.insert("fs", fs);
    QJsonObject elic; elic.insert("form", QJsonObject());
    clientCaps.insert("elicitation", elic);
    // goose-specific client capabilities: without recipeParameterRequests,
    // session/new HARD-FAILS for any recipe that declares parameters.
    QJsonObject meta;
    meta.insert("customNotifications", true);
    meta.insert("recipeParameterRequests", true);
    meta.insert("toolCallLabelEnrichment", true);
    QJsonObject goose;
    goose.insert("goose", meta);
    clientCaps.insert("_meta", goose);
    caps.insert("clientCapabilities", clientCaps);
    rpc("initialize", caps);
}

void AcpClient::onMessage(const QByteArray &data)
{
    const QJsonDocument doc = QJsonDocument::fromJson(data);
    if (!doc.isObject()) {
        emit error(QStringLiteral("bad json"), false);
        return;
    }
    handle(doc.object());
}

void AcpClient::handle(const QJsonObject &obj)
{
    const QString method = str(obj, "method");
    const int id = obj.value("id").toInt(-1);
    if (!method.isEmpty() && id != -1) {
        serverRequest(method, id, obj.value("params").toObject());
    } else if (!method.isEmpty()) {
        notification(method, obj.value("params").toObject());
    } else if (id != -1) {
        response(id, obj.value("result").toObject(), obj.value("error").toObject());
    }
}

void AcpClient::response(int id, const QJsonObject &result, const QJsonObject &errObj)
{
    const QString method = m_pending.take(id);
    if (!errObj.isEmpty()) {
        const QString errText = QString::fromUtf8(QJsonDocument(errObj).toJson(QJsonDocument::Compact));
        // A stale/archived session (e.g. a saved last_session that was since archived,
        // or a roam session no longer reachable) can't be resumed — fall back to a
        // fresh session rather than leaving the app stuck on the failed load.
        if (method == QStringLiteral("session/load")) {
            m_replaying = false;
            startNewSession();
            return;
        }
        if (method == QStringLiteral("session/prompt")) {
            emit error(method + ": " + errText, false);
            return;
        }
        // steer is foreground: a failed steer means the user's message did NOT reach the
        // model, which they must see (typically the run ended between typing and sending).
        if (method == QStringLiteral("_goose/unstable/session/steer")) {
            emit error(method + ": " + errText, false);
            return;
        }
        // A failed template fetch must clear the in-flight marker (empty html) or the
        // bubble is wedged forever — not become an error bubble.
        if (method.startsWith(QLatin1String("appres|"))) {
            emit appResource(method.mid(7), QString());
            return;
        }
        if (method.startsWith(QLatin1String("probe|"))) {
            emit sessionProbe(method.mid(6), QString(), -1);
            return;
        }
        emit error(method + ": " + errText, true);
        return;
    }

    // MCP-App template fetch: ReadResourceResponse nests the result under "result":
    // { result: { contents: [{ uri, mimeType, text }] } }
    if (method.startsWith(QLatin1String("probe|"))) {
        const QString sid = method.mid(6);
        const QJsonObject session = obj(result, "session");
        emit sessionProbe(sid, str(session, "updatedAt"),
                          obj(session, "_meta").value("messageCount").toInt());
        return;
    }

    if (method.startsWith(QLatin1String("appres|"))) {
        QString html;
        const QJsonArray contents = arr(obj(result, "result"), "contents");
        for (const auto &el : contents) {
            const QString t = str(el.toObject(), "text");
            if (!t.isEmpty()) {
                html = t;
                break;
            }
        }
        emit appResource(method.mid(7), html);
        return;
    }

    if (method == QStringLiteral("initialize")) {
        if (m_browseOnly) {
            listSessions();   // peer browse: list, don't open anything
        } else if (!m_resumeId.isEmpty()) {
            m_replaying = true;
            loadSession();
        } else {
            startNewSession();
        }
    } else if (method == QStringLiteral("session/new")) {
        m_sessionId = str(result, "sessionId");
        emit statusChanged(m_sessionId.isEmpty() ? QStringLiteral("session/new returned no sessionId")
                                                 : QStringLiteral("ready"));
        if (!m_sessionId.isEmpty())
            emit sessionReady(m_sessionId);
        const QVariantList cfg = parseConfig(result);
        emit configReady(cfg);
        applyDesired(cfg);
    } else if (method == QStringLiteral("session/load")) {
        m_replaying = false;
        m_sessionId = m_resumeId;
        emit statusChanged(QStringLiteral("ready"));
        if (!m_sessionId.isEmpty())
            emit sessionReady(m_sessionId);
        emit configReady(parseConfig(result));
    } else if (method == QStringLiteral("session/list")) {
        emit sessionsReady(parseSessions(result));
    } else if (method == QStringLiteral("session/prompt")) {
        m_activeRunId.clear();   // the run this id named is over; steering it would fail
        emit runEnded(str(result, "stopReason"));
    } else if (method == QStringLiteral("session/set_config_option")) {
        emit configReady(parseConfig(result));
    } else if (method == QStringLiteral("_goose/unstable/session/steer")) {
        // The steered message streams back as chunks; nothing to do on the reply.
    } else if (method == QStringLiteral("_goose/unstable/session/export")) {
        emit exportResult(str(result, "data"));
    } else if (method == QStringLiteral("_goose/unstable/session/rename")) {
        listSessions();
    } else if (method == QStringLiteral("_goose/unstable/session/archive")
               || method == QStringLiteral("_goose/unstable/session/unarchive")
               || method == QStringLiteral("session/delete")) {
        listSessions();
    } else if (method == QStringLiteral("_goose/unstable/tools/list")) {
        QVariantList names;
        const QJsonArray tarr = arr(result, "tools");
        for (const auto &el : tarr)
            names << str(el.toObject(), "name");
        emit toolsReady(names);
    } else if (method == QStringLiteral("_goose/unstable/config/extensions/list")) {
        emit extensionsReady(parseExtensions(result));
    } else if (method == QStringLiteral("_goose/unstable/config/extensions/set-enabled")
               || method == QStringLiteral("_goose/unstable/config/extensions/add")) {
        // Global toggles: re-list so the UI reflects the new enabled state.
        listConfigExtensions();
    } else if (method == QStringLiteral("_goose/unstable/session/extensions/list")) {
        QStringList names;
        const QJsonArray sarr = arr(result, "extensions");
        for (const auto &el : sarr) {
            const QJsonObject extension = el.toObject();
            QString name = str(extension, "name");
            // MCP extensions report their name under server.name, while
            // builtin/platform extensions report it at the top level.
            if (name.isEmpty())
                name = str(obj(extension, "server"), "name");
            if (!name.isEmpty())
                names << name;
        }
        emit sessionExtensionsReady(names);
    } else if (method == QStringLiteral("_goose/unstable/session/extensions/add")) {
        // The paired remove didn't re-list; this add's reply is the moment the session's
        // tool set is current, so refresh tools from here (mirrors the Android client).
        listTools();
        listSessionExtensions();
    } else if (method == QStringLiteral("_goose/unstable/sources/list")) {
        emit projectsReady(parseProjects(result));
    } else if (method == QStringLiteral("_goose/unstable/sources/list#skill")) {
        emit skillsReady(parseSkills(result));
    } else if (method == QStringLiteral("_goose/unstable/sources/create")
               || method == QStringLiteral("_goose/unstable/sources/delete")) {
        // Mutation replies carry no useful body; re-list so the sidebar reflects them.
        listProjects();
    } else if (method == QStringLiteral("_goose/unstable/sources/update")
               || method == QStringLiteral("_goose/unstable/sources/delete#skill")) {
        listSkills();
    } else if (method == QStringLiteral("_goose/unstable/session/project/update")) {
        listProjects();
    } else if (method == QStringLiteral("_goose/unstable/config/read")) {
        emit serverConfigValue(str(result, "key"), str(result, "value"));
    } else if (method == QStringLiteral("_goose/unstable/config/upsert")) {
        // Upsert returns empty; nothing to reflect (the caller re-reads if it wants confirmation).
    } else if (method == QStringLiteral("_goose/unstable/providers/supported-models/list")) {
        QStringList models;
        const QJsonArray marr = arr(result, "models");
        for (const auto &el : marr)
            models << el.toString();
        emit supportedModelsReady(str(result, "providerId"), models);
    } else if (method == QStringLiteral("_goose/unstable/recipes/list")) {
        emit recipesReady(parseRecipes(result));
    } else if (method == QStringLiteral("_goose/unstable/schedules/list")) {
        emit schedulesReady(parseSchedules(result));
    } else if (method == QStringLiteral("_goose/unstable/recipes/schedule")) {
        listSchedules();
        listRecipes();
    } else if (method == QStringLiteral("_goose/unstable/recipes/save")
               || method == QStringLiteral("_goose/unstable/recipes/delete")) {
        listRecipes();
    } else if (method == QStringLiteral("_goose/unstable/schedules/pause")
               || method == QStringLiteral("_goose/unstable/schedules/unpause")
               || method == QStringLiteral("_goose/unstable/schedules/delete")
               || method == QStringLiteral("_goose/unstable/schedules/update")
               || method == QStringLiteral("_goose/unstable/schedules/run-now")) {
        listSchedules();
    }
}

void AcpClient::notification(const QString &method, const QJsonObject &params)
{
    if (method == QStringLiteral("session/update"))
        standardUpdate(params);
    else if (method == QStringLiteral("_goose/unstable/session/update"))
        gooseUpdate(params);
}

void AcpClient::gooseUpdate(const QJsonObject &params)
{
    // Custom notifications, gated on clientCapabilities.customNotifications: compaction
    // status lines and per-message tok/s + cost. Mirrors grouse/AcpClient.kt gooseUpdate.
    const QJsonObject update = obj(params, "update");
    const QString tag = str(update, "sessionUpdate");
    if (tag == QStringLiteral("status_message")) {
        const QString msg = str(obj(update, "status"), "message");
        if (!msg.isEmpty())
            emit compactionStatus(msg);
    } else if (tag == QStringLiteral("message_usage")) {
        const QJsonObject usage = obj(update, "usage");
        emit messageUsage(QVariantMap{
            {"outputTokens", usage.value("outputTokens").toInt()},
            {"elapsedMs", usage.value("elapsedMs").toVariant()},
            {"timeToFirstTokenMs", usage.value("timeToFirstTokenMs").toVariant()},
            {"cost", usage.value("cost").toDouble()},
        });
    }
}

void AcpClient::standardUpdate(const QJsonObject &params)
{
    const QJsonObject updateArr = params.value("update").toObject();
    // ACP wraps the update in {update: {...}}; recently also direct — handle both.
    const QJsonObject update = updateArr.isEmpty() ? params : updateArr;
    const QString tag = str(update, "sessionUpdate");
    const QJsonObject goose = obj(obj(update, "_meta"), "goose");

    auto msgId = [&] { return str(goose, "messageId"); };
    auto text = [&] {
        const QJsonObject c = obj(update, "content");
        const QString type = str(c, "type");
        if (type.isEmpty() || type == "text")
            return str(c, "text");
        if (type == "resource_link")
            return QStringLiteral("[resource]");
        return QStringLiteral("[") + type + QStringLiteral("]");
    };

    if (tag == QStringLiteral("user_message_chunk")) {
        const QString t = text();
        if (!t.isEmpty())
            emit userChunk(t, msgId());
    } else if (tag == QStringLiteral("agent_message_chunk")) {
        const QString t = text();
        if (!t.isEmpty())
            emit agentChunk(t, msgId());
    } else if (tag == QStringLiteral("agent_thought_chunk")) {
        const QString t = text();
        if (!m_replaying && !t.isEmpty())
            emit thoughtChunk(t);
    } else if (tag == QStringLiteral("tool_call")) {
        const QJsonObject rawInput = obj(update, "rawInput");
        const QString detail = str(rawInput, "command");
        const QString toolCallId = str(update, "toolCallId");
        // MCP-App path: the server names a UI resource for this tool's output (this is how
        // ALL autovisualiser types work — sankey/radar/map/mermaid, not just charts). We fetch
        // the template via resources/read and render it; until then it's a plain tool chip.
        const QJsonObject gooseMeta = obj(obj(update, "_meta"), "goose");
        const QJsonObject mcpApp = obj(gooseMeta, "mcpApp");
        const QString appUri = str(mcpApp, "resourceUri");
        const QString appExt = str(mcpApp, "extensionName");
        if (!appUri.isEmpty() && !appExt.isEmpty()) {
            const QString appKey = appExt + QStringLiteral("|") + appUri;
            emit mcpAppToolCall(str(update, "title"), toolCallId, appKey, appUri, appExt,
                                QString::fromUtf8(QJsonDocument(rawInput).toJson(QJsonDocument::Compact)));
        } else {
            // Legacy fallback only (server too old to send mcpApp meta): `data` arrives as a
            // JSON OBJECT, not a string — reading only the primitive form silently disabled
            // every chart once.
            const QString toolName = str(obj(gooseMeta, "toolCall"), "toolName");
            QString chartData;
            const QJsonValue d = rawInput.value("data");
            if (d.isObject())
                chartData = QString::fromUtf8(QJsonDocument(d.toObject()).toJson(QJsonDocument::Compact));
            else if (d.isString())
                chartData = d.toString();
            if (toolName == QStringLiteral("autovisualiser__show_chart") && !chartData.isEmpty())
                emit chartToolCall(str(update, "title"), toolCallId, chartData);
            else
                emit toolCall(str(update, "title"), detail, toolCallId);
        }
    } else if (tag == QStringLiteral("tool_call_update")) {
        const QString id = str(update, "toolCallId");
        const QString status = str(update, "status");
        // Streaming shell output rides _meta.toolNotification (live_output variant); without
        // it a long shell run is a blank chip until it finishes. Appends `live=true` so the
        // manager ACCUMULATES the chunk on the bubble instead of replacing it.
        const QJsonObject notif = obj(obj(update, "_meta"), "toolNotification");
        if (str(notif, "type") == QStringLiteral("live_output")) {
            QString chunk;
            const QJsonArray chunks = arr(obj(notif, "params"), "chunks");
            for (const auto &el : chunks)
                chunk += str(obj(el.toObject(), "output"), "text");
            if (!chunk.isEmpty())
                emit toolCallUpdate(id, status, chunk, true);
            return;
        }
        QStringList outputs;
        const QJsonArray content = arr(update, "content");
        for (const auto &el : content)
            outputs << str(obj(el.toObject(), "content"), "text");
        emit toolCallUpdate(id, status, outputs.join(QLatin1Char('\n')), false);
    } else if (tag == QStringLiteral("usage_update")) {
        const QJsonObject costObj = obj(update, "cost");
        emit usageUpdate(update.value("used").toInt(), update.value("size").toInt(),
                         costObj.value("amount").toDouble(), str(costObj, "currency"));
    } else if (tag == QStringLiteral("session_info_update")) {
        const QString sid = str(params, "sessionId");
        const QString title = update.value("title").toString();
        const QString updatedAt = update.value("updatedAt").toString();
        // Overloaded notification: active-run lifecycle rides _meta.goose.activeRunId; its
        // presence is what makes session/steer possible. Emit it separately from the title bump.
        const QJsonObject gm = obj(obj(update, "_meta"), "goose");
        if (gm.contains("activeRunId")) {
            m_activeRunId = gm.value("activeRunId").toString();
            emit activeRunChanged(sid, m_activeRunId);
        }
        emit sessionTouched(sid, title, updatedAt);
    } else if (tag == QStringLiteral("current_mode_update")) {
        // The session's mode changed server-side (e.g. another client). Emit the value
        // alone — a synthetic configReady would CLOBBER the full config list the
        // provider/model selectors depend on. The Manager patches the single entry.
        const QString modeId = str(update, "currentModeId");
        if (!modeId.isEmpty())
            emit modeChanged(modeId);
    } else if (tag == QStringLiteral("available_commands_update")) {
        QStringList names;
        const QJsonArray carr = arr(update, "availableCommands");
        for (const auto &el : carr)
            names << str(el.toObject(), "name");
        if (!names.isEmpty())
            emit commandsReady(names);
    }
}

void AcpClient::serverRequest(const QString &method, int id, const QJsonObject &params)
{
    if (method == QStringLiteral("session/request_permission")) {
        const QJsonObject tc = obj(params, "toolCall");
        const QString toolCallId = str(tc, "toolCallId");
        const QString title = str(tc, "title");
        const QString detail = str(obj(tc, "rawInput"), "command");
        QVariantList opts;
        const QJsonArray oarr = arr(params, "options");
        for (const auto &o : oarr) {
            const QJsonObject oo = o.toObject();
            opts << QVariantMap{{"optionId", str(oo, "optionId")},
                                {"name", str(oo, "name")}, {"kind", str(oo, "kind")}};
        }
        // We answer with the chosen option via respondPermission() using the raw JSON-RPC id.
        m_pending.insert(id, QStringLiteral("__permission__") + toolCallId);
        emit permissionRequest(toolCallId, title, detail, opts);
    } else if (method == QStringLiteral("_goose/unstable/session/recipe/request-params")) {
        // A parameterized recipe started via _meta.recipeId. We have no form UI,
        // so answer immediately with each parameter's default so the session can
        // start; values map key -> default (response envelope is camelCase).
        QJsonObject values;
        const QJsonArray parr = arr(params, "parameters");
        for (const auto &p : parr) {
            const QJsonObject po = p.toObject();
            const QString key = str(po, "key");
            if (!key.isEmpty() && po.contains("default"))
                values.insert(key, po.value("default"));
        }
        send({{"jsonrpc", "2.0"}, {"id", id},
              {"result", QJsonObject{{"action", "submit"}, {"values", values}}}});
    } else {
        // Unknown server request -> a real JSON-RPC error, not a silent empty result.
        send({{"jsonrpc", "2.0"},
              {"id", id},
              {"error", QJsonObject{{"code", -32601}, {"message", "not supported by this client: " + method}}}});
    }
}

void AcpClient::startNewSession()
{
    QJsonObject params;
    params.insert("cwd", m_wantCwd);
    params.insert("mcpServers", QJsonArray());
    // _meta.client present => SessionType::User, so Desktop/CLI can see these chats.
    QJsonObject meta;
    meta.insert("client", "grouse-desktop");
    // A recipe started from the Recipes section: session/new carries its id.
    if (!m_desiredRecipeId.isEmpty())
        meta.insert("recipeId", m_desiredRecipeId);
    params.insert("_meta", meta);
    rpc("session/new", params);
}

void AcpClient::loadSession()
{
    QJsonObject params;
    params.insert("sessionId", m_resumeId);
    // session/load's cwd SILENTLY REWRITES working_dir if it differs from stored — pass the real cwd.
    params.insert("cwd", m_resumeCwd);
    params.insert("mcpServers", QJsonArray());
    rpc("session/load", params);
}

// ---- outbound RPC implementations -------------------------------------------

void AcpClient::listSessions()
{
    QJsonObject params;
    QJsonObject meta;
    meta.insert("types", QJsonArray{QStringLiteral("user"), QStringLiteral("acp")});
    params.insert("_meta", meta);
    rpc("session/list", params);
}

void AcpClient::probeSession(const QString &sessionId)
{
    rpc(QStringLiteral("_goose/unstable/session/info"),
        QJsonObject{{"sessionId", sessionId}},
        QStringLiteral("probe|") + sessionId);
}

void AcpClient::sendPrompt(const QString &text, const QVariantList &images)
{
    const QString sid = m_sessionId;
    if (sid.isEmpty()) {
        emit error(QStringLiteral("not ready — no session"), false);
        return;
    }
    QJsonObject params;
    params.insert("sessionId", sid);
    QJsonArray prompt;
    if (!text.trimmed().isEmpty()) {
        QJsonObject block;
        block.insert("type", QStringLiteral("text"));
        block.insert("text", text);
        prompt.append(block);
    }
    // Attachment content blocks, produced by the Manager (image vs embedded
    // resource). Pass them through verbatim — the type/mime/data shapes matter.
    for (const auto &v : images) {
        const QVariantMap block = v.toMap();
        QJsonObject jo;
        for (auto it = block.constBegin(); it != block.constEnd(); ++it)
            jo.insert(it.key(), QJsonValue::fromVariant(it.value()));
        prompt.append(jo);
    }
    if (prompt.isEmpty()) {
        emit error(QStringLiteral("empty prompt"), false);
        return;
    }
    params.insert("prompt", prompt);
    rpc("session/prompt", params);
}

void AcpClient::steer(const QString &text)
{
    const QString sid = m_sessionId;
    if (sid.isEmpty()) {
        emit error(QStringLiteral("not ready — no session"), false);
        return;
    }
    // Inject into the RUNNING turn; the server validates expectedRunId so a run that ended
    // between typing and sending fails loudly instead of starting a stray turn. Text-only.
    QJsonObject params;
    params.insert("sessionId", sid);
    params.insert("prompt", QJsonArray{QJsonObject{{"type", "text"}, {"text", text}}});
    params.insert("expectedRunId", m_activeRunId);
    rpc("_goose/unstable/session/steer", params);
}

void AcpClient::exportSession(const QString &sessionId)
{
    rpc("_goose/unstable/session/export", QJsonObject{{"sessionId", sessionId}});
}

void AcpClient::listSkills()
{
    // Skills share sources/list with projects; the #skill tag disambiguates the reply.
    rpc("_goose/unstable/sources/list", QJsonObject{{"type", "skill"}},
        QStringLiteral("_goose/unstable/sources/list#skill"));
}

void AcpClient::updateSkill(const QString &path, const QString &name,
                            const QString &description, const QString &content)
{
    rpc("_goose/unstable/sources/update", QJsonObject{
        {"type", "skill"}, {"path", path},
        {"name", name}, {"description", description}, {"content", content}});
}

void AcpClient::deleteSkill(const QString &path)
{
    // The #skill tag keeps the reply out of the projects branch (untagged sources/delete
    // refreshes projects; a skill delete would leave stale skill rows until a refresh).
    rpc("_goose/unstable/sources/delete", QJsonObject{{"type", "skill"}, {"path", path}},
        QStringLiteral("_goose/unstable/sources/delete#skill"));
}

void AcpClient::readConfig(const QString &key)
{
    rpc("_goose/unstable/config/read", QJsonObject{{"key", key}});
}

void AcpClient::upsertConfig(const QString &key, const QString &value)
{
    rpc("_goose/unstable/config/upsert", QJsonObject{{"key", key}, {"value", value}});
}

void AcpClient::listSupportedModels(const QString &providerId)
{
    rpc("_goose/unstable/providers/supported-models/list", QJsonObject{{"providerId", providerId}});
}

void AcpClient::readAppResource(const QString &appKey, const QString &uri, const QString &extensionName)
{
    const QString sid = m_sessionId;
    if (sid.isEmpty())
        return;
    // The tag carries the cache key because several tools can share one template and
    // several bubbles can wait on one fetch.
    rpc("_goose/unstable/resources/read", QJsonObject{
        {"sessionId", sid}, {"uri", uri}, {"extensionName", extensionName}},
        QStringLiteral("appres|") + appKey);
}

void AcpClient::respondPermission(const QString &toolCallId, const QString &optionId)
{
    // The JSON-RPC id was stored as "__permission__<toolCallId>".
    int id = -1;
    for (auto it = m_pending.begin(); it != m_pending.end(); ++it) {
        if (it.value() == QStringLiteral("__permission__") + toolCallId) {
            id = it.key();
            m_pending.erase(it);
            break;
        }
    }
    if (id == -1)
        return;
    QJsonObject outcome;
    if (!optionId.isEmpty()) {
        outcome.insert("outcome", QStringLiteral("selected"));
        outcome.insert("optionId", optionId);
    } else {
        outcome.insert("outcome", QStringLiteral("cancelled"));
    }
    send({{"jsonrpc", "2.0"}, {"id", id}, {"result", QJsonObject{{"outcome", outcome}}}});
}

void AcpClient::setConfigOption(const QString &configId, const QString &value)
{
    const QString sid = m_sessionId;
    if (sid.isEmpty())
        return;
    QJsonObject params;
    params.insert("sessionId", sid);
    params.insert("configId", configId);
    params.insert("value", value);
    rpc("session/set_config_option", params);
}

void AcpClient::cancel()
{
    if (m_sessionId.isEmpty())
        return;
    QJsonObject params;
    params.insert("sessionId", m_sessionId);
    send({{"jsonrpc", "2.0"}, {"method", QStringLiteral("session/cancel")}, {"params", params}});
}

void AcpClient::listTools()
{
    // Session-scoped: only answers on THIS session's stream (same transport as
    // the sessionId we already hold). Tools are per-conversation, so this always
    // reflects the chat currently open — not a global catalogue.
    if (m_sessionId.isEmpty())
        return;
    QJsonObject params;
    params.insert("sessionId", m_sessionId);
    rpc("_goose/unstable/tools/list", params);
}

void AcpClient::renameSession(const QString &sessionId, const QString &title)
{
    QJsonObject params;
    params.insert("sessionId", sessionId);
    params.insert("title", title);
    rpc("_goose/unstable/session/rename", params);
}

void AcpClient::archiveSession(const QString &sessionId)
{
    QJsonObject params;
    params.insert("sessionId", sessionId);
    rpc("_goose/unstable/session/archive", params);
}

void AcpClient::unarchiveSession(const QString &sessionId)
{
    QJsonObject params;
    params.insert("sessionId", sessionId);
    rpc("_goose/unstable/session/unarchive", params);
}

void AcpClient::deleteSession(const QString &sessionId)
{
    QJsonObject params;
    params.insert("sessionId", sessionId);
    rpc("session/delete", params);
}

void AcpClient::listConfigExtensions()
{
    rpc("_goose/unstable/config/extensions/list", QJsonObject());
}

void AcpClient::listSessionExtensions()
{
    if (m_sessionId.isEmpty())
        return;
    QJsonObject params;
    params.insert("sessionId", m_sessionId);
    rpc("_goose/unstable/session/extensions/list", params);
}

void AcpClient::addSessionExtension(const QJsonObject &extensionData)
{
    if (m_sessionId.isEmpty())
        return;
    QJsonObject params;
    params.insert("sessionId", m_sessionId);
    params.insert("extension", extensionData);
    rpc("_goose/unstable/session/extensions/add", params);
}

void AcpClient::removeSessionExtension(const QString &name)
{
    if (m_sessionId.isEmpty())
        return;
    QJsonObject params;
    params.insert("sessionId", m_sessionId);
    params.insert("name", name);
    rpc("_goose/unstable/session/extensions/remove", params);
}

void AcpClient::setConfigExtensionEnabled(const QString &name, bool enabled)
{
    rpc("_goose/unstable/config/extensions/set-enabled",
        QJsonObject{{"name", name}, {"enabled", enabled}});
}

void AcpClient::addConfigExtension(const QJsonObject &extensionData, bool enabled)
{
    // Rewrites config.yaml (drops its comments — goose re-serialises from its parsed
    // model). Sending the extension back with a modified available_tools is how a GLOBAL
    // tool allowlist is saved (defaults for NEW sessions).
    rpc("_goose/unstable/config/extensions/add",
        QJsonObject{{"extension", extensionData}, {"enabled", enabled}});
}

void AcpClient::listProjects()
{
    rpc("_goose/unstable/sources/list", QJsonObject{{"type", "project"}});
}

void AcpClient::createProject(const QString &name, const QString &description, const QString &root)
{
    QJsonObject params;
    params.insert("type", "project");
    params.insert("name", name);
    params.insert("description", description);
    params.insert("content", root.isEmpty() ? QString() : QStringLiteral("root: ") + root + QStringLiteral("\n"));
    params.insert("target", QJsonObject{{"scope", "global"}});
    rpc("_goose/unstable/sources/create", params);
}

void AcpClient::deleteProject(const QString &path)
{
    // sources/delete takes the source PATH (e.g. "projects/foo.md"), and type is
    // required alongside it or the server rejects with -32602.
    QJsonObject params;
    params.insert("type", "project");
    params.insert("path", path);
    rpc("_goose/unstable/sources/delete", params);
}

void AcpClient::assignSessionProject(const QString &sessionId, const QString &projectId)
{
    QJsonObject params;
    params.insert("sessionId", sessionId);
    // Empty means "unfile": send an explicit JSON null, never omit the key.
    params.insert("projectId", projectId.isEmpty() ? QJsonValue(QJsonValue::Null) : QJsonValue(projectId));
    rpc("_goose/unstable/session/project/update", params);
}

void AcpClient::listRecipes()
{
    rpc("_goose/unstable/recipes/list", QJsonObject());
}

void AcpClient::listSchedules()
{
    rpc("_goose/unstable/schedules/list", QJsonObject());
}

void AcpClient::scheduleRecipe(const QString &id, const QString &cron)
{
    // recipes/* params are snake_case; an omitted cron_schedule silently reads as
    // "no cron", so unscheduling must send an explicit null.
    QJsonObject params;
    params.insert("id", id);
    params.insert("cron_schedule", cron.isEmpty() ? QJsonValue(QJsonValue::Null) : QJsonValue(cron));
    rpc("_goose/unstable/recipes/schedule", params);
}

void AcpClient::deleteRecipe(const QString &id)
{
    rpc("_goose/unstable/recipes/delete", QJsonObject{{"id", id}});
}

void AcpClient::setSchedulePaused(const QString &scheduleId, bool paused)
{
    rpc(QStringLiteral("_goose/unstable/schedules/") + (paused ? QStringLiteral("pause") : QStringLiteral("unpause")),
        QJsonObject{{"scheduleId", scheduleId}});
}

void AcpClient::runScheduleNow(const QString &scheduleId)
{
    // This BLOCKS server-side for the whole run; the UI must not await it.
    rpc("_goose/unstable/schedules/run-now", QJsonObject{{"scheduleId", scheduleId}});
}

void AcpClient::setScheduleCron(const QString &scheduleId, const QString &cron)
{
    rpc("_goose/unstable/schedules/update", QJsonObject{{"scheduleId", scheduleId}, {"cron", cron}});
}

// ---- parsers ----------------------------------------------------------------

QVariantList AcpClient::parseSessions(const QJsonObject &result)
{
    QVariantList out;
    const QJsonArray sarr = arr(result, "sessions");
    for (const auto &el : sarr) {
        const QJsonObject o = el.toObject();
        const QString sid = str(o, "sessionId");
        const QString title = str(o, "title");
        if (title.isEmpty() && sid.isEmpty())
            continue;
        if (title.startsWith(QLatin1String("Scheduled job:")))
            continue;
        const QJsonObject meta = obj(o, "_meta");
        // goose's archive only stamps archivedAt; session/list has no archived
        // filter, so the client must drop them or they reappear on every refresh.
        if (meta.contains("archivedAt"))
            continue;
        // Roam (federated) sessions are identified by a `roam:<peer>:<id>` id;
        // the peer name drives the REMOTE grouping in the sidebar.
        QString peer;
        if (sid.startsWith(QLatin1String("roam:")))
            peer = QString(sid).mid(5).section(QLatin1Char(':'), 0, 0);
        out << QVariantMap{
            {"sessionId", sid},
            {"title", title.isEmpty() ? sid : title},
            {"updatedAt", str(o, "updatedAt")},
            {"lastMessageAt", str(meta, "lastMessageAt")},
            {"snippet", str(meta, "lastMessageSnippet")},
            {"model", str(meta, "modelId")},
            {"cwd", str(o, "cwd")},
            {"messageCount", meta.value("messageCount").toInt()},
            {"peer", peer},
            {"projectId", str(meta, "projectId")},
        };
    }
    return out;
}

QVariantList AcpClient::parseProjects(const QJsonObject &result)
{
    QVariantList out;
    const QJsonArray sarr = arr(result, "sources");
    for (const auto &el : sarr) {
        const QJsonObject o = el.toObject();
        const QString path = str(o, "path");
        const QString name = str(o, "name");
        if (path.isEmpty() || name.isEmpty())
            continue;
        // The project id is the file stem of its source path ("projects/foo.md" -> "foo").
        QString id = path.mid(path.lastIndexOf(QLatin1Char('/')) + 1);
        if (id.endsWith(QLatin1String(".md")))
            id.chop(3);
        if (id.isEmpty())
            id = name;
        QString root;
        const QString content = str(o, "content");
        const QStringList lines = content.split(QLatin1Char('\n'));
        for (const QString &line : lines) {
            if (line.trimmed().startsWith(QLatin1String("root:"))) {
                root = line.section(QLatin1Char(':'), 1).trimmed();
                break;
            }
        }
        out << QVariantMap{
            {"id", id},
            {"name", name},
            {"path", path},
            {"description", str(o, "description")},
            {"content", str(o, "content")},
            {"root", root},
        };
    }
    return out;
}

// Convert an extension as goose LISTED it into the shape the add methods ACCEPT.
// A listed remote extension is `type: streamable_http` with a header map; add wants a tagged
// union where a remote server is {type: mcp, server: {type: http, url, headers:[{name,value}]}}.
// builtin/platform/mcp pass through; feeding the listed shape straight back is rejected -32602.
QJsonObject toExtensionDto(const QJsonObject &raw)
{
    const QString type = str(raw, "type");
    const QString name = str(raw, "name");
    if (type == QStringLiteral("builtin") || type == QStringLiteral("platform")
        || type == QStringLiteral("mcp") || name.isEmpty())
        return raw;

    QJsonObject server;
    if (type == QStringLiteral("stdio")) {
        server.insert("type", "stdio");
        server.insert("name", name);
        server.insert("command", str(raw, "cmd"));
        server.insert("args", arr(raw, "args"));
        server.insert("env", QJsonArray());
    } else {
        server.insert("type", type == QStringLiteral("sse") ? "sse" : "http");
        server.insert("name", name);
        server.insert("url", str(raw, "uri"));
        QJsonArray headers;
        const QJsonObject hmap = obj(raw, "headers");
        for (auto it = hmap.constBegin(); it != hmap.constEnd(); ++it)
            headers << QJsonObject{{"name", it.key()}, {"value", it.value().toString()}};
        server.insert("headers", headers);
    }
    QJsonObject dto;
    dto.insert("type", "mcp");
    dto.insert("server", server);
    if (raw.contains("timeout")) dto.insert("timeout", raw.value("timeout"));
    if (raw.contains("description")) dto.insert("description", raw.value("description"));
    if (raw.contains("env_keys")) dto.insert("envKeys", raw.value("env_keys"));
    if (raw.contains("available_tools")) dto.insert("available_tools", raw.value("available_tools"));
    return dto;
}

QVariantList AcpClient::parseExtensions(const QJsonObject &result)
{
    QVariantList out;
    const QJsonArray earr = arr(result, "extensions");
    for (const auto &el : earr) {
        const QJsonObject wrap = el.toObject();
        const QJsonObject ext = obj(wrap, "extension");
        if (ext.isEmpty())
            continue;
        // mcp remote servers carry the name NESTED at server.name, not top-level ext.name.
        QString name = str(ext, "name");
        if (name.isEmpty())
            name = str(obj(ext, "server"), "name");
        if (name.isEmpty())
            continue;
        out << QVariantMap{
            {"name", name},
            {"type", str(ext, "type")},
            {"enabled", wrap.value("enabled").toBool()},
            {"attrib", str(ext, "type") == QStringLiteral("mcp")},
            {"availableTools", arr(ext, "available_tools")},
            {"raw", QVariant::fromValue(ext)},
        };
    }
    return out;
}

QVariantList AcpClient::parseRecipes(const QJsonObject &result)
{
    QVariantList out;
    const QJsonArray rarr = arr(result, "recipes");
    for (const auto &el : rarr) {
        const QJsonObject o = el.toObject();
        const QString id = str(o, "id");
        if (id.isEmpty())
            continue;
        // The recipe DTO is nested under `recipe`; the top-level fields are
        // snake_case (file_path, schedule_cron).
        const QJsonObject r = obj(o, "recipe");
        const QJsonObject settings = obj(r, "settings");
        QVariantList params;
        for (const auto &p : arr(r, "parameters")) {
            const QJsonObject po = p.toObject();
            params << QVariantMap{
                {"key", str(po, "key")},
                {"description", str(po, "description")},
                {"default", str(po, "default")},
                {"inputType", str(po, "input_type")},
                {"requirement", str(po, "requirement")},
            };
        }
        out << QVariantMap{
            {"id", id},
            {"title", str(r, "title")},
            {"description", str(r, "description")},
            {"prompt", str(r, "prompt")},
            {"instructions", str(r, "instructions")},
            {"filePath", str(o, "file_path")},
            {"cron", str(o, "schedule_cron")},
            {"provider", str(settings, "goose_provider")},
            {"model", str(settings, "goose_model")},
            {"parameters", params},
        };
    }
    return out;
}

QVariantList AcpClient::parseSchedules(const QJsonObject &result)
{
    // schedules/list replies are camelCase (unlike recipes), and the field is
    // `cron`, not `cron_schedule`.
    QVariantList out;
    const QJsonArray jarr = arr(result, "jobs");
    for (const auto &el : jarr) {
        const QJsonObject o = el.toObject();
        const QString id = str(o, "id");
        if (id.isEmpty())
            continue;
        out << QVariantMap{
            {"id", id},
            {"cron", str(o, "cron")},
            {"source", str(o, "source")},
            {"paused", o.value("paused").toBool()},
            {"running", o.value("currentlyRunning").toBool()},
            {"lastRun", str(o, "lastRun")},
            {"currentSessionId", str(o, "currentSessionId")},
        };
    }
    return out;
}

QVariantList AcpClient::parseConfig(const QJsonObject &result)
{
    QVariantList out;
    const QJsonArray carr = arr(result, "configOptions");
    for (const auto &el : carr) {
        const QJsonObject o = el.toObject();
        const QString id = str(o, "id");
        if (id.isEmpty())
            continue;
        QVariantList choices;
        for (const auto &c : arr(o, "options"))
            choices << choiceToMap(c);
        out << QVariantMap{
            {"id", id},
            {"name", str(o, "name")},
            {"currentValue", str(o, "currentValue")},
            {"choices", QVariant::fromValue(choices)},
        };
    }
    return out;
}

QVariantList AcpClient::parseSkills(const QJsonObject &result)
{
    // skills/list comes back through sources/list: each SourceEntry carries the whole
    // SKILL.md in `content`, so a skill can be read and edited without a second round trip.
    // `writable` is false for the ones goose bundles — offering an unsavable edit is worse
    // than showing it read-only.
    QVariantList out;
    const QJsonArray sarr = arr(result, "sources");
    for (const auto &el : sarr) {
        const QJsonObject o = el.toObject();
        const QString name = str(o, "name");
        if (name.isEmpty())
            continue;
        out << QVariantMap{
            {"name", name},
            {"description", str(o, "description")},
            {"content", str(o, "content")},
            {"path", str(o, "path")},
            {"global", o.value("global").toBool()},
            {"writable", o.value("writable").toBool()},
        };
    }
    return out;
}

void AcpClient::applyDesired(const QVariantList &options)
{    if (m_desired.isEmpty())
        return;
    QHash<QString, QString> current;
    for (const auto &v : options) {
        const QVariantMap m = v.toMap();
        current[m.value("id").toString()] = m.value("currentValue").toString();
    }
    // provider first, then model...
    QStringList order = {QStringLiteral("provider"), QStringLiteral("model"),
                         QStringLiteral("mode"), QStringLiteral("thinking_effort")};
    for (const auto &id : order) {
        for (const auto &pair : m_desired) {
            if (pair.first == id && !pair.second.isEmpty() && pair.second != current[id]) {
                setConfigOption(id, pair.second);
            }
        }
    }
}
