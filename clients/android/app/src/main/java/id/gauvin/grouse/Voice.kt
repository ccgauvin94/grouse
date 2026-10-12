// SPDX-License-Identifier: AGPL-3.0-or-later
package id.gauvin.grouse

import android.content.Context
import android.media.MediaRecorder
import android.speech.tts.TextToSpeech
import java.io.File
import java.util.Locale
import java.util.UUID

/**
 * Push-to-talk capture. One MediaRecorder clip per press, AAC in an MP4 container
 * (`audio/mp4`, which goose's dictation accepts). The clip is the WHOLE utterance —
 * goose's dictation is one-shot, there are no partials — so nothing here streams.
 *
 * VOICE_RECOGNITION rather than MIC: it asks the platform for the speech-tuned
 * source (less AGC/noise shaping that hurts transcription), and it is the source
 * that pairs with the platform echo canceller for the eventual full-duplex path.
 */
class VoiceRecorder(private val context: Context) {
    private var recorder: MediaRecorder? = null
    private var file: File? = null

    val recording: Boolean get() = recorder != null

    /** Begin a clip. Returns false if the recorder could not start (no permission, busy mic). */
    fun start(): Boolean {
        if (recorder != null) return true
        val out = File.createTempFile("grouse-voice", ".m4a", context.cacheDir)
        @Suppress("DEPRECATION") // MediaRecorder(context) is API 31+; the no-arg form works on 26+.
        val r = MediaRecorder()
        return try {
            r.setAudioSource(MediaRecorder.AudioSource.VOICE_RECOGNITION)
            r.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            r.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            r.setAudioChannels(1)
            r.setAudioSamplingRate(16000)
            r.setAudioEncodingBitRate(32000)
            r.setOutputFile(out.absolutePath)
            r.prepare()
            r.start()
            recorder = r
            file = out
            true
        } catch (e: Exception) {
            runCatching { r.release() }
            out.delete()
            false
        }
    }

    /** Stop and return the clip bytes, or null if nothing usable was captured. */
    fun stop(): ByteArray? {
        val r = recorder ?: return null
        val out = file
        recorder = null
        file = null
        return try {
            r.stop()
            r.release()
            out?.takeIf { it.exists() && it.length() > 0 }?.readBytes()
        } catch (e: Exception) {
            // stop() throws when the press was too short to capture a single frame.
            runCatching { r.release() }
            null
        } finally {
            out?.delete()
        }
    }
}

/**
 * On-device TTS. The engine warms on construction (its init is async); chunks that
 * arrive before it is ready are queued, not dropped, so the first reply is not lost
 * to a cold engine. Each chunk is one QUEUE_ADD utterance — the core already hands
 * us sentence-sized pieces, so this just plays them in order.
 */
class Speaker(context: Context) {
    private var tts: TextToSpeech? = null
    private var ready = false
    private val pending = ArrayDeque<String>()

    init {
        tts = TextToSpeech(context.applicationContext) { status ->
            ready = status == TextToSpeech.SUCCESS
            if (ready) {
                tts?.language = Locale.getDefault()
                while (pending.isNotEmpty()) speak(pending.removeFirst())
            }
        }
    }

    fun speak(text: String) {
        val t = text.trim()
        if (t.isEmpty()) return
        val engine = tts ?: return
        if (!ready) {
            pending.addLast(t)
            return
        }
        engine.speak(t, TextToSpeech.QUEUE_ADD, null, UUID.randomUUID().toString())
    }

    /** Drop anything queued or playing (a new turn, or the user talking over it). */
    fun stop() {
        pending.clear()
        tts?.stop()
    }

    fun shutdown() {
        tts?.shutdown()
        tts = null
        ready = false
    }
}
