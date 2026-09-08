# Meeting recording

Meetings start only from an explicit UI action. The default source is the configured microphone. On macOS 13 and later, the user can select microphone plus application audio for an online meeting. The latter uses ScreenCaptureKit and requires the system screen/audio recording permission. Source discovery does not request permission or start capture. Older macOS versions and Linux retain microphone recording.

The ScreenCaptureKit stream excludes Jarvis's own process and registers only an audio output. It records audio from other applications; it does not isolate a particular call application. No screen sample buffers, screenshots or video files are collected by the meeting recorder. The microphone remains owned by the shared AudioHub. Meeting recording and push-to-talk capture are mutually exclusive; wake interaction and voiced notifications are suspended until the recording ends.

Each meeting is stored under `JARVIS_DIR/meetings/<id>/` (normally `~/.jarvis/meetings/<id>/`). Directories are created with mode 0700 and audio/metadata files with mode 0600. `meeting.json` contains status and transcript segments. A microphone meeting writes `audio.wav`; an online meeting first writes `microphone.wav` and `system.wav`, then atomically creates their combined `audio.wav`. Original tracks remain available if mixing or transcription fails. Audio is mono PCM16 at 16 kHz, streamed to disk with a WAV-header checkpoint every second; memory does not grow with meeting length. One recording may last up to 12 hours.

Stopping releases capture and starts transcription in the background with the configured local STT engine. Audio is read in chunks of at most 30 seconds, with boundaries preferably placed in a nearby quiet interval. Transcript text is never inserted into another application. Completed fragments are saved as they arrive. Failed/interrupted work can be retried from the archive, including after an application restart before the online tracks were mixed. Only one meeting capture/transcription operation runs at a time.

`meetings.list` and `meetings.get` expose saved metadata and transcripts through the existing authenticated, gated local MCP bridge. Both are read-only and mark returned text as untrusted data. No MCP tool can start meeting capture.

## Limits and validation

Track/segment timestamps are approximate. System audio uses host presentation timestamps; the microphone's initial alignment uses packet arrival minus packet duration because AudioHub does not expose hardware timestamps. There is no speaker diarization or acoustic echo cancellation. A microphone may pick up the same remote voice as the system track when audio plays through speakers; headphones avoid that duplication.

Automated validation uses synthetic audio only and does not open a microphone or call ScreenCaptureKit capture. Rust tests cover long recordings, bounded transcription, partial failures, recovery, private storage, timeline gaps/overlaps and atomic mixing:

```sh
cd src-tauri
cargo test --no-default-features --bin jarvis meetings
```

On macOS the native callback can be exercised with synthetic CoreMedia buffers and a pre-cancelled start. The test verifies sample count/rate/timestamp, excludes screen outputs, and ensures cancelled/closed callbacks do not touch the Rust context:

```sh
xcrun clang -fobjc-arc -fblocks -mmacosx-version-min=11.0 \
  src/meetings/system_audio_native_test.m -framework Foundation \
  -framework CoreMedia -weak_framework ScreenCaptureKit \
  -o /tmp/jarvis-audio-test
/tmp/jarvis-audio-test
```

A real recording still needs a manual smoke test initiated by the user: allow microphone access, record and stop a microphone meeting, then explicitly select online capture and allow screen/system audio access. Verify both sides of a call in the saved WAV, switch Spaces during recording, and test stop/quit while a permission dialog is pending. No live capture was performed as part of automated implementation validation.
