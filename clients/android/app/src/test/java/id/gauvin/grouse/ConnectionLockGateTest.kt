// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The biometric lock's policy is a pure predicate over (unlocked, intent). The audit found the
 * lock was cosmetic: the Compose flag only hid the UI while the manager still dialled with the
 * stored key and notification replies still parked prompts. The gate is now checked at the
 * manager's choke points; this test pins the rule for EVERY intent family, in both states,
 * without an Activity, a core or Android framework classes.
 */
class ConnectionLockGateTest {

    @Test
    fun `unlocked allows every agent intent`() {
        AgentIntent.entries.forEach { intent ->
            assertTrue("unlocked should allow $intent", lockPermits(true, intent))
        }
    }

    @Test
    fun `locked rejects every agent intent`() {
        AgentIntent.entries.forEach { intent ->
            assertFalse("locked must reject $intent", lockPermits(false, intent))
        }
    }
}
