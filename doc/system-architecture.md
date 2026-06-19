# System Architecture

How the SEGA Saturn hardware maps onto this project's code. This is the
orientation map: for each real chip, bus, and memory region it points at the
crate/module/file that models it. For Saturn-specific vocabulary see
[`glossary.md`](glossary.md); for task-by-task status see
[`roadmap.md`](roadmap.md); for how the BIOS brings the machine up from reset to
the splash and then boots a game see [§9, Bootstrapping](#9-bootstrapping-system-bring-up-and-game-boot);
for *why* a structural choice was made see the ADRs in [`adr/`](adr/).

The guiding axis is **accuracy over performance** (ADR-0002): real chips are
low-level-emulated (LLE) cycle-by-cycle; no JIT, dynarec, or "approximate
cycle". The one deliberate exception is the **CD-block, which is HLE** — its
SH-1 firmware is undumped and half its job is an analog servo with no digital
ground truth, so there is nothing to be cycle-accurate against. The BIOS itself
is run for real (LLE); an opt-in HLE direct boot was tried and removed
(ADR-0010/0011 Superseded).

---

## 1. The Saturn at a glance

The Saturn is a multiprocessor console: **eight programmable processors** plus
fixed-function video/audio hardware, tied together by the **SCU** (System
Control Unit) across three buses. The hard part — and the reason this project is
built one chip at a time — is that their timing is tightly coupled.

```
              ┌─────────────┐     ┌─────────────┐
   CPU bus →  │  SH-2 master│     │  SH-2 slave │  ← 2× Hitachi SH7604 @ ~28.6 MHz
              └──────┬──────┘     └──────┬──────┘
                     │                   │
              ┌──────┴───────────────────┴──────┐
              │              SCU                 │  System Control Unit:
              │  (DMA ×3, interrupt aggregator,  │  glue between CPU bus,
              │   embedded 32-bit SCU-DSP)       │  A-bus and B-bus
              └───┬───────────────┬──────────────┘
       A-bus →    │               │   ← B-bus
        ┌─────────┴──┐   ┌────────┴─────────────────────────────┐
        │ cartridge  │   │ VDP1   VDP2   SCSP        CD-block    │
        │ slot       │   │ sprite bg/    sound: M68k SH-1 (HLE), │
        │            │   │ /poly  compos.+SCSP-DSP  + CD-ROM FS  │
        └────────────┘   └──────────────────────────────────────┘
                     ┌──────────┐
   low-speed bus →   │   SMPC   │  System Manager & Peripheral Control
                     │  (+ RTC, │  (reset/clock, pad input, backup RAM
                     │   pads)  │   control, slave on/off)
                     └──────────┘
```

The eight processors: **2× SH-2** (master + slave), the **MC68EC000** inside the
SCSP, the **SCU-DSP**, the **SCSP-DSP**, the **SH-1** in the CD-block, and the
VDP1/VDP2 command processors (modeled as fixed-function engines here, not as
programmable cores).

---

## 2. Processors → code

| Real processor | Role | Project home | LLE/HLE |
|---|---|---|---|
| SH-2 master (SH7604) | Main CPU | `crates/sh2/` core, scheduled as `Sh2Entity` (master) in `crates/saturn/src/scheduler.rs` | LLE, cycle-accurate |
| SH-2 slave (SH7604) | 2nd CPU (graphics/physics offload) | same `crates/sh2/` core, 2nd `Sh2Entity`; held halted at power-on, released by SMPC `SSHON` | LLE |
| MC68EC000 | SCSP sound CPU | `crates/m68k/` core, hosted inside `crates/saturn/src/scsp/mod.rs` in sound RAM; released by SMPC `SNDON` | LLE |
| SCU-DSP | SCU matrix/vector DSP | `crates/scu_dsp/` core, driven from `crates/saturn/src/system.rs` (`drain_scu_dsp`/`exec_dsp_dma`) | LLE (ADR-0006) |
| SCSP-DSP | Audio effects DSP | `crates/saturn/src/scsp/dsp.rs` | LLE |
| SH-1 (CD-block) | CD-ROM controller firmware | **HLE** — not a core; `crates/saturn/src/cd_block.rs` models the host interface + buffer/filter engine + filesystem | **HLE** (undumped firmware) |
| VDP1 | Sprite/polygon command processor | `crates/saturn/src/vdp1/` (command-list plotter) | engine model |
| VDP2 | Background/compositor | `crates/saturn/src/vdp2/` (multi-layer compositor) | engine model |

### SH-2 core (`crates/sh2/`)

`no_std + alloc`, library-shaped, no I/O — reusable and unit-testable in
isolation. Pieces:

- `bus.rs` — the `Bus` trait, the only trust boundary; each access returns
  `(value, stall_cycles)` so the host owns wait-state math (ADR-0003).
- `isa.rs` / `decoder.rs` — one `Op` per encoding; pure table-driven decode.
- `interpreter.rs` — `Cpu::step()`: interrupt check → fetch → decode → interlock
  → execute → cycle-accumulate. Centralises delay slots and the
  cached/cache-through/on-chip memory routing (`classify`).
- `pipeline.rs` — 5-stage interlock + cycle scoreboard. `cache.rs` — 4-way
  write-through I/D cache. `exceptions.rs` — vector dispatch via VBR.
- `onchip/` — the SH7604 on-chip peripherals at `0xFFFFFE00+`: `intc` `bsc`
  `dmac` `frt` `wdt` `sci` `ubc` `divu`. The **FRT** (`frt.rs`) input-capture is
  load-bearing for inter-CPU signalling (§6).

---

## 3. Buses and the system memory map

After the SH-2 strips the cache indicator (`classify`): `0x0000_0000..0x1FFF_FFFF`
is cached, `0x2000_0000..0x3FFF_FFFF` is the same memory cache-through,
`0xFFFF_FE00+` is on-chip (never hits the external bus). Everything else is the
Saturn bus.

`crates/saturn/src/bus.rs` (`SaturnBus`, which `impl sh2::Bus`) is the single
dispatcher: every access is a `match addr` against region constants. Unmapped
addresses are open bus (read 0, drop writes). The region constants are the
source of truth; this table mirrors them:

| Range (physical) | Region | Code |
|---|---|---|
| `0x0000_0000..0x000F_FFFF` | BIOS ROM (mirrored) | `memory::BiosRom` |
| `0x0010_0000..0x0017_FFFF` | SMPC / system regs | `smpc::Smpc` |
| `0x0018_0000..0x001F_FFFF` | Internal backup RAM (32 KiB, odd-byte packed) | `memory::BackupRam` |
| `0x0020_0000..0x002F_FFFF` | Low work RAM (1 MiB) | `memory::Ram` (`low_wram`) |
| `0x0040_0000..0x004F_FFFF` | Sound-area stub | `memory::StubRegisterBank` |
| `0x0100_0000..0x017F_FFFF` | **Slave FRT input-capture (FTI)** trigger | `bus.rs` → `drain_input_capture` (§6) |
| `0x0180_0000..0x01FF_FFFF` | **Master FRT input-capture (FTI)** trigger | `bus.rs` → `drain_input_capture` (§6) |
| `0x0200_0000..0x04FF_FFFF` | Cartridge slot (A-bus); cart-ID at `0x04FF_FFFF` | `cartridge::Cartridge` |
| `0x0500_0000..0x05FF_FFFF` | A-bus + B-bus window (sub-decoded below) | `abus_bbus` stub + the chips below |
| `0x0581_8000` (data) / `0x0589_0000..0x0589_FFFF` (regs) | CD-block 32-bit data port + host interface (HIRQ/CR1–4) | `cd_block::CdBlock` |
| `0x05A0_0000..0x05AF_FFFF` | SCSP sound RAM (512 KiB, mirrored) | `scsp::Scsp` (`ram`) |
| `0x05B0_0000..0x05BF_FFFF` | SCSP control/slot/DSP registers | `scsp::Scsp` (`ctrl`) |
| `0x05C0_0000` / `0x05C8_0000` / `0x05D0_0000` | VDP1 VRAM / framebuffer / registers | `vdp1::Vdp1` |
| `0x05E0_0000` / `0x05F0_0000` / `0x05F8_0000` | VDP2 VRAM / CRAM / registers | `vdp2::Vdp2` |
| `0x05FE_0000..` | SCU registers (DMA/INTC/DSP/timers) | `scu::Scu` |
| `0x0600_0000..0x06FF_FFFF` | High work RAM (1 MiB, mirrors every 1 MiB) | `memory::Ram` (`high_wram`) |

Each region struct owns its bytes with big-endian `read*/write*` at *region-local*
offsets and folds out-of-range offsets modulo its size (so a smaller image
mirrors transparently).

**Bus timing (M12)** is no longer a flat per-region default — it is a faithful
Mednafen `BSC_BusRead/Write` port (`BusTiming` in `bus.rs`, serialized since
savestate v6). A **shared bus timestamp** both CPUs sync to makes CPU↔CPU
arbitration emerge (Mednafen's `SH7095_mem_timestamp`); **CS0 is a 16-bit bus**
with per-transaction costs (a 32-bit access pays twice); **CS3 high WRAM is
32-bit SDRAM** (read +7, write +2 with an array-busy window; a cache-line fill is
one free burst); the **SH-2 write buffer** lets a lone store return 0 stall;
**bus turnaround** costs +1. The **B-bus** uses an exact deferred-write model — a
write hands off in +2 cycles and posts its device-side completion (SCSP +17/+13,
VDP1 +9/+1, VDP2 +3/+1 per 16-bit half), which only the *next* B-bus access waits
out; a B-bus read is always two 16-bit halves (VDP1 +28, VDP2 +40, **SCSP +48**).
SCU-DMA arbitration follows Mednafen's `dma_time_thing` costs, and a
C-bus-endpoint SCU DMA halts **both** SH-2s for its paced duration.

---

## 4. The system glue (`crates/saturn/src/system.rs`)

`Saturn` owns the bus + the scheduler + the master/slave/CD entity IDs. It is
the surface the frontend holds.

- **`scheduler::Scheduler<E>`** (`scheduler.rs`) is an event-driven, deterministic
  scheduler (ADR-0004): each `SchedEntity` reports `next_deadline()` (its next
  global cycle) and the most-behind entity steps; ties break by insertion order
  (master, slave, CD-block). A halted entity reports `u64::MAX` so it is skipped
  — and un-halting **resyncs its cycle** to `now()` so it can't "time-travel".
  The **live SH-2 pair, though, is stepped master-leads-slave** by
  `Saturn::step_cpus` (Mednafen's `CPU[0].Step` + `RunSlaveUntil` order; Phase
  2A), not by this most-behind rule — which is kept for the CD-block timer and
  the determinism unit test.
- **`run_for(cycles)`** is the headless loop: each batch (clamped to the next
  scheduled event edge — VBlank-IN/-OUT or INTBACK, `SMPC_POLL_QUANTUM = 256`
  ceiling) steps the SH-2 pair master-leads-slave, sampling the SCU IRL per
  master instruction; between batches it drains the queued side effects.
  **`run_frame(out)`** runs one NTSC frame (`CYCLES_PER_FRAME = 479_151`,
  `LINES_PER_FRAME = 263`, `ACTIVE_LINES = 224`) in a **single
  `run_for(CYCLES_PER_FRAME)`** then renders — it must not split the frame into
  active+VBLANK calls (the split re-anchors the batch grid and diverges the
  master's execution from the headless path).
- **Queue-and-drain** (ADR-0005): peripherals can't reach the CPUs across the
  bus borrow, so they flag a side effect and `Saturn` drains it at the batch
  boundary: `update_video_timing` (raster regs + VBlank-IN), `drain_smpc`,
  `drain_scu_dma`, `drain_scu_dsp`, `drain_vdp1`, `drain_scsp`, and
  `drain_input_capture` (the inter-CPU FRT pulse, §6). SCU interrupts are *not*
  a between-batch drain — they're sampled per master instruction in `step_cpus`
  (Phase 2B; the former `drain_scu_intc` was removed).

The **SMPC** (`smpc.rs`) is the low-speed controller: reset/clock, the RTC,
peripheral input via `INTBACK`, and slave/sound on-off (`SSHON`/`SSHOFF`,
`SNDON`). Controller ports are selectable (`PortDevice::{None,Pad,Mouse}`): the
digital pad (ID `0x02`) or the **Shuttle Mouse** (ID `0xE3`, three data bytes;
`Saturn::feed_mouse`, jupiter `--mouse`). The **SCU** (`scu.rs`) holds the 3 DMA
channels, the interrupt mask/status (IMS/IST), and the timers, and aggregates
interrupts toward the master — including the **CD-block external interrupt**
(`Source::Cd`, IST bit 16, vector `0x50`, level 7), a level driven by
`Scu::set_cd_int` from `CdBlock::irq_active()` and masked by IMS bit 15 (§7).

---

## 5. Video and audio pipelines

**Video:** VDP1 (`vdp1/plotter.rs`) rasterises its command list into a
double-buffered framebuffer; VDP2 (`vdp2/renderer.rs`) composites NBG0–3 and
RBG0/1 (rotation in `vdp2/rotation.rs`) by priority, plus the VDP1 sprite layer,
with colour calculation, windows, and per-line scroll/zoom. `Saturn::run_frame`
calls `vdp2::render_frame(...)`, which renders at the **active resolution decoded
from TVMD** (320/352/640/704 wide × 224/240/256 tall, ×2 for double-density
interlace) and returns those dims; the output buffer is sized for the maximum
(`MAX_FRAME_WIDTH 704 × MAX_FRAME_HEIGHT 512`, RGBA8888). VDP1 always plots at its
native horizontal resolution, so in the 640/704-dot modes each VDP1 framebuffer
dot occupies two display dots.

**Audio:** the SCSP (`scsp/mod.rs`) is a 32-slot FM/PCM engine + the SCSP-DSP
(`scsp/dsp.rs`) + the hosted MC68EC000 in sound RAM. `Saturn::take_audio` drains
the mixed 44.1 kHz stereo each frame. **CD-DA enters as the SCSP's EXTS digital
inputs** (M11 — *not* an aggregate-level sum): each batch `Saturn::run_for` feeds
the SCSP exactly the CD samples it will consume (`cd_need`/`feed_cd`), and the
SCSP mixes them at the game-programmed levels (slots 16/17's effect-return is the
CD volume; the effect DSP can read EXTS as inputs IRA `0x30`/`0x31`). The earlier
full-level aggregate sum played BGM 10–20× over the game's mix and drowned SFX.

**Frontend pipeline (`jupiter/`):** the SDL2 frontend overlaps work across two
cores — `main.rs` advances the machine (`Saturn::advance_frame`) while a
`render_pipe` worker composites the *previous* frame (the displayed frame trails
by one; pixels are bit-identical). **Audio is the pacer, not vsync**: the device
stays paused until the 44.1 kHz queue first holds `SAT_AUDIO_MS` (default 120 ms)
of reserve, then `main.rs` bursts emulated frames to keep that reserve filled.

---

## 6. Inter-CPU signalling (FRT input-capture)

The two SH-2s wake each other through the **free-running timer's input-capture
pin (FTI)**: a **16-bit** write to `0x0100_0000..0x017F_FFFF` pulses the *slave*'s
FTI, `0x0180_0000..0x01FF_FFFF` the *master*'s, latching FRC→FICR and setting
`FTCSR.ICF`. The bus can't reach the cores, so `SaturnBus::write16` flags it and
`Saturn::drain_input_capture` pulses the target `sh2::onchip::Frt::input_capture`.
This is the dispatch wake a game uses to hand work to its slave (e.g. VF2's slave
polls `FTCSR.ICF`). See [`glossary.md`](glossary.md) "FTI inter-CPU signalling".

---

## 7. CD-block (HLE) and the boot paths

**CD-block** (`cd_block.rs`) models the host command interface (HIRQ/CR1–4), a
200-block pool + 24 filters/partitions, a 75 Hz read pump, the 16-bit FIFO +
32-bit SCU-DMA data port, the ISO9660 filesystem, and disc authentication —
reading sectors through a `disc::SectorSource` (an in-memory image, or a live
optical drive via the feature-gated `crates/physdisc/` libcdio crate, ADR-0009,
the project's only `unsafe`). It drives the **SCU external interrupt 0**
(`Source::Cd`, vector `0x50`, level 7) as a level — `irq_active()` =
`(HIRQ & HIRQ_Mask) != 0`, sampled per master instruction (M11, §4) — and models
the disc-recognition spin-up (`DrivePhase::Startup`: ~1 s `STATUS_BUSY` before
settling to PAUSE).

**Boot — LLE only:** run the real BIOS ROM; it authenticates the disc,
region-checks, reads IP.BIN, loads the 1st-read program, and jumps to it (or
drops to its CD player). This is the path the `bios_boot` golden test pins; the
full reset→splash→game-boot sequence (the recognition command stream, the reset
HIRQ, the 1st-read handoff) is traced in [§9, Bootstrapping](#9-bootstrapping-system-bring-up-and-game-boot).
**M11 (boot a game) is complete** (tag `vf2-good-emulation`): Virtua Fighter 2 is
fully playable at a steady 60 fps (looping CD-DA BGM, balanced SFX, full 3D
fights) and Doukyuusei ~if~ is fully playable at native 640×224 hi-res (GFX, SFX,
and voices). (An opt-in HLE direct boot + HLE BIOS SYS-call library was tried and
removed — ADR-0010/0011 Superseded — because the reference oracle is itself LLE,
so a valid PC-trace-diff needs LLE↔LLE.)

**Save states** (`savestate.rs`, M8; format v9): a `bincode` snapshot of the
whole machine; external media (BIOS/disc/ROM-cart bytes) is referenced not
embedded, re-grafted on load and validated by an FNV-1a fingerprint. **Backup
RAM** persists to a host file.

---

## 8. Workspace layout

| Crate / dir | Contents |
|---|---|
| `crates/sh2/` | SH7604 core (`no_std + alloc`), on-chip peripherals |
| `crates/m68k/` | MC68EC000 core (SCSP sound CPU) |
| `crates/scu_dsp/` | SCU's embedded 32-bit DSP (ADR-0006) |
| `crates/saturn/` | System glue: bus, scheduler, SMPC, SCU, VDP1, VDP2, SCSP, CD-block, cartridge, save states |
| `crates/physdisc/` | Live optical-drive `SectorSource` via libcdio; the sole FFI/`unsafe` crate (ADR-0009) |
| `jupiter/` | SDL2 frontend (window + audio) or headless; the `osd/` in-window menu (ADR-0008) |
| `doc/` | This file, [`roadmap.md`](roadmap.md), [`glossary.md`](glossary.md), [`adr/`](adr/) |

The root `Cargo.toml` is a `[workspace]` (resolver 3, edition 2024) with
`unsafe_code = "forbid"` set workspace-wide (ADR-0007); `physdisc` is the only
crate that opts out, with justification.

---

## 9. Bootstrapping: system bring-up and game boot

§§1–8 are the chip→module *map*; this section is the *process/sequence* view —
how a real SEGA Saturn BIOS, run **low-level** (LLE) instruction by instruction,
brings the machine up from reset to the SEGA splash, then recognises a disc,
authenticates it, and loads a game. It overlaps the architecture sections by
design (the scheduler loop is §4, the inter-CPU FRT wake is §6, the CD-block
model is §7); here those pieces are traced in boot order.

**Guiding principle (ADR-0002):** every real chip is emulated cycle-by-cycle;
the BIOS is *run for real*, not high-level-emulated. The only HLE component is
the **CD-block** (its SH-1 firmware is undumped — see [ADR-0010/0011](adr/),
Superseded HLE-boot experiments aside). Because the reference oracle (Mednafen /
Beetle Saturn) is itself LLE, the whole debugging methodology is an **LLE↔LLE
master-SH-2 trace-diff**: when ours and Mednafen both run the same real BIOS,
their master PC streams should match until the first genuine divergence, and that
divergence is the bug. The deliberate divergences from the secondary references
(MAME / Yabause) are consolidated in
[§C.1](#deliberate-divergences-from-mame--yabause--do-not-regress).

### Part A — System bring-up (reset → SEGA splash)

This path is **disc-independent** and is exercised by the `bios_boot` golden
test (`crates/saturn/tests/bios_boot.rs`, no disc inserted).

#### A.1 Reset state

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

#### A.2 The scheduler loop

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

#### A.3 The SMPC handshake the BIOS waits on

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

#### A.4 Raster timing drives the BIOS frame counter

The BIOS's main boot loop advances a frame counter off the **VBlank-OUT** SCU
interrupt (vector `0x41`). The historical M4 splash blocker was a *missing*
VBlank-OUT: without it the counter never advanced and the master parked in an
`imask=15` poll. `VCNT`/`TVSTAT` (VBLANK/HBLANK/ODD) are **live**, derived from
the global cycle in `update_video_timing`.

#### A.5 The splash render

Once the BIOS programs VDP2 (TVMD display-on, NBG0 tile/bitmap, CRAM palette),
`vdp2/renderer.rs` composites the frame. The brushed-metal "SEGA SATURN" logo
is pixel-matched to MAME; the gotchas that mattered were 8bpp character base =
`char × 0x20`, the `CRAOFA/B` colour-RAM bank offset, and `NxTPON`/`R0TPON`
drawing palette code 0 as the *solid* colour `CRAM[offset]` rather than
transparent. **M1–M9 ship this path.**

---

### Part B — Game boot (CD recognition → 1st-read → gameplay)

This path is **M11 (complete — tag `vf2-good-emulation`)**. The reference
fixture is **Virtua Fighter 2 (JP, GS-9079)** booted on the JP v1.01 BIOS. The
boot has three stages: BIOS-ROM disc recognition, the work-RAM CD-boot loader,
and the 1st-read program (the game).

#### B.1 The CD-block host interface

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

#### B.2 The reset HIRQ: the load-bearing detail

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

#### B.3 The recognition command sequence

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

#### B.4 Authentication & region

`Auth (0xE0)` is header-only HLE: it checks the `"SEGA SEGASATURN"` security
string at FAD 150 (the start of the data area), sets the auth HIRQ pattern
(incl. `ECPY`), and `GetDiscRegion (0xE1)` returns `0x0004` for a Saturn data
disc. We never read the physical security ring (the SH-1 is undumped); any drive
that reads the standard tracks + TOC authenticates.

#### B.5 The work-RAM CD-boot loader

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

#### B.6 The 1st-read handoff

On success the loader reads **IP.BIN** (FAD 150, 16 sectors; carries the
1st-read load address at `+0xF0`, size at `+0xF4`, master/slave stacks at
`+0xE8/+0xEC`), then reads the 1st-read program file (`AAAVF2.BIN` for VF2) into
work RAM at its load address and jumps to it — that PC leaving BIOS/loader space
for the game's own code is "booted".

#### B.7 The boot / CD-player-panel BGM (resolved 2026-06-06)

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

### Part C — The reference-diff methodology & tooling

#### C.1 Oracles

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

##### Deliberate divergences from MAME / Yabause — do not regress

The system layer was built walking several references (Yabause → MAME → Mednafen),
each with different conventions. The points below are places where ours
**deliberately** follows Mednafen / real hardware where MAME (the secondary
reference) differs — so a future "align to MAME" impulse must not silently break
the Mednafen game-boot path. Each is documented with its per-item rationale in
`CLAUDE.md`; this is the consolidated guard-list (it absorbs the retired
2026-06-08 MAME/Mednafen cross-reference audits).

- **CD reset/Init HIRQ** = full `0x0BE1` on disc-present reset (not MAME's
  `CMOK|ESEL|EHST`) — see [§B.2](#b2-the-reset-hirq-the-load-bearing-detail).
- **HIRQ reads are sticky/W1C** — reads never clear `DCHG`/`CSCT`/`BFUL` (MAME
  read-clears them); the host's `DCHG` W1C **also clears the internal
  `disk_changed` latch**, so a later `Init` doesn't re-raise `DCHG` (the actual
  M11 boot root).
- **Get HW Info** reports **no MPEG** (`CR2=0x0002`); MAME's MPEG-present byte
  triggers the BIOS auth probe that loops recognition.
- **`DrivePhase::Startup`** holds `STATUS_BUSY` ~1 s then settles PAUSE; **empty
  drive reports `NODISC` (0x07), not PAUSE** — both match Mednafen/hardware.
- **Dual-SH-2 = master-leads-slave per instruction** (not MAME's global scheduler
  + MINIT/SINIT quantum boost); preserves timing-sensitive inter-CPU WRAM handoffs.
- **SCU interrupt is a level sampled per master instruction** (internal `0x40+`,
  external CD `0x50`, IMASK reset `0xBFFF`, bit-15 sign-extend, AIACK/`cd_prohibit`).
- **`SSHON` cold-resets the slave** (VBR=0, reset vector) + sets its BCR1
  master/slave bit; an LLE slave must cold-boot, not resume stale state.
- **INTBACK** uses Mednafen's 4 MHz SF-phasing (~261 µs status-only via
  `intback_complete_at`) and status SR `(SR&~0xA0)|0x0F|NPE`; MAME clears SF ~4×
  too fast and derails the BIOS poll.
- **Cart + internal backup-RAM use odd-byte packing** (one packing for
  backup-manager compatibility); MAME packs the cart linearly.

Where ours is simply **more faithful** than MAME (no action needed, don't
"simplify" toward MAME): the SCSP envelope/FM/timer/monitor/wait-state model, the
VDP1 event-clamped draw-end, the SCU Timer1 down-counter, and the system-level
CDDA mix. The remaining *gaps* (where MAME does more) are tracked as roadmap
[M13 Tier G](roadmap.md#milestone-13--hardware-completeness--fidelity-backlog-).

#### C.2 Our instrumentation (all env-gated, off by default)

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

#### C.3 Harness constraints (so you don't fight them)

- This environment **kills any single command running longer than ~8 s**
  (signal 16 / exit 144). `run_for`-based tests (no rendering) fit ≈ 700 frames
  in the budget; `run_frame` (renders) and long Mednafen runs get killed.
- `SS_CDTRACE` survives the kill (it `fflush`es per command); `SS_PCTRACE`
  buffers, so a long Mednafen PC trace usually loses everything on the kill —
  prefer the command trace for the recognition window.
- Headless runs **must** build `-p jupiter --no-default-features` (passing
  `--no-default-features` as a *runtime* arg silently opens the SDL window).

---

### Part D — Bring-up gotchas distilled

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

### Status — M11 complete (tag `vf2-good-emulation`)

**VF2 and Doukyuusei ~if~ both boot to their own game code and are fully
playable.** Ours matches Mednafen's command stream through recognition
(GetHwInfo → … → Auth → GetDiscRegion → ChangeDir) and on into the boot —
**Play (IP.BIN) → ChangeDir (`00ffffff`) → GetFileScope → ReadFile** — streaming
the 1st-read program; the master reaches VF2's load address `0x06004000` and
executes there. **Virtua Fighter 2** runs at a steady 60 fps (looping CD-DA BGM,
balanced SFX, full 3D fights), and **Doukyuusei ~if~** runs at native 640×224
hi-res (GFX, SFX, and voices).

The original boot blocker was a CD-block one: the host interface re-raised
**`DCHG` (Disc Changed) on the first `Init` after recognition**, because the
internal `disk_changed` latch was cleared only inside the Init handler — so that
Init reported a fresh disc swap and the BIOS looped recognition forever instead
of booting. Fix: **clear `disk_changed` when the host write-1-to-clear-
acknowledges `DCHG`** (matching Mednafen, which clears `DCHG` once during
recognition and never re-raises it at Init). It was found by a command-level CD
trace-diff: the BIOS code is identical on both LLE sides, so the root had to be a
differing CD response — and the only divergence was ours' Init leaving `DCHG`
set (`0FC4 → 0FE5`) where Mednafen left it clear (`0F84`).

Post-boot, the run to "fully playable" closed several more fronts: the **BCR1
master/slave bit** (so an `SSHON`-released slave reads itself as the slave and
skips the WRAM init rather than re-initialising over the running game);
**`run_frame` running the whole frame in one `run_for`** (the split re-anchored
the batch grid and diverged the master); the **Mednafen-alignment scheduler work**
(master-leads-slave interleave + per-instruction SCU interrupt sampling); and the
**audio endgame** (SH-2→SCSP B-bus wait-states, CD-DA routed through the SCSP
EXTS inputs, and the KYONEX EG-phase key-on gate). See [§5](#5-video-and-audio-pipelines)
and the §7 boot summary for the resolved-state architecture, and
[`roadmap.md`](roadmap.md) M11 + the memory log for the trace-by-trace history.

**Boot fidelity:** with a disc inserted the BIOS plays its disc-present **boot
animation** — the recognition spin-up (the `Startup` drive phase,
[§B.3](#b3-the-recognition-command-sequence)) holds `STATUS_BUSY` for ~1 s so the
BIOS animates instead of jumping straight to the static logo. The animation's
**silence is resolved** (an `m68k` `ADDA`/`ADDX` decode bug, fix `32662f7`) — see
[§B.7](#b7-the-boot--cd-player-panel-bgm-resolved-2026-06-06).

---

## 10. Implementation status

Rough completeness of each component **toward full Saturn fidelity within the
project's scope** — a judgement call, not a measured metric (the
authoritative, factual per-task status is [`roadmap.md`](roadmap.md)). "Done"
here means accurate enough that no game/test has yet needed more; the listed
gaps are the known remaining work. The `Count` column gives the concrete
implemented-vs-total figures the percentage is based on, where countable.

**By the numbers:** SH-2 **143/143** instruction encodings · SCU-DSP **7**
instruction families + **13** ALU ops · MC68EC000 full 68000 set
(inline-decoded) · CD-block **33 of ~50** host commands · SMPC **16** commands
(digital pad + Shuttle Mouse) · VDP1 **9** command types (full set) · VDP2
**101** named registers, 6 layers · **1099** tests workspace-wide (sh2 210,
m68k 167, scu_dsp 66, saturn 597, frontend 40, plus doc-tests).

| Component | Code | Progress | Count | Done / remaining |
|---|---|---|---|---|
| SH-2 core (master + slave) | `crates/sh2/` | ~95% | 143/143 encodings; 5/8 on-chip live | M1: full ISA decoded + executed (exhaustive `match`), 5-stage pipeline interlocks, write-through cache, exceptions. On-chip INTC/DMAC/DIVU/FRT/WDT functional; **BSC/SCI/UBC are register stubs**. Deep cycle-timing edges refine as games surface them. (153 tests) |
| MC68EC000 (SCSP CPU) | `crates/m68k/` | ~85% | full 68000 set | M5: 68000 decoded inline (no flat table — addressing modes make one awkward); runs hosted SCSP sound code. Rare ops / edge-case flags may remain. (68 tests) |
| SCU-DSP | `crates/scu_dsp/` | ~90% | 7 families + 13 ALU ops | M3: standalone core, full ISA + DSP-DMA. (21 tests) |
| SCSP (FM/PCM engine + SCSP-DSP) | `crates/saturn/src/scsp/` | ~85% | 32 slots | M6/M11: 32-slot FM/PCM engine, SCSP-DSP, hosted 68k, **CD-DA via the EXTS digital inputs** (game-programmed mix, not an aggregate sum). Some envelope/effect corners refine as games surface them. |
| VDP1 (sprite/polygon) | `crates/saturn/src/vdp1/` | ~85% | 9/9 command types | M5: full command-list plotter — normal/scaled/distorted sprites, polygon, polyline, line, user-clip, system-clip, local-coordinate; gouraud + colour-calc. Some rare colour modes/edges. |
| VDP2 (background/compositor) | `crates/saturn/src/vdp2/` | ~85% | 6 layers; 101 registers | M5: NBG0–3 + RBG0/1, rotation, priority, colour calc, windows, per-line scroll/zoom. Mosaic / some special modes refine as needed. |
| SCU (DMA / INTC / timers) | `crates/saturn/src/scu.rs` | ~90% | 3 DMA channels | M3/M11/M12: 3 DMA channels, interrupt aggregation (incl. the CD external IRQ, vec `0x50`), timers. M12 added Mednafen-faithful DMA-bus arbitration (both-CPU halt for C-bus DMAs). Block transfer is still synchronous on its own timeline. |
| SMPC | `crates/saturn/src/smpc.rs` | ~85% | 16 commands | M3–M4/M13: register bank, `INTBACK` + digital pad **and Shuttle Mouse**, RTC, slave/sound on-off, region. Clock-change / `SYSRES` are no-ops. |
| Bus + scheduler + memory | `bus.rs` `scheduler.rs` `memory.rs` | ~95% | — | M2/M12: region dispatch, deterministic deadline scheduler, typed RAM/ROM/backup regions, and the per-access **BSC bus-timing model** (`BusTiming` — 16-bit CS0 / 32-bit CS3 SDRAM, write buffer, B-bus deferred writes, shared-timestamp CPU arbitration). |
| CD-block (HLE) | `crates/saturn/src/cd_block.rs` | ~85% | 33 of ~50 commands | M7+M10+M11: host interface, block/filter/partition engine, read pump, data transfer, ISO9660 FS, auth, CD-DA (→SCSP EXTS), the SCU external interrupt (vec `0x50`), and disc-recognition spin-up. **MPEG card + move/copy sector ops deferred.** |
| Live optical drive | `crates/physdisc/` | ~80% | 1 backend (libcdio) | M10: libcdio `SectorSource` (TOC + raw sectors + CD-DA), feature-gated. Linux-verified; other OSes untested. |
| Cartridge slot | `crates/saturn/src/cartridge.rs` | ~90% | 4 cart types | M7: Extension DRAM (1/4 MB), battery RAM, ROM cart, cart-ID byte. |
| Save states + backup RAM | `savestate.rs` `memory.rs` | ~90% | format v9 | M8: whole-machine bincode snapshot (media referenced), host-persisted battery. No cross-version migration. |
| Frontend (SDL2 + OSD) | `jupiter/` | ~80% | OSD complete | M9: window, audio, input (keyboard pad, gamepad, **Shuttle Mouse**), headless mode; a **render-pipeline worker** (compositing overlaps emulation on a 2nd core) with **audio as the frame pacer**; full OSD — save/load slots, reset, disc eject/insert + a **Load Disc… image browser** (navigate + pick + boot), and Settings (Graphics / Controller keyboard-rebind / Region / Cartridge / BIOS) with persisted config. Per-button gamepad rebind + analog devices deferred to M13 E2. (40 tests) |
| LLE BIOS boot / game boot | (uses real BIOS) | ~90% | splash + 2 playable games | M4: boots to the SEGA splash, pixel-matching the reference. **M11 complete (tag `vf2-good-emulation`):** Virtua Fighter 2 and Doukyuusei ~if~ both boot via the real BIOS loader (authenticate, region-check, read IP.BIN, load the 1st-read program) and are **fully playable** — VF2 at a steady 60 fps with looping CD-DA BGM and balanced SFX, Doukyuusei at native 640×224 hi-res with voices. The original boot blocker (the CD-block re-raising `DCHG` at `Init`, fixed by clearing `disk_changed` on the host's write-1-to-clear) and the audio/scheduling endgame (B-bus wait-states, CD-DA via EXTS, master-leads-slave interleave + per-instruction SCU sampling) are resolved; remaining work is per-game cycle-accuracy polish (M12/M13). |

---

## See also

- [`glossary.md`](glossary.md) — chip names, address ranges, register acronyms.
- [`roadmap.md`](roadmap.md) — milestone tracker and per-task status.
- [`adr/`](adr/) — the architectural decision records referenced above.
- `CLAUDE.md` (repo root) — the per-module contracts and gotchas, in depth.
