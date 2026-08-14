package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Finished-turn nudge gate: show the notification only when the app is backgrounded and the
 *  push's session is the one this device armed (sent a turn and is still waiting on it).
 *  Pure function, extracted from GoosePushService so it is JVM-testable. */
class PushNudgeTest {

    @Test
    fun `foreground suppresses the nudge even on a matching session`() {
        assertFalse(shouldShowTurnNudge("s-1", "s-1", isForeground = true))
    }

    @Test
    fun `backgrounded with a matching armed session shows the nudge`() {
        assertTrue(shouldShowTurnNudge("s-1", "s-1", isForeground = false))
    }

    @Test
    fun `no session in the envelope never shows the nudge`() {
        assertFalse(shouldShowTurnNudge(null, "s-1", isForeground = false))
    }

    @Test
    fun `push for a session this device did not arm is suppressed`() {
        assertFalse(shouldShowTurnNudge("s-other", "s-1", isForeground = false))
    }
}
