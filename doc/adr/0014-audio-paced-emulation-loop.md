# 0014. Audio-paced emulation loop: real-time clock, reserve buffer, prebuffer

- **Status:** Accepted
- **Date:** 2026-06-09

## Context

The emulation core is headless and clockless: `Saturn::run_for(cycles)` advances
the deterministic machine as fast as the host allows and emits a chunk of 44.1
kHz stereo audio (`Saturn::take_audio`) per frame. *Something* in the frontend
has to decide how fast to run it so a human perceives real-time. There are two
host clocks available to pace against:

- **Display vsync** — but the SDL window's refresh rate is whatever the desktop
  is (60/75/120/144 Hz, or uncapped), unrelated to the Saturn's ~59.94 Hz NTSC
  frame. Pacing to vsync makes audio pitch/speed drift with the monitor.
- **The audio device** — a 44.1 kHz queue that the OS drains at a fixed,
  hardware-defined real-time rate, independent of the monitor. This is the one
  clock that matches the rate the Saturn's own SCSP produces samples at.

Audio glitches are also far more perceptible than a dropped video frame: a video
hitch is a flicker, an audio underrun is an audible buzz/pop. And the core's
throughput is **not** uniformly above real-time — light stages (title paint) run
well over 60 fps while heavy stages (a software-rendered "Press Start" display
list) dip below it (see [0013](0013-render-pipeline-worker-thread.md) and the
roadmap performance section). A naive "queue whatever audio this frame produced"
loop underruns on every dip → periodic "burst buzz".

## Decision

We will make **the audio device the emulator's clock**, with a tunable reserve
buffer and a prebuffer gate. In `jupiter/src/main.rs`:

1. **Audio-paced loop.** Each displayed frame, burst-advance the machine
   (`advance_frame` + queue `take_audio`) **until the SDL audio queue holds the
   target reserve**, then stop. The device drains that queue in real time, so it
   — not vsync — sets the speed. All audio is queued (nothing dropped). The burst
   is capped (`max_frames_per_burst = 2`) so a stalled device can't starve the
   render; the loop just falls slightly behind and catches up. (`0a6249c`)
2. **Tunable reserve.** The target depth is `SAT_AUDIO_MS` (default **120 ms**,
   `176_400 × ms / 1000` bytes). The reserve is pre-filled during surplus stages
   and drains across a heavy stage, so a normal pass through a sub-real-time
   screen rides through without underrunning — at the cost of that much latency.
   Larger (e.g. `SAT_AUDIO_MS=1000`) suits latency-tolerant games (visual
   novels); keep it small for twitch/action. Accuracy-neutral: the emitted
   samples are unchanged, only the host queue depth differs.
3. **Prebuffer-before-play.** The SDL device is **opened paused and resumed only
   once the queue first reaches the target** (one-shot `audio_started` gate). The
   burst keeps filling through the black boot screen while paused (the queue only
   grows), so the first sample played sits on a full reserve. (`4f44c1b`)

The reserve **delays, not cures**, an *indefinitely* sustained deficit (it drains
at the deficit rate); the real cure for a stage that is permanently below
real-time is faster compute (the [0013](0013-render-pipeline-worker-thread.md)
render thread + the interpreter/cache optimizations from the same profiling pass).

## Consequences

- **Easier:** BGM plays at correct pitch on any monitor refresh rate; per-stage
  compute dips no longer buzz (the reserve absorbs them); cold starts no longer
  underrun once (prebuffer). _Doukyuusei ~if~_'s opening BGM is glitch-free from
  the very first cold play (user-confirmed, 2026-06-09).
- **Cost we accept:** up to `SAT_AUDIO_MS` of audio latency, and a brief
  startup priming window (a few black-boot frames advance faster than real-time
  before audio starts pacing — invisible). The burst cap means motion can lurch
  by at most one extra frame when catching up after a stall.
- **Determinism untouched:** pacing lives entirely in the frontend; the core
  still runs the same deterministic stream. Headless/test paths that don't drain
  audio are unaffected (and *must* drain it — see below).
- **Follow-up / gotcha:** the SCSP mixer freezes if `take_audio` isn't drained
  each frame (its output buffer caps and `mix()` is skipped), so any loop that
  observes sound state must drain audio. A persisted config file would let
  `SAT_AUDIO_MS` move from an env var into the OSD ([0008](0008-frontend-osd-software-composite.md))
  — queued with the M9 settings work.

## Alternatives considered

- **Pace to display vsync.** Rejected: ties emulator speed to the monitor's
  refresh, so audio pitch drifts on anything that isn't a 59.94 Hz display, and
  uncapped/120 Hz monitors run the machine fast.
- **Sleep to a wall-clock frame timer (`sleep(16.6 ms - elapsed)`).** Workable
  but it paces video while audio still over/underruns against its independent
  device clock (the two clocks drift), reintroducing buzz; the audio queue depth
  is the more direct and glitch-relevant signal.
- **Fixed small audio buffer (the original ~83 ms).** What we had; it underran on
  every sub-real-time stage. The tunable reserve is a strict superset (set it to
  83 ms to reproduce the old behavior).
- **Resume audio at startup (original).** Caused exactly one cold-start underrun
  while the queue filled during boot/disc-seek; prebuffer-before-play fixes it
  with no steady-state change.
