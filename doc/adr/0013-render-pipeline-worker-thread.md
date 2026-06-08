# 0013. Render-pipeline worker thread (offload only the read-only render edge)

- **Status:** Accepted
- **Date:** 2026-06-08

## Context

The emulation core is **deliberately single-threaded**: `Saturn::step_cpus`
runs the whole machine in master-leads-slave lockstep (master one instruction →
slave catches up to the master's timestamp, with SCU interrupts sampled per
master instruction) as one deterministic instruction stream. This is not an
accident of implementation — it is forced by the hardware's coupling. The two
SH-2s + the SCU + work-RAM cluster interact ~every cycle: shared WRAM with **no
hardware cache coherency** (software associative-purges), **per-instruction
cross-CPU interrupts** (the FRT input-capture FTI wakes the sibling on a specific
instruction), and every memory access flowing through the SCU bus arbiter.
Accurate cross-chip ordering would force a sync at *every* shared access, which
serializes anyway — now with cross-core overhead on top — and OS-scheduled
thread interleaving is non-deterministic, which would break both the save-state
round-trip test and the entire vs-Mednafen PC-trace-diff methodology that this
project's debugging depends on. So "one thread per Saturn chip" is off the table.

At the same time, M11 game boot reached real, demanding workloads: _Doukyuusei
~if~_ renders its title at native **640×224 hi-res**, and profiling measured the
machine at ~55 % of real-time (~33 fps) at its heaviest stage — a *fidelity* gap
(the game runs in slow motion), not a luxury. One host core sat at 100 % while
the others idled. We needed more throughput **without** touching the
cycle-faithful default path or its determinism.

The key observation: **`vdp2::render_frame` is a pure read of VDP state.** It
takes a snapshot of the VDP2 registers/VRAM/CRAM and the VDP1 display
framebuffer and composites pixels; it mutates nothing the core will read back.
That makes the *render* a loosely-coupled edge that synchronizes only at frame
boundaries — exactly the kind of work that parallelizes, unlike the
sub-instruction-coupled CPU cluster. (See the "decompose by coupling, not by
component" rationale; [0002](0002-accuracy-over-performance.md) keeps accuracy
the default, and this stays within it because the emulated state is untouched.)

## Decision

We will **keep the SH-2×2 + SCU + WRAM core single-threaded, and offload only
the VDP composite to a worker thread**, synchronized at frame boundaries.

Concretely (`jupiter/src/render_pipe.rs`, commit `757f164`):

- Each displayed frame, the main thread advances the machine with
  `Saturn::advance_frame` (the compute half of `run_frame` — `run_for` with no
  composite) **while** the worker thread composites the *previous* frame.
- The frontend hands the worker a **clone** of the VDP2 state plus the VDP1
  display framebuffer (`RenderPipe::submit(&Saturn)`); the worker calls
  `vdp2::render_frame` into an output buffer and returns `(Vec<u8>, dims)` via
  `wait()`; buffers are recycled through a 1-deep handshake (`recycle`).
- The displayed frame therefore **trails the simulation by one frame** (pipeline
  latency), and the rendered pixels are **bit-identical** to the single-threaded
  path — `render_frame` is the same pure function fed the same snapshot.
- `render_pipe` is **sdl2-free and unit-tested** (submit/wait/recycle), like the
  OSD module ([0008](0008-frontend-osd-software-composite.md)); `main.rs` is the
  thin SDL wiring.

This is the **one** place multi-threading is allowed. Any proposal to thread the
*core* (CPUs, SCU, bus, WRAM) violates this ADR and must be argued as a new one.

## Consequences

- **Easier:** a second host core now does useful work; Doukyuusei 640-hi-res
  compute-only rose from ~33 to the ~67 fps range (with the companion
  accuracy-neutral interpreter/cache micro-optimizations from the same profiling
  pass — INTC O(1) pending cache `addfe06`, interrupt re-arm early-out `69b6fdf`,
  decode LUT `bc7c3c1`, cache hit-path copy elimination `6b0f907` — all
  bit-identical, `bios_boot` golden unchanged). Compute-only is the pipeline's
  displayed ceiling, so this is what the user experiences.
- **Cost we accept:** one frame of display latency, an extra VDP2-state clone per
  frame (cheap relative to the composite), and the rule that `render_frame` must
  stay a pure read — if a future VDP feature needs to write back into core state
  during compositing, it cannot run on the worker.
- **Determinism is preserved:** the core is still one deterministic instruction
  stream; the worker only reads a snapshot. Save-state round-trips and the
  vs-Mednafen PC-trace-diff are unaffected.
- **Boundary stays bright:** "never multi-thread the core" remains the invariant;
  this ADR is the explicit, narrow exception for the read-only render edge.

## Alternatives considered

- **One thread per chip (sh2-master, sh2-slave, SCU, VDP1, VDP2, SCSP).**
  Rejected: the CPU/SCU/WRAM cluster is sub-instruction-coupled, so accurate
  ordering forces a barrier at every shared access — serialized with added
  cross-core cost — and non-deterministic interleaving breaks the save-state and
  trace-diff methodologies. (Subprocesses are strictly worse: IPC per access vs
  tens-of-ns chip interactions.) This is *why* RPCS3-style chip threading only
  works for HLE/timing-approximate emulators, which the charter forbids.
- **Stay single-threaded, optimize only the interpreter.** We did this too (the
  micro-opts above), but it alone left the heavy stages sub-real-time; the render
  is a large, cleanly-separable fraction (~36 % of compute+render frame time)
  that the interpreter work can't reclaim.
- **Approximate/skip rendering under load (frame-skip the composite).** Rejected
  on the default path: it changes what the user sees and invites
  "approximate when busy" creep. Overlapping the *exact* composite keeps every
  frame faithful; it just displays one frame later.
