package id.gauvin.grouse

import android.app.Application
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner

/** Tracks whole-app foreground/background so ConnectionManager can decide when to notify. */
class GooseApp : Application() {
    override fun onCreate() {
        super.onCreate()
        val cm = ConnectionManager.get(this)
        Push.refresh(this)   // re-register the UnifiedPush endpoint if push is enabled
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) = cm.setForeground(true)
            override fun onStop(owner: LifecycleOwner) = cm.setForeground(false)
        })
    }
}
