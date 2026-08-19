// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * Foreground service whose only job is to keep the process (and thus the ACP socket) alive
 * while a turn is running or the user opted into a persistent connection. ConnectionManager
 * — the process singleton — owns the actual socket; this just holds the process up.
 */
class ConnectionService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val n = Notifier(this).ongoing("Connected to your agent")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(Notifier.ID_ONGOING, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(Notifier.ID_ONGOING, n)
        }
        return START_STICKY
    }
}
