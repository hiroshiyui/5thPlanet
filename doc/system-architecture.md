# System Architecture

How the SEGA Saturn hardware maps onto this project's code. This is the
orientation map: for each real chip, bus, and memory region it points at the
crate/module/file that models it. For Saturn-specific vocabulary see
[`glossary.md`](glossary.md); for task-by-task status see
[`roadmap.md`](roadmap.md); for *why* a structural choice was made see the ADRs
in [`adr/`](adr/).

The guiding axis is **accuracy over performance** (ADR-0002): real chips are
low-level-emulated (LLE) cycle-by-cycle; no JIT, dynarec, or "approximate
cycle". The one deliberate exception is the **CD-block, which is HLE** — its
SH-1 firmware is undumped and half its job is an analog servo with no digital
ground truth, so there is nothing to be cycle-accurate against (ADR-0010 also
covers an *opt-in* HLE direct game boot; the LLE BIOS path stays the default).

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
mirrors transparently). Wait states are SH7604 BSC defaults (`waits_for`).

---

## 4. The system glue (`crates/saturn/src/system.rs`)

`Saturn` owns the bus + the scheduler + the master/slave/CD entity IDs. It is
the surface the frontend holds.

- **`scheduler::Scheduler<E>`** (`scheduler.rs`) is an event-driven, deterministic
  scheduler (ADR-0004): each `SchedEntity` reports `next_deadline()` (its next
  global cycle) and the most-behind entity steps; ties break by insertion order
  (master, slave, CD-block). A halted entity reports `u64::MAX` so it is skipped
  — and un-halting **resyncs its cycle** to `now()` so it can't "time-travel".
- **`run_for(cycles)`** is the headless loop: it runs the scheduler in
  `SMPC_POLL_QUANTUM = 256`-cycle batches (clamped to the next VBlank-IN edge),
  then between batches drains the queued side effects. **`run_frame(out)`** runs
  one NTSC frame (`CYCLES_PER_FRAME = 479_151`, `LINES_PER_FRAME = 263`,
  `ACTIVE_LINES = 224`) and snapshots the framebuffer at the active→VBLANK edge.
- **Queue-and-drain** (ADR-0005): peripherals can't reach the CPUs across the
  bus borrow, so they flag a side effect and `Saturn` drains it at the batch
  boundary: `update_video_timing` (raster regs + VBlank-IN), `drain_smpc`,
  `drain_scu_dma`, `drain_scu_dsp`, `drain_vdp1`, `drain_scsp`, `drain_scu_intc`
  (forwards the top unmasked SCU source to the master INTC), and
  `drain_input_capture` (the inter-CPU FRT pulse, §6).

The **SMPC** (`smpc.rs`) is the low-speed controller: reset/clock, the RTC, pad
input via `INTBACK`, and slave/sound on-off (`SSHON`/`SSHOFF`, `SNDON`). The
**SCU** (`scu.rs`) holds the 3 DMA channels, the interrupt mask/status (IMS/IST),
and the timers, and aggregates interrupts toward the master.

---

## 5. Video and audio pipelines

**Video:** VDP1 (`vdp1/plotter.rs`) rasterises its command list into a
double-buffered framebuffer; VDP2 (`vdp2/renderer.rs`) composites NBG0–3 and
RBG0/1 (rotation in `vdp2/rotation.rs`) by priority, plus the VDP1 sprite layer,
with colour calculation, windows, and per-line scroll/zoom. `Saturn::run_frame`
calls `vdp2::render_frame(&vdp2, Some(vdp1.display_fb()), out)` to produce the
final RGBA8888 320×224 frame.

**Audio:** the SCSP (`scsp/mod.rs`) is a 32-slot FM/PCM engine + the SCSP-DSP
(`scsp/dsp.rs`) + the hosted MC68EC000 in sound RAM. `Saturn::take_audio` drains
the mixed 44.1 kHz stereo each frame and **sums in CD-DA** decoded by the
CD-block (CDDA→SCSP, M10) at the aggregate.

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
the project's only `unsafe`).

**Boot — two paths:**
- **LLE (default, reference):** run the real BIOS ROM; it initialises the
  hardware and either boots a disc or drops to its CD player. This is the path
  the `bios_boot` golden test pins.
- **HLE direct boot (opt-in, `--hle-boot`, ADR-0010/0011):**
  `Saturn::cold_hle_boot` loads the disc's 1st-read program, installs an HLE BIOS
  **SYS-call library** (`bios_hle.rs`) over the work-RAM call table
  (`0x0600_0200..0x0600_0360`), enables a per-core dispatch hook, and hands off
  both SH-2s (the slave is started on `SSHON` at the game-written entry
  `[0x06000250]`, `VBR = 0x06000400`). This is the active M11/M12 work.

**Save states** (`savestate.rs`, M8): a `bincode` snapshot of the whole machine;
external media (BIOS/disc/ROM-cart bytes) is referenced not embedded, re-grafted
on load and validated by an FNV-1a fingerprint. **Backup RAM** persists to a
host file.

---

## 8. Workspace layout

| Crate / dir | Contents |
|---|---|
| `crates/sh2/` | SH7604 core (`no_std + alloc`), on-chip peripherals |
| `crates/m68k/` | MC68EC000 core (SCSP sound CPU) |
| `crates/scu_dsp/` | SCU's embedded 32-bit DSP (ADR-0006) |
| `crates/saturn/` | System glue: bus, scheduler, SMPC, SCU, VDP1, VDP2, SCSP, CD-block, cartridge, save states, `bios_hle` |
| `crates/physdisc/` | Live optical-drive `SectorSource` via libcdio; the sole FFI/`unsafe` crate (ADR-0009) |
| `fifth_planet/` | SDL2 frontend (window + audio) or headless; the `osd/` in-window menu (ADR-0008) |
| `doc/` | This file, [`roadmap.md`](roadmap.md), [`glossary.md`](glossary.md), [`adr/`](adr/) |

The root `Cargo.toml` is a `[workspace]` (resolver 3, edition 2024) with
`unsafe_code = "forbid"` set workspace-wide (ADR-0007); `physdisc` is the only
crate that opts out, with justification.

---

## 9. Implementation status

Rough completeness of each component **toward full Saturn fidelity within the
project's scope** — a judgement call, not a measured metric (the
authoritative, factual per-task status is [`roadmap.md`](roadmap.md)). "Done"
here means accurate enough that no game/test has yet needed more; the listed
gaps are the known remaining work. The `Count` column gives the concrete
implemented-vs-total figures the percentage is based on, where countable.

**By the numbers:** SH-2 **143/143** instruction encodings · SCU-DSP **7**
instruction families + **13** ALU ops · MC68EC000 full 68000 set
(inline-decoded) · CD-block **33 of ~50** host commands · SMPC **16** commands ·
VDP1 **9** command types (full set) · VDP2 **101** named registers, 6 layers ·
HLE BIOS **9 of 26** SYS slots · **533** tests workspace-wide (sh2 153, m68k 68,
scu_dsp 21, saturn 279, frontend 12).

| Component | Code | Progress | Count | Done / remaining |
|---|---|---|---|---|
| SH-2 core (master + slave) | `crates/sh2/` | ~95% | 143/143 encodings; 5/8 on-chip live | M1: full ISA decoded + executed (exhaustive `match`), 5-stage pipeline interlocks, write-through cache, exceptions. On-chip INTC/DMAC/DIVU/FRT/WDT functional; **BSC/SCI/UBC are register stubs**. Deep cycle-timing edges refine as games surface them. (153 tests) |
| MC68EC000 (SCSP CPU) | `crates/m68k/` | ~85% | full 68000 set | M5: 68000 decoded inline (no flat table — addressing modes make one awkward); runs hosted SCSP sound code. Rare ops / edge-case flags may remain. (68 tests) |
| SCU-DSP | `crates/scu_dsp/` | ~90% | 7 families + 13 ALU ops | M3: standalone core, full ISA + DSP-DMA. (21 tests) |
| SCSP (FM/PCM engine + SCSP-DSP) | `crates/saturn/src/scsp/` | ~80% | 32 slots | M6: 32-slot FM/PCM engine, SCSP-DSP, hosted 68k, CDDA mix-in. SCSP CD-input level/pan fidelity and some envelope/effect corners are refinements. |
| VDP1 (sprite/polygon) | `crates/saturn/src/vdp1/` | ~85% | 9/9 command types | M5: full command-list plotter — normal/scaled/distorted sprites, polygon, polyline, line, user-clip, system-clip, local-coordinate; gouraud + colour-calc. Some rare colour modes/edges. |
| VDP2 (background/compositor) | `crates/saturn/src/vdp2/` | ~85% | 6 layers; 101 registers | M5: NBG0–3 + RBG0/1, rotation, priority, colour calc, windows, per-line scroll/zoom. Mosaic / some special modes refine as needed. |
| SCU (DMA / INTC / timers) | `crates/saturn/src/scu.rs` | ~85% | 3 DMA channels | M3: 3 DMA channels, interrupt aggregation, timers. Synchronous block DMA — cycle-stealing timing is a later refinement. |
| SMPC | `crates/saturn/src/smpc.rs` | ~85% | 16 commands | M3–M4: register bank, `INTBACK` + digital pad, RTC, slave/sound on-off, region. Clock-change / `SYSRES` are no-ops. |
| Bus + scheduler + memory | `bus.rs` `scheduler.rs` `memory.rs` | ~95% | — | M2: region dispatch, deterministic deadline scheduler, typed RAM/ROM/backup regions. |
| CD-block (HLE) | `crates/saturn/src/cd_block.rs` | ~85% | 33 of ~50 commands | M7+M10: host interface, block/filter/partition engine, read pump, data transfer, ISO9660 FS, auth, CDDA. **MPEG card + move/copy sector ops deferred.** |
| Live optical drive | `crates/physdisc/` | ~80% | 1 backend (libcdio) | M10: libcdio `SectorSource` (TOC + raw sectors + CD-DA), feature-gated. Linux-verified; other OSes untested. |
| Cartridge slot | `crates/saturn/src/cartridge.rs` | ~90% | 4 cart types | M7: Extension DRAM (1/4 MB), battery RAM, ROM cart, cart-ID byte. |
| Save states + backup RAM | `savestate.rs` `memory.rs` | ~90% | — | M8: whole-machine bincode snapshot (media referenced), host-persisted battery. No cross-version migration. |
| Frontend (SDL2 + OSD) | `fifth_planet/` | ~55% | OSD phase 1/4+ | M9: window, audio, input, headless mode; OSD phase 1 (save/load, reset, eject, quit). Graphics / controller-rebind / region-BIOS / cartridge submenus + config file remain. (12 tests) |
| LLE BIOS boot | (uses real BIOS) | ~70% | splash | M4: boots to the SEGA splash, pixel-matching the reference. Booting a *game* via the real BIOS loader is not reached (the reason for the HLE path). |
| HLE direct boot + SYS library | `bios_hle.rs` + `cold_hle_boot` | ~40% | 9 of 26 SYS slots | M11/M12 active, opt-in (`--hle-boot`): VF2 loads + runs its own code on both SH-2s (slave dispatch, interrupts, work queue) but does **not yet render** — a frame-sync handshake remains. SYS coverage is per-game/iterative. |

---

## See also

- [`glossary.md`](glossary.md) — chip names, address ranges, register acronyms.
- [`roadmap.md`](roadmap.md) — milestone tracker and per-task status.
- [`adr/`](adr/) — the architectural decision records referenced above.
- `CLAUDE.md` (repo root) — the per-module contracts and gotchas, in depth.
