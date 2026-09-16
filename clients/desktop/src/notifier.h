#pragma once

#include <QString>

/**
 * Desktop notifications, over the freedesktop notification service.
 *
 * Grouse's desktop has no push and needs none: everything it announces it already
 * knows from its own ACP connection — a turn ended, a tool wants approval, another
 * client touched a session. That is deliberate: it keeps the client working against
 * a stock `goose serve` with no server-side helpers, no config keys of ours, and no
 * extra infrastructure. Anything a connected client can't know (a run that finishes
 * while nothing is connected) is out of scope here — that is what the phone's
 * UnifiedPush registration is for, and it is the operator's own plumbing.
 */
namespace Notifier {

/** True only when the app is not the active window — the one time a notification
 *  tells the user something the window isn't already showing. */
bool shouldNotify();

/** Post a notification. Never fatal: a missing (or sandbox-blocked) notification
 *  service means the call is dropped, not that the app misbehaves. */
void send(const QString &summary, const QString &body);

} // namespace Notifier
