// SPDX-License-Identifier: AGPL-3.0-or-later
package id.gauvin.grouse

import android.content.Context
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import java.io.ByteArrayOutputStream
import java.util.Locale
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import kotlin.math.max
import kotlin.math.sqrt

/**
 * Conversation mode's ears: ONE continuous AudioRecord stream, an energy VAD that
 * cuts it into utterances on trailing silence, and 16 kHz mono WAV for goose's
 * dictation (which is one-shot — a whole clip in, text out).
 *
 * Energy VAD, not a model: enough to tell speech from silence, no download, and
 * the whole loop stays on the phone. The platform echo canceller is switched on
 * with the stream — without it the TTS bleeds into the mic and the loop
 * interrupts itself.
 */
class VoiceLoop(
    private val onUtterance: (ByteArray) -> Unit,
    private val onLevel: (Float) -> Unit,
    private val onBargeIn: () -> Unit,
    private val isSpeaking: () -> Boolean,
) {
    private val sampleRate = 16_000
    private val frameSamples = sampleRate / 50          // 20 ms
    private val frameBytes = frameSamples * 2

    @Volatile private var running = false
    private var thread: Thread? = null
    private var record: AudioRecord? = null
    private var aec: AcousticEchoCanceler? = null
    private var ns: NoiseSuppressor? = null

    val active: Boolean get() = running

    fun start(): Boolean {
        if (running) return true
        val min = AudioRecord.getMinBufferSize(
            sampleRate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (min <= 0) return false
        val rec = try {
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_COMMUNICATION, sampleRate,
                AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT,
                max(min, frameBytes * 12))
        } catch (e: Exception) {
            return false
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            return false
        }
        val session = rec.audioSessionId
        if (AcousticEchoCanceler.isAvailable()) {
            aec = runCatching {
                AcousticEchoCanceler.create(session)?.apply { enabled = true }
            }.getOrNull()
        }
        if (NoiseSuppressor.isAvailable()) {
            ns = runCatching { NoiseSuppressor.create(session)?.apply { enabled = true } }.getOrNull()
        }
        record = rec
        running = true
        rec.startRecording()
        thread = Thread({ pump(rec) }, "grouse-voice").apply { start() }
        return true
    }

    fun stop() {
        running = false
        thread?.join(600)
        thread = null
        record?.let { runCatching { it.stop() }; it.release() }
        record = null
        runCatching { aec?.release() }; aec = null
        runCatching { ns?.release() }; ns = null
        onLevel(0f)
    }

    private fun pump(rec: AudioRecord) {
        val frame = ShortArray(frameSamples)
        val utterance = ByteArrayOutputStream()
        var noise = 250.0
        var onset = 0
        var trailing = 0
        var inSpeech = false
        val preroll = ArrayDeque<ShortArray>()
        val prerollMax = 12   // 240 ms of context before the first speech frame

        while (running) {
            var read = 0
            while (read < frameSamples && running) {
                val n = rec.read(frame, read, frameSamples - read)
                if (n <= 0) break
                read += n
            }
            if (read <= 0) {
                if (!running) break
                continue
            }

            var sum = 0.0
            for (i in 0 until read) {
                val v = frame[i].toDouble()
                sum += v * v
            }
            val rms = sqrt(sum / read)
            onLevel((rms / 3500.0).coerceIn(0.0, 1.0).toFloat())

            val speech = rms > max(noise * 3.5, 400.0)

            if (!inSpeech) {
                noise = noise * 0.96 + rms * 0.04
                preroll.addLast(frame.copyOf(read))
                if (preroll.size > prerollMax) preroll.removeFirst()
                onset = if (speech) onset + 1 else 0
                // While the reply is playing, require a longer onset: that is the
                // barge-in gate, and an echo leak must not trip it.
                if (onset >= if (isSpeaking()) 15 else 4) {
                    inSpeech = true
                    trailing = 0
                    if (isSpeaking()) onBargeIn()
                    for (p in preroll) writePcm(utterance, p, p.size)
                    preroll.clear()
                }
            } else {
                writePcm(utterance, frame, read)
                trailing = if (speech) 0 else trailing + 1
                if (trailing >= 38) {          // ~760 ms of silence ends the turn
                    inSpeech = false
                    onset = 0
                    val pcm = utterance.toByteArray()
                    utterance.reset()
                    if (pcm.size >= frameBytes * 8) onUtterance(wav(pcm))   // > ~160 ms
                }
            }
        }
    }

    private fun writePcm(out: ByteArrayOutputStream, samples: ShortArray, n: Int) {
        val b = ByteArray(n * 2)
        for (i in 0 until n) {
            val s = samples[i].toInt()
            b[i * 2] = (s and 0xFF).toByte()
            b[i * 2 + 1] = ((s shr 8) and 0xFF).toByte()
        }
        out.write(b)
    }

    /** 16 kHz mono 16-bit PCM wrapped in a 44-byte RIFF/WAVE header. */
    private fun wav(pcm: ByteArray): ByteArray {
        val out = ByteArrayOutputStream(44 + pcm.size)
        fun le32(v: Int) = byteArrayOf(
            (v and 0xFF).toByte(), ((v shr 8) and 0xFF).toByte(),
            ((v shr 16) and 0xFF).toByte(), ((v shr 24) and 0xFF).toByte())
        fun le16(v: Int) = byteArrayOf((v and 0xFF).toByte(), ((v shr 8) and 0xFF).toByte())
        out.write("RIFF".toByteArray()); out.write(le32(36 + pcm.size)); out.write("WAVE".toByteArray())
        out.write("fmt ".toByteArray()); out.write(le32(16)); out.write(le16(1)); out.write(le16(1))
        out.write(le32(sampleRate)); out.write(le32(sampleRate * 2)); out.write(le16(2)); out.write(le16(16))
        out.write("data".toByteArray()); out.write(le32(pcm.size)); out.write(pcm)
        return out.toByteArray()
    }
}

/**
 * On-device TTS. The engine warms on construction (its init is async); chunks that
 * arrive before it is ready are queued, not dropped. Each chunk is one QUEUE_ADD
 * utterance — the core already hands us sentence-sized pieces.
 *
 * `speaking` is the barge-in gate: the VAD asks whether audio is playing so it can
 * demand a longer onset before treating the user's voice as an interruption.
 */
class Speaker(context: Context, private val onIdle: () -> Unit = {}) {
    private var tts: TextToSpeech? = null
    private var ready = false
    private val pending = ArrayDeque<String>()
    private val outstanding = AtomicInteger(0)

    val speaking: Boolean get() = outstanding.get() > 0

    init {
        tts = TextToSpeech(context.applicationContext) { status ->
            ready = status == TextToSpeech.SUCCESS
            if (ready) {
                tts?.language = Locale.getDefault()
                tts?.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                    override fun onStart(utteranceId: String?) {}
                    override fun onDone(utteranceId: String?) { done() }
                    @Deprecated("Deprecated in Java")
                    override fun onError(utteranceId: String?) { done() }
                })
                while (pending.isNotEmpty()) speak(pending.removeFirst())
            }
        }
    }

    private fun done() {
        if (outstanding.decrementAndGet() <= 0) {
            outstanding.set(0)
            onIdle()
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
        outstanding.incrementAndGet()
        engine.speak(t, TextToSpeech.QUEUE_ADD, null, UUID.randomUUID().toString())
    }

    /** Drop anything queued or playing (a new turn, or the user talking over it). */
    fun stop() {
        pending.clear()
        outstanding.set(0)
        tts?.stop()
    }

    fun shutdown() {
        tts?.shutdown()
        tts = null
        ready = false
    }
}
