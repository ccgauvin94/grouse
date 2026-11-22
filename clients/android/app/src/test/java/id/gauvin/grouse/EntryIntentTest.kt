// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Launch instructions are one-shot. Android replays the original intent on rotation, so
 *  consuming it in onCreate on every recreation reopened chats and re-sent messages.
 *  The rule is extracted from MainActivity purely so it can be checked without an Activity. */
class EntryIntentTest {

    @Test
    fun `a cold start consumes the launch intent`() {
        assertTrue(shouldConsumeEntryIntent(hasSavedState = false, fromNewIntent = false))
    }

    @Test
    fun `a recreation replays the intent and must not consume it`() {
        assertFalse(shouldConsumeEntryIntent(hasSavedState = true, fromNewIntent = false))
    }

    @Test
    fun `a real onNewIntent delivery is always consumed`() {
        assertTrue(shouldConsumeEntryIntent(hasSavedState = false, fromNewIntent = true))
        assertTrue(shouldConsumeEntryIntent(hasSavedState = true, fromNewIntent = true))
    }
}
