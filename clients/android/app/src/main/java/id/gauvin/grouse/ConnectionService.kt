// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * Foreground service whose only job is to keep the process (and thus the ACP socket) alive
 * while a turn is running, a connection is live under the persistent option, or a roam peer
 * is being re-dialed. ConnectionManager — the process singleton — owns the actual socket;
 * this just holds the process up.
 *
 * Type is `specialUse`, not `dataSync`: Android 14+ caps dataSync FGS runs at ~10 minutes,
 * which would itself drop the "persistent connection" it exists to hold.
 *
 * START_STICKY + a null intent means a process the system reclaimed restarts the service on
 * its own — onStartCommand then re-pokes the connection through ConnectionManager.
 */
class ConnectionService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val n = Notifier(this).ongoing("Connected to your agent")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(Notifier.ID_ONGOING, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(Notifier.ID_ONGOING, n)
        }
        // A START_STICKY restart delivers a NULL intent: the process was
        // reclaimed and no Activity runs, so nothing else re-establishes the
        // wires. A normal startService (non-null intent) has the app alive and
        // its own connect path — rekindling here would only double-connect.
        if (intent == null) {
            ConnectionManager.get(applicationContext).rekindleOnServiceStart()
        }
        return START_STICKY
    }
}
