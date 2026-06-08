# MAME cross-reference divergence review

**Date:** 2026-06-08 · **Reference:** MAME (`mameref/src/mame/sega/` — `saturn.cpp`,
`saturn_m.cpp`, `saturn_scu.cpp`, `smpc.cpp`, `saturn_cd_hle.cpp`, `saturn_v.cpp`;
`mameref/src/devices/sound/scsp.cpp` + `scspdsp.cpp`; `mameref/src/devices/bus/saturn/`)

This is the MAME counterpart to [`mednafen-divergence-review.md`](mednafen-divergence-review.md):
a read-only, per-subsystem semantic comparison of the whole 5thPlanet system layer
against MAME's Saturn driver. **No MAME source is reproduced** — findings cite
behaviour + line numbers only (MAME is GPL; 5thPlanet is a clean-room
re-implementation, MIT). Method: one reviewer per subsystem read both sides and
reported divergences with severity (H/M/L) and a classification. The findings
below were then **cross-checked against the codebase** — a few reviewer
misreads are corrected inline and flagged `[corrected]`.

## Why a MAME review (and how it differs from the Mednafen one)

MAME plays a **different role** from Mednafen, so this review reads differently:

- **Mednafen is the accuracy oracle for *game* behaviour** (it boots the
  commercial library). A divergence from Mednafen is, by default, *our* bug.
- **MAME is the low-level / early-boot reference**, and its **CD-block is HLE
  like ours** — in fact ours was *modelled on* MAME's `saturn_cd_hle.cpp`
  (CLAUDE.md). So for most subsystems a MAME divergence is **not** a bug but one
  of three things: (a) a **deliberate** place where ours follows Mednafen / real
  hardware where MAME and Mednafen disagree (most of the M11 CD-block work is
  exactly this); (b) a spot where **ours is more faithful** than MAME (MAME's
  arcade-focused scope tolerates approximations we don't); or (c) a genuine
  **gap** where MAME models something we defer.

> **The MAME-vs-Mednafen tension** (already noted in the Mednafen review): the two
> disagree on CD-block conventions — power-on/reset HIRQ, DCHG stickiness, the
> HW-info MPEG byte, `is_cdrom` semantics, the recognition spin-up. The **splash**
> was matched to MAME; the **game boot** had to be matched to Mednafen. The
> classification tags below make each such choice explicit.

## Headline conclusion

**5thPlanet is broadly aligned with, or ahead of, MAME across every subsystem.**
The comparison surfaces almost no MAME-relative bugs; instead it cleanly separates
into:

1. **Deliberate Mednafen/hardware alignments** — the CD-block recognition path
   (reset HIRQ `0x0BE1`, sticky/W1C HIRQ, no-MPEG HW-info, `Startup` spin-up,
   one-shot `DCHG`, NODISC-not-PAUSE), the master-leads-slave interleave, the
   Mednafen INTBACK timing, the SSHON full-reset, the per-instruction SCU
   interrupt level. These are the right calls and are why the game boot works.
2. **Places ours is *more faithful* than MAME** — the SMPC INTBACK timing model,
   the SCSP envelope/FM/timer/monitor/wait-state model, the VDP1 event-clamped
   draw-end, the SCU Timer1 down-counter. MAME approximates; ours matches
   Mednafen/hardware.
3. **A short list of genuine gaps** where MAME does more — VDP1 framebuffer TVM
   modes (1024×256 / 512×512), several deferred VDP2 rotation/window features
   (M13 Tier C), VDP1 `CEF`-clear-on-swap, the SCSP per-sample (`0x400`)
   interrupt. None block the current targets.

## Deliberate divergences from MAME (the important list)

These are intentional — ours follows Mednafen / real hardware where MAME differs.
**Do not "fix" these toward MAME** without re-checking the Mednafen game-boot path.

| # | Area | MAME | Ours (deliberate) | Why |
|---|------|------|-------------------|-----|
| D1 | CD reset/Init HIRQ | `CMOK\|ESEL\|EHST` on Init | full `0x0BE1` (`+ECPY\|EFLS\|DCHG\|MPED`) on disc-present reset | MAME-shaped value desyncs the BIOS recognition state machine; Mednafen + our golden confirm `0x0BE1` ([`bootstrapping.md` §B.2](bootstrapping.md#b2-the-reset-hirq-the-load-bearing-detail)) |
| D2 | HIRQ read semantics | `hirq_r` **read-clears** `DCHG`/`CSCT`/`BFUL` (`saturn_cd_hle.cpp:~504`) | sticky **W1C** — reads never clear; host writes 0 to clear (`cd_block.rs:~780`) | read-clearing `DCHG` left the recognition shadow missing it; Mednafen keeps it |
| D3 | `DCHG` re-raise | implicit in tray state | host W1C also clears the internal `disk_changed` latch, so a later `Init` does **not** re-raise `DCHG` (`cd_block.rs:~846`) | the actual M11 boot root — MAME doesn't track the ack latch |
| D4 | Get HW Info (0x01) | `CR2=0x0201` (MPEG-present byte), `CR4=0x0400` | `CR2=0x0002` (no MPEG), `CR4=0x0600` (`cd_block.rs`) | MAME's MPEG byte triggers the BIOS MPEG-auth probe that loops recognition; ours matches Mednafen |
| D5 | Recognition spin-up | none — status flips immediately on insert | `DrivePhase::Startup` holds `STATUS_BUSY` ~1 s then settles PAUSE (`cd_block.rs:~255`) | the window the BIOS fills with its boot animation; Mednafen `DRIVEPHASE_STARTUP` |
| D6 | Empty drive | (tray-state) | reports `NODISC` (0x07), not PAUSE | matches Mednafen + hardware |
| D7 | Seek track-0 / stop | → PAUSE | → `STANDBY` with sentinel geometry (`cd_block.rs:~2257`) | Mednafen stop geometry; fixes a boot probe halt |
| D8 | Get-and-Delete (0x63) | frees blocks only on over-read | frees partition sectors on End-Data-Transfer (0x06) (`cd_block.rs:~2021`) | avoids stale IP.BIN sectors prepending the next read |
| D9 | Dual-SH-2 interleave | global `machine().scheduler()` + quantum boost on MINIT/SINIT (`saturn_m.cpp:~72`) | explicit **master-leads-slave** per-instruction (`step_cpus`, `system.rs:~847`) | Mednafen ordering preserves timing-sensitive inter-CPU WRAM handoffs |
| D10 | INTBACK timing | lump ~8 µs status / +700 µs periph (`smpc.cpp:~513`) | Mednafen 4 MHz model: ≈261 µs status-only via `intback_complete_at` (`system.rs:~31`) | MAME clears SF ~4× too fast and would derail the BIOS SF poll |
| D11 | INTBACK status SR | `0x40 \| (stage<<5)` | `(SR&~0xA0)\|0x0F\|NPE` (`system.rs:~1115`) | preserves the low-nibble status bits the BIOS reads for completion |
| D12 | SSHON/SSHOFF | `sshres(0/1)` line toggle | `release_slave` does a **full** SH-2 reset (VBR=0, re-fetch PC/SP) + BCR1 master/slave bit (`system.rs:~366`) | LLE slave must cold-boot at the reset vector, not resume stale state |
| D13 | SCU interrupt | delivered via `test_pending_irqs` scan | a **level sampled per master instruction** (`fresh_assertions` vs `ist`); CD source gated by `cd_prohibit`/AIACK (`scu.rs:~706`) | lands the IRQ at the exact instruction SR.imask drops; Mednafen-aligned |
| D14 | Cart backup-RAM packing | linear `(hi<<16)\|lo` per word (`bram.cpp:~104`) | odd-byte packing mirroring the *internal* backup RAM (`cartridge.rs:~189`) | one packing for backup-manager compatibility across internal + cart |

## Genuine gaps — where MAME does more than us

None block BIOS boot, VF2, or *Doukyuusei ~if~*; pull them in when a game needs them.

| # | Gap | Subsystem | Severity | Note |
|---|-----|-----------|----------|------|
| G1 | **VDP1 framebuffer TVM modes** — ours hard-codes 512×256; MAME supports 1024×256 (TVM=1) and 512×512 (TVM=3) (`saturn_v.cpp:~369`) | VDP1 | M | erase/draw bounds need TVM-awareness for those modes (unused by tested games) |
| G2 | **VDP1 `CEF` not cleared on buffer swap** — MAME clears `CEF` on every swap (`saturn_v.cpp:~364`); ours clears only at list-start (`vdp1/mod.rs`) | VDP1 | L | edge case: reading `CEF` between swap and next plot reports stale draw-end |
| G3 | **VDP1 double-interlace erase scaling** (FBCR DIE) + **`BEF`** status flag — MAME models; ours doesn't | VDP1 | L | interlace + status-only flag, rarely polled |
| G4 | **VDP2 rotation/window deferred set** — per-line coefficient table (KTCTL kx/ky), screen-over modes, line-zoom/reduction gating, window line-table, hi-res RBG plane scaling, mosaic-on-tiles, RBG1 RPMD enforcement | VDP2 | M | the known M13 Tier C deferrals (see [`emulation-capabilities-evaluation.md`](emulation-capabilities-evaluation.md)); MAME implements most |
| G5 | **SCSP per-sample interrupt** (`SCIPD`/`MCIPD` bit `0x400`) | SCSP | M | **both** MAME and ours skip it (drivers clocked off it get no tick); listed for completeness, not a MAME-relative gap |
| G6 | **CD move/copy sector ops (0x65/0x66) + MPEG card** | CD-block | L | deferred in both (MAME stubs them too); matches our CLAUDE.md "still deferred from M7" |

---

## Findings by subsystem

Tags: **bug** (ours wrong vs MAME) · **deliberate** (ours follows Mednafen/HW) ·
**MAME-approx** (ours more faithful) · **gap** (MAME more complete) · **consistent**.

### CD-block (`cd_block.rs`, `disc.rs`) vs `saturn_cd_hle.cpp`

This is the closest-coupled subsystem — ours was modelled on MAME's HLE — so most
divergences are the *deliberate* M11 re-alignments (D1–D8 above). Beyond those:

- **Command dispatch** (`cr_written == 0xF`, command byte `CR1>>8`), the
  status-code set, the 200-block pool + 24 filters/partitions (≤2-hop filter
  chain), the read pump, the End-Data-Transfer 24-bit word-count formula
  (`xfer>>17` / `xfer>>1` with `0xFF`/`0xFFFF` zero sentinel), and the
  sector-length codes (0→2048/1→2336/2→2340/3→2352) all **match** MAME.
- **Periodic report cadence** — MAME drives it off a fixed 75/150 Hz sector timer;
  ours uses idle vs active intervals (`PERIODIC_IDLE_CYC` / `PERIODIC_ACTIVE_CYC`).
  **deliberate** (Mednafen cadence) and golden-safe (periodic gated until the
  first host command so the power-on `"CDBLOCK"` CR signature survives).
- **`[corrected]` Authentication (0xE0/0xE1) and the ISO9660 file commands are
  implemented**, not stubs — the reviewer misread M7-phase comments. `0xE0`/`0xE1`
  live at `cd_block.rs:~2460/2478`; `read_new_dir`/`make_dir_current` at
  `~1636/1675`. M7 is complete (all five phases). **consistent** with MAME's logic.
- **`[corrected]` CD-DA audio playback works** (M10): the read pump decodes audio
  sectors and `Saturn::take_audio` sums them into the SCSP — the reviewer checked
  only the device and missed the system-level mix. MAME's `cd_playdata` audio
  start is commented out, so here **ours is more complete**.
- **gap (G6):** move/copy sector ops (0x65/0x66) and the MPEG card are stubs in
  both. **consistent** (both deferred).

### SCU (`scu.rs`, `crates/scu_dsp/`) vs `saturn_scu.cpp`

- **DMA model** — MAME runs DMA blocking inside `handle_dma_direct/indirect` and
  fires the end-IRQ off a `dma_tick` timer; ours queues (`take_pending_dma`) and
  the aggregate drains synchronously, charging per-access bus wait-states and
  stalling the requesting CPU (cycle-steal). **deliberate** (M13 Tier A A3); the
  end-IRQ timing differs but both complete before the next instruction observes it.
- **`[corrected]` DMA-illegal IS implemented.** The reviewer flagged "no
  BIOS-region check → bug" by reading only `scu.rs`; the check lives in
  `system.rs:155` (`scu_dma_illegal(src,dst)` raising `Source::DmaIllegal`, M13
  D5). **Nuance worth a note:** ours' illegal predicate is *same-bus / unmapped*
  (Mednafen `StartDMATransfer`), while MAME keys specifically on a **BIOS-region
  source** (`src & 0x07f00000 == 0`, `saturn_scu.cpp:~389`). The two predicates
  overlap but aren't identical — verify against a game that DMAs from BIOS if one
  surfaces. **deliberate / partial.**
- **Interrupt model** — IST/IMS W1C, vector `0x40+`internal / `0x50+`external,
  IMASK reset `0xBFFF`, per-instruction level sampling, `cd_prohibit`/AIACK gating:
  ours is **more explicit** than MAME's `irq_level[]` scan but semantically
  aligned. **consistent** (D13 is the deliberate part).
- **Timer0** line-compare — **consistent** (both simplified, no free-running
  counter). **Timer1** — ours is a per-line down-counter reloaded at HBLANK
  (`scu.rs:~627`); MAME uses a scanline-condition trigger. **MAME-approx** (ours
  finer, Mednafen-aligned).
- **SCU-DSP** — both embed it; ours' DSP-initiated DMA is queued
  (`Dsp::take_dma`) but driven by the host, MAME wires its DSP DMA callbacks to
  the A/B-bus directly. **consistent** logic; ours' DSP-DMA bus drive is a later
  item (no current game needs it).
- *Consistent:* register map, DGO-bit/byte-don't-trigger, count-0→max promotion,
  channel widths (ch0 20-bit / ch1-2 12-bit), DSP port regs (PPAF/PPD/PDA/PDD),
  indirect-table structure (end-flag = bit 31 of the src word — verify ours honours
  it in the drain).

### SMPC (`smpc.rs`, `system.rs`) vs `smpc.cpp`

Here **ours is consistently more faithful than MAME** — the INTBACK timing/SR
model (D10/D11), the `intback_complete_at` SF phasing, CKCHG halt+NMI (D-style),
and SSHON full-reset (D12) all beat MAME's arcade-scope approximations.

- **CKCHG (0x0E/0x0F)** — MAME halts the system until the VBLANK clock boundary,
  switches the dot-clock, halts the slave, then NMIs the master; ours halts the
  slave and NMIs the master immediately (`system.rs:~1145`). **deliberate
  simplification** — captures the BIOS-observable effect (halt + NMI) without the
  PLL-synchronous choreography; the dot-clock relay is a display detail.
- **SETTIME (0x16) / SETSMEM (0x17) / RTC** — both decode BCD into the RTC and
  echo SMEM in INTBACK OREG12–15. **consistent** (different storage models).
- **Open/minor (matches the Mednafen review):** OREG0 STE/RESD hardcoded `0x80`
  (no `ResetNMIEnable` model), OREG10 dot-select bit hardcoded, peripheral-only
  INTBACK (IREG0=0) unhandled, non-INTBACK command SF-delays not modelled
  (BIOS-invisible). **MAME-approx** on each — MAME models a bit more here, but the
  BIOS never observes the difference.
- *Consistent:* all command codes, the INTBACK CONTINUE/BREAK staging, NMIREQ
  delivery, odd-byte register addressing, PDR DDR masking, region default `0x04`.

### System glue (`bus.rs`, `system.rs`, `memory.rs`, `scheduler.rs`, `cartridge.rs`) vs `saturn.cpp`, `saturn_m.cpp`, `bus/saturn/*`

- **Scheduling (D9)** — master-leads-slave per-instruction vs MAME's global
  scheduler + MINIT/SINIT quantum boost. **deliberate.**
- **FTI inter-CPU wake** — same regions (`0x0100_0000` slave-FTI / `0x0180_0000`
  master-FTI; MAME's `minit_w`/`sinit_w`), same semantics; ours applies the pulse
  per-instruction (`apply_fti!`). **consistent.**
- **Memory map** — low WRAM 1 MiB @ `0x0020_0000`, high WRAM 1 MiB @ `0x0600_0000`,
  BIOS mirror, open-bus (read 0 / drop write): all **consistent** with MAME (MAME
  expresses the high-WRAM fold as a `.mirror(0x21f00000)`; ours folds in the
  CPU/bus — same effect). Ours follows Mednafen on the 16 MiB high-WRAM window
  with 1 MiB mirrored.
- **Cartridge** — internal + cart backup-RAM odd-byte packing **consistent** (the
  cart linear-vs-odd-byte packing is the **deliberate** D14). DRAM mirroring:
  MAME truncates each bank at 512 KiB/chip (`dram.cpp:~60`); ours mirrors the full
  2 MiB window. **MAME-approx** (ours more permissive; no game probes beyond its
  cart size). Cart-ID byte at exact `0x04FF_FFFF`, families (DRAM 0x5A/0x5C, BRAM
  0x21–0x24, ROM 0xFF), "BackUpRam Format" pre-format: **consistent.**
- **Backup-RAM high byte** reads `0x00` (even lanes) in both; the `DB|0xFF00`
  open-bus nuance is unmodelled in both. **consistent.**

### VDP2 (`vdp2/*.rs`, `system.rs::update_video_timing`) vs `saturn_v.cpp` (VDP2 part)

- **Complete + matching:** NBG tile/bitmap addressing, CRAM banking (CRAOFA/B),
  transparent-pen-as-solid (TPON), special priority / special colour-calc
  (SFPRMD/SFCCMD modes 1–3, per-dot), RGB888 CRAM/colour-calc MSB handling, the
  live raster registers (VCNT/HCNT/TVSTAT VBLANK/HBLANK/ODD). **consistent.**
- **gap (G4):** the deferred rotation/window/zoom set — per-line coefficient
  table, screen-over modes, reduction-enable + per-line zoom gating, window
  line-table, hi-res RBG plane scaling, mosaic-on-tiles, RBG1 RPMD enforcement.
  MAME implements most; ours defers them (M13 Tier C). **None block the current
  targets** (mid-game effects). Cross-ref
  [`emulation-capabilities-evaluation.md`](emulation-capabilities-evaluation.md).
- *Consistent:* layer set, char/bitmap basics, CRAM banking, colour offset
  (CLOFEN/CLOFSL), the standard priority resolution.

### VDP1 (`vdp1/*.rs`) vs `saturn_v.cpp` (VDP1 part)

**Highly aligned** — the command-list walker (END/NEXT/JUMP/CALL/RETURN, 1-level
nest), all primitives, the textured-quad rasteriser (16.16 forward-difference),
SPD/end-code/mesh/MSBON, the colour-calc modes (shadow / half-luminance /
half-transparent, all with the dest-MSB gate), PTMR/FBCR swap logic, and the six
CMDPMOD colour modes are **byte-for-byte equivalent** in behaviour.

- **Draw-end interrupt** — MAME schedules a timer `spritecount*16` cycles out;
  ours computes `commands*16 + pixels*1` and feeds it into the event-clamped
  scheduler (`vdp1/mod.rs`). **MAME-approx** (ours more precise + drives the
  edge-clamp).
- **Erase** — MAME erases at the frame-change (displayed-or-draw buffer per mode);
  ours erases the **draw** buffer pre-plot. Both valid; ours avoids the
  erase-on-displayed complexity the Mednafen review flagged as open. **deliberate.**
- **Gouraud** — MAME accumulates per-scanline deltas; ours interpolates
  per-pixel/bilinear. **MAME-approx** (different method, same result).
- **gaps:** G1 (TVM framebuffer modes), G2 (`CEF`-clear-on-swap), G3
  (double-interlace erase + `BEF`).

### SCSP / MC68EC000 (`scsp/*.rs`, `crates/m68k/`) vs `scsp.cpp`, `scspdsp.cpp`

**Ours is materially more faithful than MAME here**, because MAME's SCSP is a
mature-but-pragmatic device model while ours was tuned to Mednafen during the
BIOS-BGM work (see [`bootstrapping.md` §B.7](bootstrapping.md#b7-the-boot--cd-player-panel-bgm-resolved-2026-06-06)):

- **EG** exponential attack curve (ours/Mednafen) vs MAME's linear step;
  **FM SoundStack** 4-slot read-delay (ours models the inter-slot pipeline; MAME
  doesn't); **MDL ≤ 4 = no modulation** threshold (ours/hardware; MAME applies a
  small shift); **EG×TL** log-curve applied in all phases (ours) vs MAME's
  conditional; **Timer** 8-bit free-running, phase-locked to the sample clock
  (ours) vs MAME's `emu_timer`; **MVOL** logarithmic (ours/spec) vs MAME linear;
  **slot CA/EG monitor (0x408)** computed live (ours — the boot-jingle BGM-loop
  root) vs static. All **MAME-approx** (ours more complete, Mednafen-aligned).
- **gap (G5):** the per-sample (`0x400`) SCIPD/MCIPD interrupt — **both** skip it.
- **`[corrected]` CDDA mix is implemented** (M10, `system.rs:~1493`) — the
  reviewer's "neither mixes CDDA" is wrong for us (the mix is at the system level,
  not the SCSP device). **ours more complete.** **MIDI (0x404)** reports
  empty-flags + discards MOBUF (M13 B3); MAME stubs it. **ours more complete.**
- **SNDON** — ours does a full 68k `reset` each time rather than an un-halt; MAME
  doesn't model SNDON at all (the SCSP is a device, the host would gate it). This
  is the same **open item** the Mednafen review lists — a SNDON-after-running
  re-resets the driver. **open** (no MAME baseline to match; fix toward an
  un-halt/`SetExtHalted` model).
- **Low-confidence / to verify:** a reviewer flagged `LPSLNK` (loop-start envelope
  link) as not gating the attack→decay transition. Unverified here and the BGM is
  user-confirmed working, so it is recorded as a **check item**, not a confirmed
  bug.
- *Consistent:* slot register layout (32 × 0x20), AR/DR envelope tables, LFO
  waveforms, 12-bit interpolation, loop modes, DISDL/DIPAN panning, DSP
  PACK/UNPACK + per-sample `Step`, 68k interrupt delivery, 512 KiB sound RAM map.

---

## Net assessment

The MAME comparison **corroborates** the post-consolidation state the Mednafen
review reached: the system layer is coherent and mostly ahead of MAME. The
actionable output is small and already tracked:

1. **VDP1 TVM framebuffer modes (G1)** — the one concrete rendering gap with a
   clear MAME reference; do it when a 1024×256 / 512×512 game surfaces.
2. **VDP2 Tier C deferrals (G4)** — already in the roadmap; MAME is a usable
   reference for the per-line coefficient table and screen-over modes.
3. **SCSP SNDON un-halt** and the **per-sample `0x400` interrupt (G5)** — the
   Mednafen review's open SCSP items; MAME doesn't help on either (it skips both).
4. **SCU DMA-illegal predicate** — confirm ours' same-bus/unmapped condition
   covers MAME's BIOS-source case if a game DMAs from BIOS.

Everything else is either **deliberate** (don't regress it toward MAME) or
**ours-more-faithful**. The deliberate-divergence table above is the load-bearing
artifact: it records *why* ours leaves MAME, so a future "align to MAME" impulse
doesn't silently break the Mednafen game-boot path.
