---
name: performance-profile
description: Profile and analyze the performance of the 5thPlanet SEGA Saturn emulator end-to-end — from the SH-2 interpreter core through the VDP2 compositor to the SDL2 jupiter frontend — then report concrete, accuracy-safe optimization opportunities.
---

Performance is **explicitly subordinated to fidelity** in this project: a faster emulator that diverges from Mednafen/MAME is a regression, not a win. Every optimization here must be **bit-identical** (the boot golden hash and the savestate round-trip stay unchanged) and must **never** introduce a JIT, dynarec, approximate-cycle shortcut, or multi-thread the core. The goal of profiling is to close the gap to ~60 fps NTSC real-time on the heavy scenes *without touching observable behavior*. See the `threading-performance-model`, `m13-cycle-accuracy`, and `m11-game-boot-progress` memories for the campaign history and the dead ends already ruled out.

When asked to profile/analyze performance, follow these steps.

## 1. Establish the baseline — always `--release`

**The `test`/`dev` profile is unoptimized and gives meaningless numbers** (a documented pitfall — slow trace boots were partly this). Every benchmark below MUST run with `--release`. The benches live in `crates/saturn/tests/trace_boot.rs`, are all `#[ignore]`d so CI never runs them, and read a real BIOS from `bios/` + a disc from `roms/` (they print "skipped" and return if missing — note that in the report rather than failing).

Run the relevant bench(es) for the scenario the user named (default to the two canonical heavy scenes if unspecified):

- **`bench_fps`** — sustained fps on the heavy 640 hi-res Doukyuusei menu snapshot, reported **both** `run_for` (compute only) and `run_frame` (compute + composite), so the render fraction is explicit. Env: `BENCH_FRAMES` (600), `SNAP_AT`/`SNAP_FILE`, `CUE`, `BIOS`.
  `cargo test --release -p saturn --test trace_boot bench_fps -- --ignored --nocapture`
- **`bench_vf2_fight`** — input-scripted VF2 3D fight (the 704×448 double-density worst case), loaded from a cached snapshot. Env: `FIGHT_AT` (2700; the `tmp/vf2_fight_f<N>.sav` snapshot — **rebuild it after any machine-behavior fix**, a stale snapshot freezes old game state). Also prints `take_audio` samples/frame (the audio-starvation probe).
  `cargo test --release -p saturn --test trace_boot bench_vf2_fight -- --ignored --nocapture`
- **`bench_vf2_pipeline`** — advance overlapped with the banded render, the **only** bench that sees thread contention (sequential benches lie about the in-vivo rate). Respects `SAT_RENDER_THREADS`.
  `cargo test --release -p saturn --test trace_boot bench_vf2_pipeline -- --ignored --nocapture`
- **`bench_stages`** — per-stage fps curve across the opening (locates which stage dips). Env: `STAGE_FRAMES`, `WINDOW`.
- **`frame_timing`** — per-`run_frame` timing of the VF2 boot, reports the slowest frames + master PC and overruns of the 16.67 ms budget. Env: `FRAMES`, `RENDER=0` (compute only), `CUE`.

Record the **compute-only fps, the compute+render fps, the render share %, and the real-time headroom %** as the baseline. Compute-only is the render-pipeline's displayed ceiling, so it's what the user experiences.

## 2. Attribute the cost with a sampling profiler

`perf` is available (`/usr/bin/perf`); `cargo-flamegraph` is not installed (install it or fall back to raw `perf` — don't assume flamegraph). `perf` needs `perf_event_paranoid ≤ 2` (`sysctl kernel.perf_event_paranoid` to check). Profile the **release** bench binary, not `cargo test` (the wrapper noise dominates):

1. Build the bench binary: `cargo test --release -p saturn --test trace_boot --no-run` and note the emitted `target/release/deps/trace_boot-<hash>` path.
2. `perf record -g --call-graph dwarf -- <that binary> bench_vf2_fight --ignored --nocapture --exact bench_vf2_fight`
3. `perf report --stdio` (or `perf report` interactively) and read the **self-time** leaders.

Attribute every leader to one of the cost classes (the campaign's established taxonomy):
- **Interpreter core** — `Cpu::step` + decode + execute + `mem_read*` + cache fill (~half of compute; largely inherent).
- **Per-instruction overhead** — interrupt refresh/sampling (`refresh_interrupts`, `take_pending_interrupt`, `set_pending`), `drain_dma` probe, **`env::var`/`env::var_os` in a per-instruction or per-poll path** (a real cost — process-global lock + alloc; the fix is always a `OnceLock` cache).
- **Render** (`render_frame`/`render_line`) — the VDP2 compositor; this is the **blessed parallel edge**. Look for per-dot work that is frame-invariant (register/VRAM-derived values recomputed per pixel — e.g. the historical `nbg_vcp_fetch_masks` 12% and `RotationParams::read` 8.5% leaders).
- **`run_for` batch overhead** — fidelity-locked at the 256-cycle SMPC/VCNT/HBLANK poll quantum; **do NOT relax it** to gain speed.
- **Cache** — use `bench_cache` (`Cache::dbg_stats`, prints master/slave hit/miss) to decide whether `cache_fill`-class cost is the hit-path line copy (optimisable) or the cold line-fill (only ~0.1% of accesses — proven not worth it).

## 3. Profile the frontend (SDL/jupiter) separately

The frontend is a different cost domain from the core. Run the real binary with the per-second perf counters:

`SAT_PERFLOG=1 cargo run --release -p jupiter -- <bios.bin> --cue <disc.cue>`

It prints, once per second:
- **EMU thread**: `frames/s`, `burst[0/1/2]` distribution (a `burst[2]` attractor = each iteration emulating 2 frames / displaying 1 = the >16.7 ms/iteration signature), and `advance avg ms/frame`.
- **MAIN thread**: present/upload timing and the SDL audio-queue depth (the reserve, `SAT_AUDIO_MS` default 120 ms — the gauge that rides out compute dips; underrun = it hits 0 → audible crackle).

Interpret against the architecture (see `render_pipe`/`main.rs` and the `threading-performance-model` memory):
- Emu thread = `Saturn::advance_frame` (compute); render-pipeline worker composites the *previous* frame in parallel; MAIN does SDL events + audio queueing + texture upload + vsync present.
- A healthy state: MAIN locked at the display rate, EMU at/above real-time, audio queue never reaching 0. Diagnose `frames/s ≈ 60 but iters collapse to ~30` as the burst[2] attractor (per-iteration time = advance + present ≈ at the budget edge).
- `SAT_RENDER_THREADS` tunes the render band count (default `(logical_cpus/2).clamp(1,4)`; flat-8 oversubscribes and inflates the overlapped emu thread — "gameplay slows while pause hits 60"). Sweep it if the machine's core count differs from the dev box.
- Audio starvation that is **fights-only** points at the CD-DA EXTS feed / pre-roll jitter buffer or a `run_for`-vs-stepper overshoot in the sample feed (see the resolved cases in the memory), **not** raw compute — confirm with the `bench_vf2_fight` samples/frame probe before blaming the CPU.

## 4. Validate any proposed change is accuracy-neutral

Before recommending (or, if asked, applying) an optimization, the bar is:
- **Boot golden unchanged** — `cargo test -p saturn --test bios_boot` (the hash `0x0B1BA6E5180766F7`; a perf change that moves it is a behavior change, reject it).
- **Savestate round-trip unchanged** — `cargo test -p saturn --test savestate`.
- **Clippy clean** — `cargo clippy --workspace --all-targets -- -D warnings` (the enforceable gate; do NOT run `cargo fmt --all`, hand-format added lines).
- For a render change, confirm bit-identical output across band counts (the parallel composite is bit-identical *by construction* — disjoint rows of a pure function of frozen state; verify the invariant still holds).
- The core stays **single-threaded** — never propose per-chip threads/subprocesses (breaks determinism + the vs-Mednafen trace-diff methodology; the barrier problem serializes it anyway). Only the read-mostly frame-boundary edges (render; *carefully*, the SCSP mix) may parallelize.

## 5. Avoid the known dead ends

Don't re-propose these — they were measured to zero gain or are forbidden:
- **Bus dispatch reorder / HWRAM fast-path in `SaturnBus`** — zero gain; the SH-2 cache absorbs ~99.9% of HWRAM accesses, so the bus is hit only on misses + cache-through.
- **Cache line-fill optimization** — only ~0.1% of accesses miss; proven not worth it (the hit-path copy was the real win, already landed).
- **Relaxing the `run_for` batch / SMPC poll quantum** — fidelity-locked.
- **JIT / dynarec / approximate cycles / per-chip threading** — charter-forbidden.

## 6. Report

Present a **Performance Profile Report** with these sections:

1. **Baseline** — the scenes profiled, host (core count), and the measured table: compute-only fps, compute+render fps, render share %, real-time headroom %. State which BIOS/disc were used (or which were missing).
2. **Hotspot attribution** — the top self-time leaders from `perf`, each mapped to a cost class (§2) with its approximate %.
3. **Frontend** — the `SAT_PERFLOG` EMU/MAIN/audio-queue reading and what it implies (real-time held? burst attractor? audio reserve healthy?).
4. **Improvement opportunities** — a prioritized list. For each: the lever, the expected magnitude (cite the comparable historical win if one exists), the **accuracy-safety argument** (why it's bit-identical), and the rough effort. Order by `gain ÷ effort`, with anything touching observable behavior or the core's single-threadedness explicitly flagged as out-of-charter.
5. **Verification plan** — which golden/round-trip/clippy checks gate each proposed change.

Keep recommendations honest about uncertainty: "likely +N% per the FrameCtx-hoist precedent" is fair; inventing a number is not. If the heavy scenes are already at/above real-time, say so and recommend *no* change rather than micro-optimizing a met budget.
