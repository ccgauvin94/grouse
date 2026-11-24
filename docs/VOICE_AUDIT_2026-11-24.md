# Voice interaction audit — 2026-11-24

Status: **audit record, not a claim that voice is release-hardened.** The 0.3
release ships the existing voice behaviour unchanged, with the gaps below known
and unfixed. The happy paths described here were traced in code; the proposed
safety work was not implemented.

## Scope

The audit covered the core's speech/dictation surfaces, the Android voice loop
and text-to-speech playback, the desktop and macOS surfaces, and the server-side
STT/TTS assumptions the clients rely on.

## What works today

- **Core TTS feed:** `core/grouse-core/src/speak.rs` sanitizes assistant text and
  emits sentence-sized chunks through `CoreListener::on_speak(String)`. Android
  consumes those chunks with the platform `TextToSpeech`; desktop drops them.
- **Core dictation surface:** `_goose/unstable/dictation/config` and
  `_goose/unstable/dictation/transcribe` are exposed through
  `GrouseUnstable`/uniffi, with the exact wire shapes covered by core unit tests.
  There are no C ABI dictation intents, so desktop has no voice input path.
- **Android input:** the voice loop records with `AudioRecord`, uses energy-based
  voice activity detection, wraps the audio as WAV, and base64-encodes it for the
  dictation RPC. There is no wake word; the user starts each utterance explicitly.
- **Server assumptions:** dictation is expected to reach goose's configured STT
  provider (on this deployment, LocalAI's `STT-Parakeet`). TTS is entirely local
  to Android; no server TTS endpoint is used by Grouse.
- **Roam:** dictation is main-wire-only. A roam chat's typed text works, but voice
  input does not use the peer transport.

## Known gaps, by severity

These are the findings from the audit, ordered by what would hurt a user first.
They are deliberately not ranked as "bugs fixed" because no voice code changed
in 0.3.

### High — lifecycle and backgrounding

- Voice keeps running across app backgrounding and screen lock. A microphone
  loop that survives the lock screen is both a privacy problem and a battery
  problem, and the current foreground-service/wake-lock work covers live turns,
  not an active voice capture.
- There is no voice-specific wake lock. An utterance in progress can be frozen
  mid-capture or mid-transcription when the device sleeps, which is the same
  class of failure the turn wake-lock work addressed for the prompt stream.
- Voice does not stop on connection loss. If the ACP wire drops while a
  transcription request is in flight, the result has nowhere to go and the UI
  can remain in a listening state.

### High — TTS races and stale audio

- TTS utterances have no request or generation id. A late `on_speak` chunk from
  a superseded turn, a Stop action, or a chat switch can still be spoken after
  the user has moved on.
- There is no "unlocked and visible" gate for playback. Speech can start on the
  lock screen or after the app has been backgrounded.
- The utterance queue is not cancelled as one generation. Stopping a turn should
  cancel queued and speaking audio together, not merely stop accepting new
  chunks.

### Medium — request correlation and error visibility

- A dictation reply is not tied to the utterance that produced it by an explicit
  id in the client state. Rapid retakes can let an older reply be inserted into
  the current composer.
- In-flight transcriptions are not cancelled when the user cancels or restarts.
- Voice errors are not surfaced while the voice UI is active. Permissions,
  recorder failures, empty audio, an unreachable STT provider and an ACP error
  all need a visible message instead of a silent return to idle.

### Medium — permissions and parity

- Permission-denied handling is not a first-class flow. The user should see why
  voice is unavailable and how to enable it, rather than a dead button.
- Desktop and macOS have no voice input surface and no C ABI dictation intents.
  This is a parity gap, not an accidental omission, but it must be dispositioned
  in `docs/PARITY.md` if voice becomes a core-visible surface.
- TTS is Android-only; desktop discards `on_speak`. Any future TTS work on other
  clients belongs in the core contract and the parity ledger, not in a private
  UI path.

## Proposed safety pass (not started)

1. Stop voice on lock, background and disconnect; add a bounded voice wake lock
   from capture start through transcription completion.
2. Gate TTS on "app unlocked and visible"; cancel the whole utterance generation
   on Stop, chat switch, or lost wire.
3. Add utterance/request ids and generation checks to both dictation and speech.
4. Show voice errors in the active voice UI, not only in logs.
5. Fix the permission UX, then add tests and parity dispositions for every new
   surface.

## Files to start from

- `core/grouse-core/src/speak.rs` — TTS chunking.
- `core/grouse-core/src/unstable.rs` — dictation RPCs.
- `core/grouse-core/src/lib.rs` — `CoreListener` speech callback.
- Android voice loop, settings and TTS call sites under
  `clients/android/app/src/main/java/id/gauvin/grouse/`.
- `clients/desktop/src/corebridge.{h,cpp}` and `docs/PARITY.md` — the C ABI and
  the ledger if dictation/TTS ever becomes cross-client.
