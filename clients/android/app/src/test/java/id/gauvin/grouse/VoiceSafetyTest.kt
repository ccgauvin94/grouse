// SPDX-License-Identifier: AGPL-3.0-or-later
package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The pure gates behind the voice safety pass. No AudioRecord, TTS or looper. */
class VoiceSafetyTest {
    @Test
    fun `voice needs an unlocked foreground app with microphone permission`() {
        assertTrue(voicePermits(true, true, true))
        assertFalse(voicePermits(false, true, true))
        assertFalse(voicePermits(true, false, true))
        assertFalse(voicePermits(true, true, false))
        assertFalse(voicePermits(false, false, false))
    }

    @Test
    fun `a late callback is stale after any generation bump`() {
        val generation = 7L
        assertTrue(voiceGenerationCurrent(generation, generation))
        assertFalse(voiceGenerationCurrent(generation + 1, generation))
        assertFalse(voiceGenerationCurrent(generation, generation - 1))
    }

    @Test
    fun `dictation accepts one pending request and rejects a stale id`() {
        val request = "req-1"
        assertTrue(voiceRequestAccepts(request, null))
        assertTrue(voiceRequestAccepts(request, request))
        assertFalse(voiceRequestAccepts(request, "req-0"))
        assertFalse(voiceRequestAccepts(null, request))
        assertFalse(voiceRequestAccepts(null, null))
    }

    @Test
    fun `every stop reason has an honest notice or is deliberately silent`() {
        assertEquals("Microphone permission is off — enable it in Android settings",
            voiceNoticeFor("permission-denied"))
        assertEquals("Couldn't start the microphone — check the permission",
            voiceNoticeFor("microphone"))
        assertEquals("Voice needs the app open and the phone unlocked", voiceNoticeFor("gated"))
        assertEquals("Transcription timed out", voiceNoticeFor("timeout"))
        assertEquals("Didn't catch that — try again", voiceNoticeFor("empty"))
        assertEquals("Connection lost — voice stopped", voiceNoticeFor("wire-lost"))
        assertEquals("Voice stopped while the phone was locked", voiceNoticeFor("locked"))
        assertEquals("Voice stopped while the app was in the background",
            voiceNoticeFor("background"))
        assertEquals(null, voiceNoticeFor("stop"))
        assertEquals(null, voiceNoticeFor("chat"))
    }
}
