# Bootstrapping: system bring-up and game boot

How a real SEGA Saturn BIOS — run **low-level** (LLE), instruction by
instruction — brings the machine up from reset to the SEGA splash, and how it
then recognises a disc, authenticates it, and loads a game. This is the
process/sequence reference; for the chip→module map see
[`system-architecture.md`](system-architecture.md), for vocabulary see
[`glossary.md`](glossary.md), for task status see [`roadmap.md`](roadmap.md),
and for the point-in-time cross-reference audit against Mednafen see
[`mednafen-divergence-review.md`](mednafen-divergence-review.md).

**Guiding principle (ADR-0002):** every real chip is emulated cycle-by-cycle;
the BIOS is *run for real*, not high-level-emulated. The only HLE component is
the **CD-block** (its SH-1 firmware is undumped — see
[ADR-0010/0011](adr/), Superseded HLE-boot experiments aside). Because the
reference oracle (Mednafen / Beetle Saturn) is itself LLE, the whole debugging
methodology is an **LLE↔LLE master-SH-2 trace-diff**: when ours and Mednafen
both run the same real BIOS, their master PC streams should match until the
first genuine divergence, and that divergence is the bug.

---

## Part A — System bring-up (reset → SEGA splash)

This path is **disc-independent** and is exercised by the `bios_boot` golden
test (`crates/saturn/tests/bios_boot.rs`, no disc inserted).

### A.1 Reset state

`Saturn::reset` (`system.rs`) puts the machine in its power-on state:

- **Master SH-2** runs from the reset vector. PC/SP are loaded from
  `[VBR+0]`/`[VBR+4]` with `VBR = 0`; the first fetches come from BIOS ROM
  mirrored at `0x0000_0000` (and the cache-through alias `0x2000_0000`).
- **Slave SH-2 is halted.** Its `Sh2Entity::halted` flag makes
  `next_deadline()` return `u64::MAX`, so the deterministic scheduler skips it
  entirely (ADR-0004). The BIOS releases it later via SMPC `SSHON`.
- Peripherals are at their power-on register values. The **CD-block** presents
  the ASCII `"CDBLOCK"` signature in CR1–CR4 and (in this project) `HIRQ = MPED`
  (`0x0800`) — see [§B.2](#b2-the-reset-hirq-the-load-bearing-detail).

A subtle but load-bearing rule: **un-halting a CPU must resync its cycle**
(`Saturn::release_slave` bumps `pipeline.cycles` up to `now()` first), or the
scheduler sees it as millions of cycles "behind" and runs that many catch-up
steps of stale code in one batch ("time travel"). Regression:
`dual_sh2::releasing_slave_resyncs_its_cycle_no_time_travel`.

### A.2 The scheduler loop

`Saturn::run_for(cycles)` is the headless heartbeat (`run_frame` wraps it, in a
**single** `run_for(CYCLES_PER_FRAME)` + render — never split into active+VBLANK
calls, which would diverge the master's execution). Each **batch is clamped to
the next scheduled peripheral-event edge** (`batch_size` → `cycles_to_next_event`:
the next VBlank-IN, VBlank-OUT, or pending INTBACK-completion, capped by
`SMPC_POLL_QUANTUM = 256`) and steps the SH-2 pair **master-leads-slave**
(`step_cpus`: master one instruction, then slave catches up to its timestamp),
sampling the SCU interrupt line per master instruction. Between batches it:

1. `update_video_timing()` — derives `VCNT`/`TVSTAT` from the global cycle and
   raises **VBlank-IN / VBlank-OUT** on the raster edges.
2. `drain_smpc()` — runs queued SMPC commands, completes INTBACK, etc.
3. `drain_scu_dma()` / `drain_scu_dsp()` — synchronous DMA / DSP runs.
4. `drain_input_capture()` — applies inter-CPU FRT input-capture (FTI) pulses.

(SCU interrupts are *not* a between-batch drain — they're sampled per master
instruction inside `step_cpus`; the former `drain_scu_intc` was removed in
Mednafen-alignment Phase 2B.)

The edge-clamp mirrors Mednafen's `next_event_ts` model (`ss.cpp`), so interrupt
assertion and the raster registers settle at the cycle-exact point the reference
produces them — **keeping the LLE↔Mednafen trace-diff aligned** (ADR-0005, the
queue-and-drain pattern). HBlank and SCU-DMA are deliberately *not* clamp edges.

### A.3 The SMPC handshake the BIOS waits on

The BIOS will not progress until several SMPC exchanges complete; getting any of
them wrong hangs the boot:

- **`INTBACK` (`0x10`)** is *not* instantaneous — it holds `SF` busy for a
  request-dependent time (`intback_busy_us`, reconciled to Mednafen's 4 MHz
  SMPC-clock model: a status-only INTBACK ≈ 261 µs ≈ 7475 cycles) via
  `intback_complete_at`, then fills OREG, raises the SMPC interrupt, and clears
  `SF`. The BIOS polls `SF` in a wait loop and derails if it clears too early.
- **`SSHON` (`0x02`)** releases the slave SH-2 (see the resync rule above).
- **`CKCHG320/352` (`0x0E`/`0x0F`)** clock change raises the master NMI; the BIOS
  issues it during `ChangeSystemClock` early in boot.
- Command discriminants are `#[repr(u8)]` and **match the hardware codes
  exactly** — `IntBack = 0x10`, `NmiReq = 0x18`, etc. (swapping INTBACK/NMIREQ
  silently breaks boot).

### A.4 Raster timing drives the BIOS frame counter

The BIOS's main boot loop advances a frame counter off the **VBlank-OUT** SCU
interrupt (vector `0x41`). The historical M4 splash blocker was a *missing*
VBlank-OUT: without it the counter never advanced and the master parked in an
`imask=15` poll. `VCNT`/`TVSTAT` (VBLANK/HBLANK/ODD) are **live**, derived from
the global cycle in `update_video_timing`.

### A.5 The splash render

Once the BIOS programs VDP2 (TVMD display-on, NBG0 tile/bitmap, CRAM palette),
`vdp2/renderer.rs` composites the frame. The brushed-metal "SEGA SATURN" logo
is pixel-matched to MAME; the gotchas that mattered were 8bpp character base =
`char × 0x20`, the `CRAOFA/B` colour-RAM bank offset, and `NxTPON`/`R0TPON`
drawing palette code 0 as the *solid* colour `CRAM[offset]` rather than
transparent. **M1–M9 ship this path.**

---

## Part B — Game boot (CD recognition → 1st-read → gameplay)

This path is the active milestone (**M11**). The reference fixture is **Virtua
Fighter 2 (JP, GS-9079)** booted on the JP v1.01 BIOS. The boot has three
stages: BIOS-ROM disc recognition, the work-RAM CD-boot loader, and the
1st-read program (the game).

### B.1 The CD-block host interface

The CD-block is HLE (`cd_block.rs`, modelled on MAME `saturn_cd_hle.cpp`,
cross-checked against Mednafen `mednaref/src/ss/cdb.cpp`). The host (BIOS) drives
it through:

- **HIRQ** (`0x0589_0008`) — 16-bit interrupt-status word, **write-1-to-clear**
  (`hirq &= written`). Bits: `CMOK=0x01`, `DRDY=0x02`, `CSCT=0x04`, `BFUL=0x08`,
  `PEND=0x10`, `DCHG=0x20`, `ESEL=0x40`, `EHST=0x80`, `ECPY=0x100`, `EFLS=0x200`,
  `SCDQ=0x400`, `MPED=0x800`.
- **CR1–CR4** (`0x0589_0018..0024`) — command/response registers. A command is
  dispatched when all four are written (`cr_written == 0xF`); the command byte is
  `CR1 >> 8`. Responses pack the drive **status** in `CR1`'s high byte.
- **Data transfer** — a 16-bit FIFO (`0x0589_8000`) and a 32-bit SCU-DMA port
  (`0x0581_8000`).

CD **status** codes (`CR1` high byte): `BUSY=0x00`, `PAUSE=0x01`,
`STANDBY=0x02`, `PLAY=0x03`, `SEEK=0x04`, `NODISC=0x07`; `PERIODIC=0x20` is OR'd
into unsolicited periodic reports.

### B.2 The reset HIRQ — the load-bearing detail

When a disc is present, the real CD-block (and Mednafen, `cdb.cpp:4075`)
**reset-completes with the full HIRQ set**:

```
CMOK | DCHG | ESEL | EHST | MPED | ECPY | EFLS  =  0x0BE1
```

This is the value the BIOS reads *before* its first recognition command. If the
block instead presents only `MPED` (`0x0800`), the BIOS concludes it is "not
ready", issues an extra `Init(SW-reset) + GetStatus`, and the recognition state
machine **desyncs** — it then loops `AbortFile` and gives up to the CD player.

In this project the rich reset HIRQ is set in `insert_disc` (a *disc-present*
boot), **gated on disc presence**: the no-disc splash keeps the `MPED`-only
power-on HIRQ, because setting the full set at cold power-on breaks the splash
(a spurious power-on `CMOK` derails the BIOS — confirmed against the `bios_boot`
golden). This gate is why `insert_disc`, not `CdBlock::new`, owns the value.

### B.3 The recognition command sequence

The recognition runs in **BIOS ROM** (~`0x4200`, stable; commands are issued via
the helper at `0x42C4`). It is a poll-driven state machine: after each command
it polls HIRQ into a work-RAM shadow at **`[0x060003A4]`** and waits for the
expected mask + `CMOK` before advancing. The correct (Mednafen-matching) stream,
which our build now reproduces byte-for-byte, is:

```
01 GetHwInfo → 75 AbortFile → 06 EndDataXfer → 01 GetHwInfo → 67 GetCopyError
→ 48 ResetSelector → 60 SetSectorLen → 02 GetToc → 06 EndDataXfer
→ 03 GetSession ×2 → E0 Auth → E1 GetDiscRegion (= 0x0004, Saturn disc)
→ 70 ChangeDir → 75 AbortFile → 04 Init → 30 SetDeviceConnection
→ 03 GetSession ×2 → 10 Play (FAD 0x96 = 150) → 51 GetBufStat
→ 63 GetThenDeleteSector (16 sectors = IP.BIN) → 06 EndDataXfer
→ 70 ChangeDir → 72 GetFileScope → 74 ReadFile (the 1st-read) → …
```

The decisive branch is **`GetToc(02)` vs `AbortFile(75)`** after
`SetSectorLen(60)`: proceeding to `GetToc` reads the TOC and continues to
auth/Play/ReadFile; looping back to `AbortFile` retries and eventually gives up.
That branch is data-driven on the recognition's HIRQ shadow — which is why
[§B.2](#b2-the-reset-hirq-the-load-bearing-detail) is load-bearing.

**Recognition spin-up (the `Startup` drive phase, commit `e2884e7`).** A disc
present at power-on/insert does not report ready instantly: it reports
`STATUS_BUSY` for ~1 s (Mednafen `DRIVEPHASE_STARTUP`, `cdb.cpp:2175` =
`1*44100*256` CD clocks) while the pickup spins up and the TOC is read, then
settles to `PAUSE`. During that window the BIOS plays its **disc-present boot
animation** (the morphing SEGA-SATURN logo). `insert_disc` enters
`DrivePhase::Startup` reporting BUSY; crucially the host `Init (0x04)` is guarded
*not* to park the drive while it is in `Startup` — it still resets the
buffer/filter engine, but the physical pickup keeps spinning up. Reporting
`PAUSE` immediately (the old behaviour) made the BIOS see an already-ready /
door-closed drive and skip straight to the static logo with no animation; every
earlier "just report BUSY" attempt failed because the BIOS's own `Init` reset the
drive back to `PAUSE` before the window was ever observed (the symptom the user
spotted: ours jumped to the logo "as if the CD door were open"). Verified against
MAME with an audio CD inserted — ours now plays the animation, and Doukyuusei
~if~ still boots to its title. The boot *sound* over the animation was a separate
SCSP voice-keying issue, since **resolved** — see
[§B.7](#b7-the-boot--cd-player-panel-bgm-resolved-2026-06-06).

### B.4 Authentication & region

`Auth (0xE0)` is header-only HLE: it checks the `"SEGA SEGASATURN"` security
string at FAD 150 (the start of the data area), sets the auth HIRQ pattern
(incl. `ECPY`), and `GetDiscRegion (0xE1)` returns `0x0004` for a Saturn data
disc. We never read the physical security ring (the SH-1 is undumped); any drive
that reads the standard tracks + TOC authenticates.

### B.5 The work-RAM CD-boot loader

After recognition the BIOS copies a **CD-boot loader overlay into high work RAM
(`0x0602_0000+`, `GBR = 0x06020000`)** and runs it. Internals worth knowing when
trace-diffing the loader (addresses are for the JP v1.01 BIOS):

- The loader keeps a small state block at GBR. The **give-up dispatcher** at
  `0x06028106` reads byte `[0x06020002]` and, if `(byte & 0x0F) != 0`, jumps to
  the **CD player** (`JSR @0x06040000`) — the "reject the disc" outcome.
- That nibble is the **error code** copied from `[0x0601FFF0]`. The error code is
  written by an error handler (`0x060200A6`) on the *failure* path; it is `0`
  (proceed) only when recognition succeeded. (A common trap: `[0x0600022C]` near
  there only *sub-selects* error code 1 vs 8 — both non-zero, both give up — so
  it is **not** the proceed/fail gate. The gate is whether the failure path runs
  at all.)
- Several `0x0602xxxx` addresses are **overlays**: the bytes change across boot
  stages, so disassemble them *live* (at the relevant moment), not statically.

### B.6 The 1st-read handoff

On success the loader reads **IP.BIN** (FAD 150, 16 sectors; carries the
1st-read load address at `+0xF0`, size at `+0xF4`, master/slave stacks at
`+0xE8/+0xEC`), then reads the 1st-read program file (`AAAVF2.BIN` for VF2) into
work RAM at its load address and jumps to it — that PC leaving BIOS/loader space
for the game's own code is "booted".

### B.7 The boot / CD-player-panel BGM (resolved 2026-06-06)

With a disc inserted the BIOS plays its disc-present **boot animation**
([§B.3](#b3-the-recognition-command-sequence)); with no disc the multimedia
**CD-player panel** animates and plays BGM. Both exercise the same SCSP sound
driver (an MC68EC000 program the BIOS uploads to sound RAM), and for a long time
both were **silent** — the animation drew correctly but no BGM voice keyed.

**Root: an `m68k` decode bug, not a timing divergence.** `ADDA.L`/`SUBA.L Dn,An`
(opmode `0b111`) was mis-decoded as `ADDX`/`SUBX` — the ADDX dispatch guard in
`op_addsub` (`crates/m68k/src/interpreter.rs`) did not exclude opmode `0b11`. So
the sound driver's note-ring enqueue `adda.l d7,a2` never accumulated its offset,
collapsing a 9-entry command ring to 2; note-on records overwrote each other
before the player drain (`0x2162`) consumed them, so the BGM voices never keyed.
**Fix `32662f7`** (add `&& op & 0x00C0 != 0x00C0` to the dispatch); regression
`crates/m68k/tests/ring_offset_repro.rs`. Result: the audio-CD panel keys **12
voices (was 1)**, avg |amplitude| 0→111 — user-confirmed full melody. (This is
the same `ADDA`/`ADDX` decode hazard called out in the `m68k` crate notes — it
was also the SCSP-BGM-silence root.)

**Found by a cross-emulator note-ring slot diff.** Using an **audio CD** (which
the LLE oracle Mednafen *can* boot, unlike no-disc) and a mednaref `SS_SEQFIRE`
hook, Mednafen wrote 9 distinct ring slots (`0x7A00,04,08,…,20`) where ours wrote
only 2 (`0x7A00,04`) on byte-identical driver code and index — which pinned the
bad `adda.l` to an `m68k` unit test. The decisive instrument was the
config-driven cross-emulator **signal "oscilloscope"** (`Scsp::enable_scope` /
`take_scope` + `tools/scope_diff.py`, a 68k-trigger-PC timebase sampling
sound-RAM channels on both emulators and reporting the first divergent row) — the
generalization of the one-off `ENQLOG`/itrace/write-watch probes that preceded
it. See [the debugging-tooling note in `CLAUDE.md`](../CLAUDE.md) and [ADR-0012,
SCSP sound-driver HLE](adr/0012-scsp-sound-driver-hle.md).

The long hunt that preceded this fix — seq-tick-phase, WRAM-bus-timing, and a
68k-control-flow-fork hypothesis — were all downstream **symptoms** of the
collapsed ring. One real finding it surfaced is genuine but **decoupled from the
BGM**: ours under-charges the master SH-2's external-bus accesses relative to
Mednafen's shared `SH7095_mem_timestamp` model, so the master's BGM-trigger
timeline runs a phase early. That **per-access SH-2 cycle model** is its own
cycle-accuracy task (roadmap M13; large and `bios_boot`-golden-churning), not a
prerequisite for sound.

---

## Part C — The reference-diff methodology & tooling

### C.1 Oracles

- **Mednafen / Beetle Saturn** (`mednaref/`) — the accuracy reference for
  *game-level* behaviour (it boots the commercial library). Authoritative for
  M11.
- **MAME** (`mameref/`) — the low-level / early-boot reference. Authoritative for
  CPU/bus/peripheral mechanics; limited game compatibility.
- **Yabause** (`yabref/`) — secondary opinion.

All three are local, **never-committed** (gitignored), behavioural references
only — no emulator code is included or derived.

> **MAME-vs-Mednafen tension.** The two disagree on CD-block conventions
> (power-on/reset HIRQ, DCHG stickiness, `is_cdrom` semantics). The splash was
> matched to MAME; the *game boot* must match Mednafen. Several M11 fixes are
> exactly this re-alignment — keep both the `bios_boot` golden (MAME-shaped, no
> disc) and the Mednafen disc-present path green.

### C.2 Our instrumentation (all env-gated, off by default)

| Env / tool | What it does |
| --- | --- |
| `SAT_WWATCH=0xADDR` (`bus.rs`) | bus-level write-watch: logs `addr, width, value, AccessKind, cycle, pc` for any write covering ADDR. `AccessKind::Dma` vs `Data` distinguishes a DMA engine from a CPU store. The single chokepoint all writers pass through (both SH-2s' stores + on-chip DMAC, SCU-DMA, SCU-DSP-DMA). |
| `CD_TRACE` / `CD_RWATCH` (`cd_block.rs`) | per-command CD trace / HIRQ-read watch. |
| `dump_giveup_state` (`tests/trace_boot.rs`) | **the workhorse**: no-render `run_for` + a master breakpoint; stops at any boot PC and dumps regs, live code, loader-state words, the CD command-history ring (`cmd_log`), and an optional live `DISASM_FROM` range. `FRAMES`/`GIVEUP_PC`/`CMD_LOG_TAIL`/`DISASM_FROM` envs. |
| `gen_vf2_pc_trace` (`tests/trace_boot.rs`) | collapsed master-PC trace through the aligned `run_for_traced`; `PCTRACE_LO` filters before the loop-collapse window (matches Mednafen's `SS_PCTRACE_LO`). |
| `CdBlock::cmd_log` | gated, `#[serde(skip)]` command-history ring (cmd, CR in, CR out, HIRQ, status). |

Mednafen side: `SS_PCTRACE`/`SS_PCTRACE_LO`/`SS_PCTRACE_N` (master PC, same
loop-collapse format as ours), `SS_CDTRACE` (per-command, `fflush`ed), and
`SS_WWATCH`/`SS_WWATCH_OUT` (work-RAM write-watch). Invoke headless:

```sh
SDL_VIDEODRIVER=dummy SDL_AUDIODRIVER=dummy \
  SS_CDTRACE=/tmp/mdfn_cd.log mednaref/src/mednafen -sound 0 roms/vf2_full.cue
```

### C.3 Harness constraints (so you don't fight them)

- This environment **kills any single command running longer than ~8 s**
  (signal 16 / exit 144). `run_for`-based tests (no rendering) fit ≈ 700 frames
  in the budget; `run_frame` (renders) and long Mednafen runs get killed.
- `SS_CDTRACE` survives the kill (it `fflush`es per command); `SS_PCTRACE`
  buffers, so a long Mednafen PC trace usually loses everything on the kill —
  prefer the command trace for the recognition window.
- Headless runs **must** build `-p jupiter --no-default-features` (passing
  `--no-default-features` as a *runtime* arg silently opens the SDL window).

---

## Part D — Bring-up gotchas distilled

Hard-won, each with a regression or golden behind it:

1. **Pad wire format** — the SMPC digital-pad bit order is the canonical SGL
   `PER_DGT_*` layout; a bit-reversed table makes "Left" read as "C".
2. **HIRQ bits are W1C, not read-cleared** — `CSCT` and `DCHG` stay set until the
   host writes HIRQ. Read-clearing `DCHG` (MAME's `hirq_r`) left the recognition
   shadow missing it; Mednafen keeps it.
3. **`is_cdrom` is read-based, not position-based** — the CR1 bit 7 is set only
   once the read pump reads a *data* sector during PLAY, cleared at Init/insert
   and on audio; a `track_at_fad` position lookup reads `1` prematurely during
   recognition and diverges the BIOS.
4. **The reset HIRQ must be `0x0BE1` for a disc-present boot** (incl. `ECPY`) but
   only `MPED` for the no-disc splash — see
   [§B.2](#b2-the-reset-hirq-the-load-bearing-detail).
5. **`NODISC` (0x07), not `PAUSE`, when empty** — an empty drive reports NODISC;
   a loaded idle disc reports PAUSE.
6. **Cache coherency is software's job** — DMA does not snoop the SH-2 cache;
   both we and the references rely on the BIOS purging it. A stale I-cache line
   surfaces as the master fetching old code where RAM has new code.

---

## Status (M11, as of this writing)

**VF2 and Doukyuusei ~if~ now boot to their own game code.** Ours matches
Mednafen's command stream through recognition (GetHwInfo → … → Auth →
GetDiscRegion → ChangeDir) and on into the boot — **Play (IP.BIN) → ChangeDir
(`00ffffff`) → GetFileScope → ReadFile** — streaming the 1st-read program; the
master reaches VF2's load address `0x06004000` and executes there.

The final blocker was a CD-block one: the host interface re-raised **`DCHG`
(Disc Changed) on the first `Init` after recognition**, because the internal
`disk_changed` latch was cleared only inside the Init handler — so that Init
reported a fresh disc swap and the BIOS looped recognition forever instead of
booting. Fix: **clear `disk_changed` when the host write-1-to-clear-acknowledges
`DCHG`** (matching Mednafen, which clears `DCHG` once during recognition and
never re-raises it at Init). It was found by a command-level CD trace-diff: the
BIOS code is identical on both LLE sides, so the root had to be a differing CD
response — and the only divergence was ours' Init leaving `DCHG` set (`0FC4 →
0FE5`) where Mednafen left it clear (`0F84`). Post-boot, two more fixes (the
BCR1 master/slave bit, so an `SSHON`-released slave doesn't re-init WRAM over the
running game; and `run_frame` running the whole frame in one `run_for`) let VF2
run and stream its CD asset load. It then stalls mid-load: a Mednafen dev-build
CD trace-diff proved the CD layer byte-identical to Mednafen, so the remaining
blocker is **scheduler/interrupt-timing accuracy** (Mednafen-alignment Phase 2
landed — master-leads interleave + per-instruction SCU sampling — Phase 3
remains). See [`roadmap.md`](roadmap.md) M11 and the memory log for the
trace-by-trace history.

**Boot fidelity (latest, `e2884e7`):** with a disc inserted the BIOS now plays
its disc-present **boot animation** — the recognition spin-up (the `Startup`
drive phase, [§B.3](#b3-the-recognition-command-sequence)) holds `STATUS_BUSY`
for ~1 s so the BIOS animates instead of jumping straight to the static logo.
The animation's **silence is resolved** (an `m68k` `ADDA`/`ADDX` decode bug, fix
`32662f7`) — see [§B.7](#b7-the-boot--cd-player-panel-bgm-resolved-2026-06-06).
