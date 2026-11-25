// SPDX-License-Identifier: AGPL-3.0-or-later
package id.gauvin.grouse

/**
 * Pure voice-safety decisions. Kept free of Android framework types so the JVM
 * suite can prove every gate without an AudioRecord, a TextToSpeech engine or a
 * looper. The framework-facing code in ConnectionManager only supplies facts.
 */

/** Voice may capture or speak only while the device is unlocked, the app is in
 *  the foreground and the microphone permission has been granted. */
internal fun voicePermits(unlocked: Boolean, foreground: Boolean, permissionGranted: Boolean): Boolean =
    unlocked && foreground && permissionGranted

/** A late callback is current only when its captured generation still matches. */
internal fun voiceGenerationCurrent(current: Long, observed: Long): Boolean = current == observed

/**
 * Accept a dictation reply only while one request is pending. The core's
 * callback carries no request id, so a named reply is checked against the one
 * we issued; an unnamed reply is accepted only when there is exactly one
 * outstanding request. Strict wire-level identity needs a contract change.
 */
internal fun voiceRequestAccepts(current: String?, observed: String?): Boolean =
    current != null && (observed == null || observed == current)

/** User-visible text for a voice stop/reason. Null means the stop is silent. */
internal fun voiceNoticeFor(reason: String): String? = when (reason) {
    "permission-denied" -> "Microphone permission is off — enable it in Android settings"
    "microphone" -> "Couldn't start the microphone — check the permission"
    "gated" -> "Voice needs the app open and the phone unlocked"
    "timeout" -> "Transcription timed out"
    "empty" -> "Didn't catch that — try again"
    "wire-lost" -> "Connection lost — voice stopped"
    "locked" -> "Voice stopped while the phone was locked"
    "background" -> "Voice stopped while the app was in the background"
    else -> null
}
