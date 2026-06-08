# Mednafen cross-reference divergence review

**Date:** 2026-05-30 · **Revised:** 2026-06-08 · **Reference:** Mednafen / Beetle
Saturn (`mednaref/src/ss/`)

> **Update (2026-06-08) — this consolidation pass is essentially complete; the
> findings below are now a resolution record.** Almost every boot-critical and
> medium item in this review has since landed (Mednafen-alignment Phases 1–2,
> M11 game-boot, M13 Tier A/C/D). The per-subsystem sections below have been
> annotated in place with their current status — **✅ resolved**, **◑ partial /
> by-design**, **○ open** — and a `file:line` or commit pointer. The short
> version:
>
> - **CD-block** — the actual M11 boot root (the `DCHG`-at-`Init` latch, see the
>   2026-06-01 note) is fixed, plus `DrivePhase::Startup` recognition spin-up and
>   the CD→SCU external interrupt (vector `0x50`, level 7). All ✅.
> - **SCU** — the interrupt model was reworked exactly as #5 proposed: a level
>   sampled per master instruction (`fresh_assertions` vs `ist`), internal/external
>   vector split, IMASK reset `0xBFFF` + bit-15 sign-extend, AIACK/`cd_prohibit`.
>   All ✅ except Timer0 (line-compare only) and a couple of L items.
> - **System** — slave full-reset + BCR1 master/slave bit on `SSHON`; the coarse
>   256-cycle batch replaced by an **event-clamped, master-leads-slave**
>   interleave with per-instruction SCU sampling; `run_frame` is a single
>   `run_for`. ✅ The full per-access SH-2 bus-timing model (#5/system-#3 residue)
>   is the one large item still open (M13).
> - **SMPC** — `CKCHG` now NMIs the master; INTBACK SF held via
>   `intback_complete_at`; status-SR corrected. ✅
> - **VDP2** — TVSTAT display-off, HBlank-IN, Timer1, CRAOFB + colour-offset,
>   RBG1/RPMD, special priority/CC, hi-res, NBG reduction+fractional scroll all
>   ✅. The VBlank-OUT 1-line phase (H2) and progressive-ODD (M5) remain ○ minor.
> - **VDP1** — the full command-list plotter shipped (RGB transparency,
>   end-codes, SPD, gouraud, half-transparency, MSBON, scaled sprites, draw-end).
>   Erase-on-displayed-buffer is the notable ○.
> - **SCSP** — the BIOS-BGM saga is fully resolved (the `m68k` `ADDA`/`ADDX`
>   decode bug `32662f7`, slot-monitor `0x408`, access wait-states; see
>   [`bootstrapping.md` §B.7](bootstrapping.md#b7-the-boot--cd-player-panel-bgm-resolved-2026-06-06)).
>   SNDON-full-reset and the per-sample `0x400` interrupt are the open ◑/○ items.
>
> **Boot status:** VF2 (JP) and Doukyuusei ~if~ both boot **into their own game
> code** on the LLE path — the "VF2 LLE boot fails after IP.BIN" headline below is
> resolved. Doukyuusei reaches its title screen; VF2 stalls later, in its intro
> demo-script loop on a *polled CD-state* divergence (a different, post-boot
> issue). See [`bootstrapping.md`](bootstrapping.md) §B and the
> `m11-game-boot-progress` memory.

> **Update (2026-06-01) — VF2 now boots; the root was in the CD-block after
> all.** This review's headline ("eliminated the CD-block as the cause… the
> likely culprits cluster in the interrupt/raster timing model") was premature.
> The actual M11 boot blocker was a **CD-block** bug: the host interface
> re-raised `DCHG` (Disc Changed) on the first `Init` after recognition (the
> internal `disk_changed` latch was cleared only inside the Init handler), so
> the BIOS perceived a fresh disc swap and looped recognition. Clearing
> `disk_changed` on the host's `DCHG` write-1-to-clear (matching Mednafen) fixed
> it — VF2 and Doukyuusei ~if~ now load their 1st-read and reach game code. The
> "changing CD outputs doesn't change the loader's decision" finding missed it
> because the divergence was a *HIRQ acknowledgment latch*, surfaced only by a
> command-level CR/HIRQ trace-diff (not the response-value diffs tried here).
> The general fidelity observations below (SCU/timing model, etc.) still stand
> as accuracy notes, but were not the boot decider.

## Why

5thPlanet's Saturn layer was built up one chip at a time, walking several
references in sequence — Yabause → MAME → Yabause → Mednafen. Each models the
same hardware with different conventions (status-bit meanings, HIRQ/IST
stickiness, command side-effects, timing), so stitching them produced logic
that is *locally* defensible but *globally* incoherent. This review consolidates
the whole system layer against a **single high-fidelity reference** — Mednafen,
the only open-source emulator that boots the commercial library — to find the
VF2-boot blocker and the latent inconsistencies it papers over.

Method: one reviewer per subsystem compared our module to its Mednafen
counterpart (read-only), reporting semantic divergences with severity and
boot/game impact. The CD-block section is from this session's direct work.

## Headline conclusion (2026-05-30, superseded)

> *Superseded — kept as the record of what this review originally concluded. The
> actual boot root was the CD-block `DCHG`-at-`Init` latch (2026-06-01 note); the
> interrupt/raster consolidation it called for landed anyway and was the right
> hygiene. VF2 now boots into game code (2026-06-08).*

The VF2 LLE boot fails *after* the BIOS reads a valid IP.BIN: the boot loader
rejects the disc and re-recognizes instead of loading the 1st-read. This
session **eliminated the CD-block as the cause** (IP.BIN content, FAD report,
Play status, HIRQ bits all verified/ruled out — changing CD outputs does not
change the loader's decision). The review shows why: the **interrupt and raster
timing model is the most divergent area**, and the BIOS boot loader is
interrupt-driven and timing-sensitive. The likely culprits cluster in:

- **SCU interrupt model** — IST pending-vs-acknowledged semantics, edge-only
  (non-level) presentation, wrong IMASK reset, no auto-mask-on-vector-fetch.
- **VDP2 raster timing** — TVSTAT.VBLANK ignores display-off (the BIOS waits
  for VBLANK *before* enabling display), VBlank-OUT edge a line late, HBlank-IN
  and SCU timers never raised.
- **System** — slave SH-2 not reset on `SSHON` (LLE), coarse 256-cycle batch
  scheduling vs event-exact interleave.
- **SMPC** — `CKCHG` is a no-op (misses the master NMI the BIOS waits on),
  INTBACK status-phase SR value wrong.

## Boot-critical fix queue (all landed)

Every row below has since been implemented; the queue is kept to show the path
taken. Status as of 2026-06-08:

| # | Fix | Subsystem | Status |
|---|-----|-----------|--------|
| 1 | TVSTAT.VBLANK reflects display-off (=1 when display off / power-on) | VDP2 | ✅ `3e43928` (`vdp2/mod.rs`) |
| 2 | IMASK reset = `0xBFFF`; IMS writes masked to `0xBFFF` | SCU | ✅ `5ce37d4` (`scu.rs:279,319`) |
| 3 | VBlank-OUT/VBLANK-clear edge at last line (262), not line 0 | VDP2 | ○ deferred (1-line phase, marginal; `system.rs:718`) |
| 4 | Raise HBlank-IN per line; implement SCU Timer0/Timer1 | VDP2/SCU | ✅ HBlank-IN + Timer1; Timer0 line-compare (`scu.rs:627`) |
| 5 | IST = live *pending* set (asserted vs pending; level re-assert; clear only on W1C / vector-fetch) | SCU | ✅ `fresh_assertions` vs `ist`, per-instruction level (`scu.rs:706`) |
| 6 | Slave SH-2 full-reset on `SSHON`/`SSHOFF` (LLE), VBR=0, reset vector | system | ✅ `879bba7` + BCR1 bit (`system.rs:366`) |
| 7 | `CKCHG` performs subsystem reset + master NMI | SMPC | ✅ halt-slave + master NMI (`system.rs:1145`) |
| 8 | INTBACK status SR = `(SR&~0x80)|0x0F`; OREG0/10/11 from live state | SMPC | ✅-SR `534a7ba` (`(SR&~0xA0)|0x0F|npe`, `system.rs:1115`); OREG live-state ◑ |

#5 (the SCU interrupt rework) was the deepest and riskiest; it was done on its
own with the `bios_boot` golden as guard and is now the per-instruction
level-sampled model described in [`../CLAUDE.md`](../CLAUDE.md) and `scu.rs`.

**Historical status (2026-05-30):** four boot-critical consolidation fixes
landed first — #1 `3e43928`, #2 `5ce37d4`, #6 `879bba7` (slave power-on-reset on
SSHON), #8-SR `534a7ba`. All golden-safe, suite + clippy green. **VF2 still gave
up** at that point — expected: the post-IP.BIN rejection was a *convergence* and
none of these was the single unblock. The actual unblock came later: the
CD-block `DCHG`-at-`Init` latch (2026-06-01 note).

**Reassessment of the queue at the time (now historical):**
- **#5 (SCU IST live-pending / level re-assert)** — judged *unlikely* the VF2
  unblock (correct — it wasn't), but a real Mednafen-consolidation fix. Landed
  anyway; the per-instruction level model is now load-bearing for the M11 CD
  external interrupt (vector `0x50`).
- **#3 (VBlank-OUT edge)** — a 1-line phase shift; deferred then, still ○ open
  (marginal, golden risk).
- **#7 (CKCHG subsystem-reset + master NMI)** — landed: it halts the slave and
  NMIs the master, which the BIOS `ChangeSystemClock` SYS call waits on.
- **#8-OREG / #4 timers** — minor / BIOS-ignored fidelity; Timer1 since
  implemented, OREG live-state still partial.

---

## Findings by subsystem

Each finding is tagged with its 2026-06-08 status: **✅ resolved**, **◑ partial /
by-design**, **○ open**.

### SCU (`scu.rs`, `crates/scu_dsp/`) vs `scu.inc`, `scu_dsp_*.cpp`

- **H1 — IST exposes ack-cleared state, not pending.** *(orig: `take_pending_interrupt`
  cleared `ist` on vector-take; a handler reading IST saw the bit gone.)*
  **✅ resolved** — reworked to Mednafen's split: `fresh_assertions` (edge, drives
  re-pend) vs `ist` (software-visible status), and the SCU IRL is now a **level
  sampled per master instruction** (`scu.rs:706-727`; system.rs `step_cpus`).
- **H2 — vector/priority hand-coded.** **✅ resolved** — `Source::vector()` gives
  internal `0x40+index`; the external CD source (`Source::Cd = 16`) lands at
  `0x50`, matching Mednafen's internal/external split (`scu.rs:84-96`).
- **H3 — no level re-assertion; one source per drain.** **✅ resolved for the
  level case** — the CD external interrupt is driven as a live level
  (`set_cd_int`, `(HIRQ & HIRQ_Mask) != 0`); VBlank-IN/OUT remain raised as edges
  on the raster transition (functionally correct, since they're momentary
  sources) (`system.rs:703-725`).
- **H4 — IMASK reset/auto-mask wrong.** **✅ resolved** — reset = `0xBFFF`, IMS
  writes masked to `0xBFFF`, and external interrupts masked by IMS **bit 15** via
  16-bit sign-extension (`~(int16)IMask`), matching Mednafen (`scu.rs:279,319,712`).
- **M5 — IST W1C polarity.** **✅ resolved** — `ist &= !val` (W1C), with a
  regression `ist_writes_are_write_one_to_clear` (`scu.rs:322`).
- **M6 — manual DMA with count 0 skipped.** **✅ resolved** — `dma_count` promotes
  0 → channel max (1 MiB ch0 / 4 KiB ch1-2) (`system.rs:209-217`).
- **M7 — indirect-mode write-back address.** ○ open — not revisited in this pass.
- **M8 — Timer0/Timer1 unimplemented.** **◑ partial** — Timer1 is a full per-line
  down-counter (reload at HBLANK, fires on underflow, TENB-gated); Timer0 is a
  raster line-compare (no free-running counter) (`scu.rs:627-672`,
  `system.rs:734-738`).
- **M9 — DSP-end IRQ not deasserted on PPAF read.** **✅ resolved** — the EF flag
  is cleared on the PPAF read (`scu.rs:437-441`).
- **L10–L12** — DMA stride reset defaults, AIACK/ABusIProhibit, register field
  masks. **✅ AIACK/`cd_prohibit`** present (set on CD fire, cleared by AIACK bit 0,
  `scu.rs:227,329`); the rest unverified/minor.
- *Consistent:* register offset map, internal vector base, priority levels,
  channel widths, byte/halfword-don't-trigger-DMA, indirect end-flag, DSP ports.

### SMPC (`smpc.rs`) vs `smpc.cpp`

- **1 (H) — INTBACK status SR wrong.** **✅ resolved** — `SR = (SR&~0xA0)|0x0F|npe`
  (masks bits 7/5, sets the low nibble, ORs `SR_NPE=0x20` when a peripheral phase
  follows), the staged-protocol form (`system.rs:1115`).
- **2 (H) — OREG0 RESD/STE static.** ○ open — `ResetNMIEnable` still unmodeled.
- **3 (H) — INTBACK peripheral OREG layout.** ◑ partial — the multi-phase SF/timing
  is modeled; the full nibble-stream peripheral payload is still simplified.
- **4 (H) — CKCHG is a no-op.** **✅ resolved** — `CkChg320/352` halts the slave and
  raises the master **NMI**, which the BIOS `ChangeSystemClock` waits on
  (`system.rs:1145-1151`).
- **5 (H) — SF busy/ready phasing.** **✅ resolved** — SF is held busy across the
  multi-phase fetch via `intback_complete_at` (set per phase, cleared by
  `settle_intback` when the cycle is reached), reconciled to Mednafen's 4 MHz
  SMPC clock (`smpc.rs:128`, `system.rs:1116-1174`).
- **6–10 (M)** — INTBACK-when-no-status, SNDON/SNDOFF 68k reset, SYSRES,
  RESENAB/RESDISA, region default. **◑ partial** — region default `0x04` (NA),
  overridable; SNDON releases the sound 68k but via a full reset (see SCSP below);
  the rest largely unmodeled (BIOS-tolerated).
- **11–13 (L)** — OREG10/11 static; OREG16+ blanket `0xFF`; power-on master NMI. ○.
- *Consistent:* all command codes, odd-byte register addressing, RTC/SETTIME,
  SSHON/SSHOFF→slave, SETSMEM echo.

### System glue (`bus.rs`, `scheduler.rs`, `system.rs`, `memory.rs`, `cartridge.rs`) vs `ss.cpp`, `cart.cpp`

- **1 (H) — slave not reset on SSHON.** **✅ resolved** — `release_slave` now calls
  the slave's full power-on `reset` (re-fetch PC/SP from `0x00000000`, VBR=0) and
  resyncs its cycle, and the slave's **BCR1 master/slave bit** is set so the BIOS
  cold-start takes the slave path instead of re-initialising WRAM
  (`system.rs:366-382,327-336`).
- **2 (H) — SSHOFF should also reset.** **◑ by-design** — `halt_slave` sets only
  `halted`; the full re-vector happens on the next `SSHON`/`release_slave`, so an
  off/on cycle still re-vectors the slave.
- **3 (H) — coarse 256-cycle batch scheduling.** **✅ resolved** — replaced by an
  **event-clamped** interleave: each batch is clamped to the next peripheral-event
  edge (`cycles_to_next_event`: VBlank-IN/OUT, INTBACK-completion, VDP1 draw-end,
  Timer0 line-compare), capped by `SMPC_POLL_QUANTUM = 256`. The SH-2 pair steps
  **master-leads-slave** (`step_cpus`) with SCU interrupts sampled per master
  instruction (`system.rs:801-865,935`).
- **4 (H) — SCU DMA synchronous/instant.** **◑ by-design** — still synchronous, but
  now charges per-access wait-state cost and stalls the requesting CPU
  (cycle-steal approximation) (`system.rs:118-176,974-1000`).
- **5 (M) — per-region wait states differ.** **○ open (the big one)** — current
  flat defaults (BIOS 10, backup 6, low WRAM 3, high WRAM 1 r/w, VDP1 14/11, VDP2
  20/5, CD-block A-bus CS2 8). The full **per-access SH-2 cycle model** (shared
  `SH7095_mem_timestamp`, region-weighted waits + CPU↔CPU contention, SDRAM page
  timing) is the one large, golden-churning item still deferred to **M13**
  (`bus.rs:73-314`).
- **6 (M) — low work RAM window.** **○ open (minor)** — maps 1 MiB; Mednafen
  decodes 2 MiB with the upper 1 MiB returning `0xFFFF` (revision-dependent)
  (`bus.rs:48-49`).
- **7 (M) — FRT input-capture.** **✅ resolved** — applied per instruction (not a
  deferred batch) via `apply_fti!` after each CPU step; 16-bit writes pulse it
  (32-bit composes from two 16-bit) (`system.rs:885-910`).
- **8 (M) — backup-RAM high byte.** **◑ by-design** — even byte lanes read `0x00`
  (the MAME `backupram_r` odd-byte packing now used consistently); the `DB|0xFF00`
  open-bus nuance is unmodeled (`memory.rs:197-202`).
- **9 (M) — cart backup-RAM packing.** **✅ resolved** — now 1 byte per 16-bit word
  at odd addresses, consistent with the internal backup RAM (`cartridge.rs:185-202`).
- **10 (M-L) — cart ID.** **○ open (minor)** — only at the exact `0x04FF_FFFF`
  (`cartridge.rs:46,162`).
- **11–13 (L)** — SCSP RAM mirror span, CS1/CS2 routing, RTC frame-rate. ○ minor.
- **`run_frame`** — **✅** runs the whole frame in a **single** `run_for(CYCLES_PER_FRAME)`
  (the active+VBLANK split that diverged the master was removed; this was a VF2
  stall fix) (`system.rs:1457-1481`).
- *Consistent (lower 1 MiB common cases):* region dispatch shape, the FTI region
  selectors, internal backup packing, cart enum.

### VDP2 (`vdp2/*.rs`, `system.rs::update_video_timing`) vs `vdp2.cpp`, `vdp2_render.cpp`

- **H1 — TVSTAT.VBLANK ignores display-off.** **✅ resolved** — VBLANK reads 1 when
  display is off (incl. power-on), OR'd with the raster-derived bit
  (`vdp2/mod.rs:71-82`).
- **H2 — VBlank-OUT/VBLANK-clear a line late.** **○ open** — still wraps at the
  frame boundary (~1-line phase error); marginal, golden risk (`system.rs:718`).
- **H3 — HBlank-IN never raised.** **✅ resolved** — raised on the HBLANK rising
  edge per scanline, TENB-gated (`scu.rs:627-670`).
- **H4 — SCU Timer0/Timer1 storage-only.** **✅/◑** — Timer1 fully counts/fires;
  Timer0 is line-compare (see SCU M8).
- **M1 — sprite CRAM offset (CRAOFB) ignored.** **✅ resolved** — CRAOFB applied as
  the RBG CRAM offset; the broader **colour-offset** feature (C7) also landed
  (`vdp2/regs.rs:255-263`).
- **M2 — RBG1 added as extra layer.** **✅ resolved** — RBG0 selects its rotation
  parameter set via **RPMD 0/1**; RBG1 shares NBG0's slot in dual-rotation
  (`vdp2/renderer.rs:602-610`).
- **M3 — color-calc ratio blend off-by-one.** **◑ partial** — the **special
  priority / special colour-calc** modes (C4) are implemented per-dot; the exact
  ratio rounding (`/0x1F` vs `>>5`) is not separately re-verified
  (`vdp2/renderer.rs`).
- **M4 — sprite shadow / type 8–F handling simplified.** **○ open** — deferred
  until a game exercises it.
- **M5 — ODD bit toggles in progressive mode.** **○ open** — still toggles per
  frame; should be constant 1 when LSMD ≠ 3 (`system.rs:691-692`).
- **L1–L3** — PAL/EXLATCH bits, HBLANK width, VCNT latch-on-read. ○ minor.
- **Hi-res output (640/704) + NBG0/1 reduction + fractional scroll** — **✅** added
  in the M11/M13 Tier C push (`vdp2/regs.rs:130-148`, `vdp2/renderer.rs`).
- *Consistent:* layer set, char/bitmap addressing basics, CRAM banking for NBG.

### VDP1 (`vdp1/*.rs`) vs `vdp1.cpp`, `vdp1_*.cpp`

**✅ mostly resolved** — the full command-list plotter shipped (M5): command
walker (END/NEXT/JUMP/CALL/RETURN), normal/scaled/distorted sprites + polygons +
lines, all six CMDPMOD colour modes, **SPD-governed transparency**, per-mode
**end-codes**, **gouraud** shading, the **half-transparency / shadow / half-luminance**
colour-calc modes, **MSBON** (set MSB without overwriting colour), scaled-sprite
zoom-point decode, and a cycle-exact **draw-end** (`vdp1/plotter.rs`).

Still simplified / **○ open:** erase targets the draw buffer rather than the
*displayed* (non-draw) buffer at swap; and some 2nd-user-clip (type 0xB) edge
cases. The original "Med/Low" item list below is retained as the spec of what was
implemented:

- *(implemented)* RGB-mode transparency via raw==0; RGB end-code pattern; SPD
  governs polygon/line transparency; gouraud + half-transparency on polygons;
  MSBON read-modify; scaled-sprite two-axis zoom; draw-end at list completion.
- *(open/minor)* erase-on-displayed-buffer at swap; half-transparent blend
  rounding; coordinate masking widths; EDSR BEF/COPR→LOPR at swap.

### SCSP / MC68EC000 (`scsp/*.rs`, `crates/m68k/`) vs `scsp.inc`, `sound.cpp`

- **SNDON does a full 68k reset every time.** **○ open** — `SndOn` still calls
  `cpu.reset` (full reset) rather than an un-halt; no `SetExtHalted`-style gate
  (`scsp/mod.rs:1519-1526`). A SNDON-after-running re-resets the driver.
- **per-sample interrupt (SCIPD/MCIPD bit 10, `0x400`) never generated.** **○ open**
  — sound drivers clocked off the per-sample tick get none; `SoundRequest` is only
  sourced from the timers.
- **sound IRQ level encoding.** **◑ partial** — picks one source by priority
  (`decode_sci`) rather than bitwise-OR of all enabled SCILV levels
  (`scsp/mod.rs:579-598`).
- **slot CA monitor (`0x408`).** **✅ resolved** — `slot_monitor()` computes the
  MSLC slot's CA/EG-phase/level live, instead of returning a static backing byte;
  this was the boot-jingle BGM-loop root (`scsp/mod.rs:416-454`).
- **the BIOS-BGM silence** *(the whole "voices never key" saga that motivated much
  of this and the SCSP fidelity items)* — **✅ resolved**: the root was the `m68k`
  `ADDA.L`/`SUBA.L` → `ADDX`/`SUBX` decode bug (`32662f7`, guard
  `op & 0x00C0 != 0x00C0`, `m68k/interpreter.rs:1049`), plus the SCSP sound-RAM
  access wait-state and interleave-budget carry. See
  [`bootstrapping.md` §B.7](bootstrapping.md#b7-the-boot--cd-player-panel-bgm-resolved-2026-06-06).
- **Low/Med (audio fidelity):** EG model, LFO, FM phase, loop-mode, DSP MIXS
  scaling, master volume, timer reload. **◑** — several addressed during the BGM
  work (DSP effect-send scaling, MIXS-wrap, master volume reg); LFO and the
  ms-table-vs-counter EG remain approximations.
- *Consistent:* 68k interrupt delivery (autovector, IPL, NMI), memory map,
  main-interrupt level-latch.

### CD-block (`cd_block.rs`) vs `cdb.cpp`

- **✅ status report uses live `cd_curfad`** (was the stale `self.fad`,
  `9e0ea9f`; current at `cd_block.rs:946-950`).
- **✅ HIRQ register model is W1C/sticky** — reading HIRQ no longer clears
  `DCHG|BFUL|CSCT`; and the host's `DCHG` write-1-to-clear **also clears the
  internal `disk_changed` latch**, which was the actual M11 boot root (it stopped
  `Init` re-raising `DCHG` and looping recognition) (`cd_block.rs:765-850`).
- **✅ `DrivePhase::Startup`** — a disc present at power-on/insert reports
  `STATUS_BUSY` for ~1 s of recognition spin-up before settling to PAUSE, so the
  BIOS plays its disc-present boot animation (`cd_block.rs:255-266,1454-1462`).
- **✅ CD→SCU external interrupt** — the CD-block drives `Source::Cd` (IST bit 16,
  vector `0x50`, level 7) as a level (`irq_active()`), masked by IMS bit 15,
  re-armed by AIACK (M11).
- **✅ `NODISC` (0x07), not `PAUSE`, when empty** (`cd_block.rs:556-565`).
- *Consistent:* command set, buffer/filter/partition engine, read pump, data
  transfer, ISO9660 FS, auth/region — match Mednafen through the IP.BIN read and
  on into the 1st-read load.

---

## Fix plan & risks (retrospective)

The plan below was followed; its outcome is folded into the status tags above.

1. The **boot-critical queue** was worked in order, each as its own commit with
   the `bios_boot` golden re-verified — all eight items landed.
2. The **SCU interrupt rework (#5)** was isolated as planned; it is now the
   per-instruction level-sampled model and underpins the M11 CD external
   interrupt.
3. **VDP1/VDP2 rendering and SCSP fidelity** were addressed post-boot (M11 / M13
   Tier C and the BGM work) — most items resolved, the rest deferred until a game
   exercises them.
4. The **256-cycle batch scheduler (system #3)** was the dominant architectural
   divergence and *was* reworked (event-clamped, master-leads-slave). The one
   remaining large item is the **per-access SH-2 bus-timing model** (system #5),
   deferred to M13 because it is golden-churning and system-wide.
