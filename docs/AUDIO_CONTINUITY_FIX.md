# Playback continuity, recovery and radio

This patch is integrated with upstream 0.3.3. It preserves HLS, RSS feeds,
video playback, speed control, normalization, the equalizer and limiter.

## PCM continuity

With rodio 0.22.2, converting a source whose decoder exposes short packet spans
can restart interpolation at every span. Synthetic packetized 44.1 kHz input
then differs in samples and length from the same continuous input at 48 kHz.
This is a span-boundary conversion defect, not evidence of a broken AAC codec.

`audio_source::convert` gives each finite track a separate continuous converter.
A complete-frame guard rejects truncated frames or changing format metadata.
Conversion precedes Tempo and Meter; EQ and the limiter remain on the bridge.
Conversion and Tempo construction are deferred until the source is pulled, so
initial lookahead cannot block the audio command thread.

An empty rodio player initially advertises mono / 48 kHz. Attaching it to a mixer
before appending stereo PCM can duplicate its first 512 interleaved samples.
`connect_player` primes the queue with a zero-length source in the mixer format,
retires that seed synchronously with one complete silence frame, and then
attaches the queue. It preserves normal queue spans and device buffer settings.

## Cancellation and recovery

`Player::clear` can wait for a device callback or a blocked network reader.
Replacement instead stops the old controls, interrupts their reader and creates
a freshly primed player, preserving volume and pause state. Seek and device
recovery reserve replacement readers before interrupting old ones. Decoder jobs
run off the command thread and publish only for the current generation and seek
sequence. Outgoing fades and superseded jobs are cancelled without cancelling
a replacement that shares the same download.

Partial HTTP blocks resume from the received prefix. Malformed or overlong bodies
cannot silently shift the cached stream. Playback uses shorter stall and block
deadlines than offline downloads. AAC and Opus recovery avoid indexing a missing
file tail. HLS retains segment-aware seeking; RSS keeps its existing decoder.

Recovery validates actual decoder output at the requested position and retains
the first complete PCM frame. Headers alone are insufficient. Candidate lookup
and preparation have bounded budgets and separate buffers for different formats.
URL renewal preserves the selected progressive format or HLS segment layout.
HLS remains available when later progressive byte ranges are refused.

The UI retains the track and position during recovery. Failure does not advance
the queue or switch a song to its video. Automatic retries are bounded; actual
playback progress rearms the budget, and manual play retries at the saved position.

## Radio and rendering

Radio follows YouTube Music's seed-specific watch endpoint, automix session and
continuations with bounded pagination. Music videos remain eligible; explicit
podcast episodes are excluded from music radio. Bounded account-specific RAM
history prefers fresh server recommendations when alternatives exist. No random
filler or scrobbling service is required. Quick Picks retains one-page behavior.

Lyrics reuse shaped word galleys with font/atlas and DPI cache invalidation.
The upstream translation-invariant glow cache and typography are preserved.
Animated backgrounds evaluate smooth noise on a 64 by 64 vertex grid in OpenGL
and WGSL, clamp after interpolation and avoid an unused previous-cover sample.
Static backgrounds use one triangle. Theme controls, rotation, video textures,
frosted glass, upstream pacing and fullscreen background suppression are retained.

## Verification

Run on the final source tree:

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Deterministic tests use generated PCM, synthetic AAC/Opus fixtures and local HTTP
servers. They cover conversion phase/length, queue priming, complete frames,
nonblocking DSP construction, cancellation, partial-block repair, format identity,
actual resume preparation, radio parsing/ordering and text cache invalidation.
Shader tests validate grid coverage and WGSL. See [fixture provenance](../src/testdata/README.md).

Ignored live tests require explicit execution and depend on external services.
Headless tests do not establish listening acceptance, physical-device recovery,
all-account availability or measured GPU savings. Native playback/rendering,
platform coverage and performance results must refer to the exact tested revision
in its PR or release record. Historical test counts are not current validation.

The original license and attribution remain in [LICENSE](../LICENSE) and
[README.md](../README.md).
