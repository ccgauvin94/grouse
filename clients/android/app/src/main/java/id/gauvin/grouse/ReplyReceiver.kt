// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.RemoteInput

/** Handles the inline "Reply" + "Mark read" actions on a finished-turn / briefing notification. */
class ReplyReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getIntExtra(Notifier.EXTRA_NOTIF_ID, -1)
        when (intent.action) {
            Notifier.ACTION_REPLY -> {
                val text = RemoteInput.getResultsFromIntent(intent)
                    ?.getCharSequence(Notifier.KEY_REPLY)?.toString()?.trim()
                if (!text.isNullOrBlank()) ConnectionManager.get(context).sendWhenReady(text)
                if (id >= 0) context.getSystemService(NotificationManager::class.java).cancel(id)
            }
            Notifier.ACTION_MARK_READ ->
                if (id >= 0) context.getSystemService(NotificationManager::class.java).cancel(id)
        }
    }
}
