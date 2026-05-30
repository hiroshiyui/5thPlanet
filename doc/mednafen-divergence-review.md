# Mednafen cross-reference divergence review

**Date:** 2026-05-30 · **Reference:** Mednafen / Beetle Saturn (`mednaref/src/ss/`)

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

## Headline conclusion

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

## Boot-critical fix queue (prioritized)

| # | Fix | Subsystem | Risk | Confidence |
|---|-----|-----------|------|-----------|
| 1 | TVSTAT.VBLANK reflects display-off (=1 when display off / power-on) | VDP2 | low | high |
| 2 | IMASK reset = `0xBFFF`; IMS writes masked to `0xBFFF` | SCU | low | high |
| 3 | VBlank-OUT/VBLANK-clear edge at last line (262), not line 0 | VDP2 | low | med |
| 4 | Raise HBlank-IN per line; implement SCU Timer0/Timer1 | VDP2/SCU | med | med |
| 5 | IST = live *pending* set (separate asserted vs pending; level re-assert; clear only on W1C / vector-fetch) | SCU | **high** (golden) | high |
| 6 | Slave SH-2 full-reset on `SSHON`/`SSHOFF` (LLE), VBR=0, reset vector | system | med | high |
| 7 | `CKCHG` performs subsystem reset + master NMI | SMPC | med | med |
| 8 | INTBACK status SR = `(SR&~0x80)|0x0F`; OREG0/10/11 from live state | SMPC | med | med |

Each fix re-verifies the `bios_boot` splash golden and re-checks the VF2 LLE
boot trace before moving on. #5 (the SCU interrupt rework) is the deepest and
riskiest; do it on its own with the golden as guard.

**Status (2026-05-30):** four boot-critical consolidation fixes landed — #1
`3e43928`, #2 `5ce37d4`, #6 `879bba7` (slave power-on-reset on SSHON), #8-SR
`534a7ba` (INTBACK status SR = `(SR&~0x80&~NPE)|0x0F`). All correct, golden-safe
(splash hash unchanged), full suite + clippy green. **VF2 still gives up** —
expected: the post-IP.BIN rejection is a *convergence* and none of these is the
single unblock.

**Reassessment of the remaining queue:**
- **#5 (SCU IST live-pending / level re-assert)** — on closer scoping this is
  *unlikely* the VF2 unblock: the boot loader barely touches the SCU IST (~3
  register writes the whole run; it reads the CD HIRQ instead), and the
  boot-path interrupt sources (VBlank-IN/OUT, HBlank) are edges, not held
  levels, so the level-re-assertion substance is moot for boot. It's a real
  Mednafen-consolidation fix but high-risk (golden) + low boot payoff. Deferred.
- **#3 (VBlank-OUT edge)** — a 1-line phase shift; marginal, golden risk. Deferred.
- **#7 (CKCHG subsystem-reset + master NMI)** — involved and *not* low-risk
  (injecting a master NMI mid-boot is a large behavior change), and it isn't
  blocking us (we proceed past display init). Warrants its own focused pass.
- **#8-OREG / #4 timers** — minor / BIOS-ignored fidelity.

**The actual VF2 unblock is still the unpinned, overlay-obscured value the
loader reads when it rejects the disc post-IP.BIN** — that needs a focused deep
trace (master read-watch / step-trace through the loader's validation), not more
interrupt-model edits. The consolidation pass above has materially de-tangled
the boot-path interrupt/timing model against Mednafen, which was the goal.

---

## Findings by subsystem

### SCU (`scu.rs`, `crates/scu_dsp/`) vs `scu.inc`, `scu_dsp_*.cpp`

- **H1 — IST exposes ack-cleared state, not pending.** `take_pending_interrupt`
  clears `ist` on the vector-take (`scu.rs:~576`); a handler reading IST (0xA4)
  sees the bit already gone. Mednafen separates `IPending` (set on assert,
  cleared only by W1C or vector-fetch) from `IAsserted` (live line, drives
  re-pend). Ours conflates them and clears too aggressively. *Prime boot suspect.*
- **H2 — vector/priority hand-coded.** We use `0x40+index` for all 14 sources;
  Mednafen splits internal (`0x40+bit`, `internal_tab`) vs external/A-bus
  (`0x50+bit`, `external_tab`), selected by tzcount. Priority levels for common
  sources *do* match `internal_tab`.
- **H3 — no level re-assertion; one source per drain.** `raise()` sets an edge
  bit consumed once; a held level never re-pends. `system.rs` only ever
  `raise()`s VBlankIn/Out (rising edge), never deasserts; Mednafen drives them
  as levels (`SetInt(..., active)`) each line.
- **H4 — IMASK reset/auto-mask wrong.** Reset leaves `ims=0` (all unmasked);
  Mednafen resets to `0xBFFF`, re-sets `IMask=0xBFFF` on every vector-fetch, and
  masks IMS writes to `0xBFFF`.
- **M5 — IST W1C polarity.** We clear bits set in `val` (`ist &= !val`);
  Mednafen keeps bits where the data bit is 1 within the lane (`IPending &= DB |
  ~mask`). Verify vs the SCU manual.
- **M6 — manual DMA with count 0 skipped.** Mednafen promotes 0 → max length.
- **M7 — indirect-mode write-back address.** Mednafen writes back the *table*
  pointer for indirect; we don't distinguish indirect vs direct.
- **M8 — Timer0/Timer1 unimplemented** (registers store, never count/fire).
- **M9 — DSP-end IRQ not deasserted on PPAF read.**
- **L10–L12** — DMA stride reset defaults, AIACK/ABusIProhibit, register field masks.
- *Consistent:* register offset map, internal vector base, priority levels,
  channel widths, byte/halfword-don't-trigger-DMA, indirect end-flag, DSP ports.

### SMPC (`smpc.rs`) vs `smpc.cpp`

- **1 (H) — INTBACK status SR wrong.** We set `sr = 0x40 | (stage<<5)`;
  Mednafen does `SR = (SR&~0x80)|0x0F` (+`SR_NPE` if peripheral requested).
- **2 (H) — OREG0 RESD/STE static.** Hardcoded `0x80`; should be
  `(RTC.Valid<<7)|(!ResetNMIEnable<<6)`. We don't model `ResetNMIEnable`.
- **3 (H) — INTBACK peripheral OREG layout** doesn't match the real nibble-stream
  protocol (static bytes vs the JR engine / per-port loop).
- **4 (H) — CKCHG is a no-op.** Should reset SOUND/VDP1/VDP2/SCU, switch clock,
  wait vblanks, then **NMI the master** — which the BIOS waits on.
- **5 (H) — SF busy/ready phasing.** We clear SF between INTBACK phases; Mednafen
  holds SF set across the whole multi-phase fetch (BIOS polls SF tightly).
- **6–10 (M)** — INTBACK runs even when no status requested; SNDON/SNDOFF 68k
  reset path; SYSRES no-op; RESENAB/RESDISA unmodeled; region default `0x04`
  (NA) mismatches a JP/EU BIOS unless overridden (wrong region = hard halt).
- **11–13 (L)** — OREG10/11 static; OREG16+ blanket `0xFF`; power-on master NMI.
- *Consistent:* all command codes, odd-byte register addressing, RTC/SETTIME,
  SSHON/SSHOFF→slave, SETSMEM echo.

### System glue (`bus.rs`, `scheduler.rs`, `system.rs`, `memory.rs`, `cartridge.rs`) vs `ss.cpp`, `cart.cpp`

- **1 (H) — slave not reset on SSHON.** `release_slave` only resyncs cycle +
  un-halts; Mednafen `SetActive(true)` calls a full power-on `Reset` (VBR=0,
  imask=0xF, re-fetch PC/SP from `0x00000000`). On LLE the slave must come up at
  the BIOS reset vector, not resume stale state.
- **2 (H) — SSHOFF should also reset.** We only set `halted`; Mednafen resets,
  so an off/on cycle re-vectors the slave.
- **3 (H) — coarse 256-cycle batch scheduling** vs Mednafen's event-exact
  interleave (`SH7095_mem_timestamp`, per-chip event handlers). Peripheral side
  effects (DMA done, SCU assert, sound IRQ) observed up to ~256 cycles late;
  mis-orders interrupts vs CPU poll loops. Dominant architectural divergence;
  not a quick fix.
- **4 (H) — SCU DMA synchronous/instant**, no start-factor/cycle-steal interleave.
- **5 (M) — per-region wait states differ** (BIOS 10 vs +8, low RAM 3 vs +7,
  STUB 0 under-counts). Re-derive from `BusRW_DB_CS0`.
- **6 (M) — low work RAM window** maps 1 MiB (`0x00200000..0x002FFFFF`); Mednafen
  decodes 2 MiB with the upper 1 MiB returning `0xFFFF` (revision-dependent).
- **7 (M) — FRT input-capture** applied as a deferred batch pulse, 16-bit only;
  Mednafen fires immediately on any non-byte write (16 *or* 32-bit). The pulse
  is ~256 cycles late (master→slave wake path).
- **8 (M) — backup-RAM high byte reads `0x00`** here vs `0xFF` on hardware
  (`DB | 0xFF00`). SMPC reads similarly OR `0xFF00` — check `smpc.rs` too.
- **9 (M) — cart backup-RAM packing** uses a 4-byte stride; Mednafen (and our
  *internal* backup) use 1 byte per 16-bit word at odd addresses. Inconsistent.
- **10 (M-L) — cart ID** only at exact `0x04FFFFFF`; Mednafen decodes a window
  and the even-1 address.
- **11–13 (L)** — SCSP RAM mirrors 512 KiB across 1 MiB (upper half should be
  unmapped); CS1/CS2 flat stub vs SCU/A-bus routing; RTC frame-rate constant.
- *Consistent (lower 1 MiB common cases):* region dispatch shape, the FTI region
  selectors, internal backup packing, cart enum.

### VDP2 (`vdp2/*.rs`, `system.rs::update_video_timing`) vs `vdp2.cpp`, `vdp2_render.cpp`

- **H1 — TVSTAT.VBLANK ignores display-off.** We derive VBLANK purely from
  raster position; Mednafen forces `InternalVB = !DisplayOn` (and reset
  `InternalVB=true`). With display off (incl. power-on), hardware reads
  VBLANK=1 continuously; we only pulse it. **The BIOS waits for VBLANK before
  enabling display** — strong boot suspect.
- **H2 — VBlank-OUT/VBLANK-clear a line late.** We wrap at line 0; Mednafen
  clears at the last line (262 NTSC). ~1 scanline phase error every frame.
- **H3 — HBlank-IN never raised** (source defined, never fired); drives SCU
  Timer0 + line interrupts.
- **H4 — SCU Timer0/Timer1 storage-only** (mirror of SCU M8).
- **M1 — sprite CRAM offset (CRAOFB) ignored** → wrong sprite palette bank.
- **M2 — RBG1 added as extra layer** instead of replacing NBG0 in dual-rotation.
- **M3 — color-calc ratio blend off-by-one** (`/0x1F` vs `>>5`, fore=`ratio^0x1F`).
- **M4 — sprite shadow / type 8–F handling simplified** (no `src&0xFF` mask).
- **M5 — ODD bit toggles in progressive mode** (should be constant 1).
- **L1–L3** — PAL/EXLATCH bits, HBLANK width approximation, VCNT latch-on-read.
- *Consistent:* layer set, char/bitmap addressing basics, CRAM banking for NBG.

### VDP1 (`vdp1/*.rs`) vs `vdp1.cpp`, `vdp1_*.cpp`

(Not on the critical boot path — a game must launch first — but these make a
launched game render wrong; medium priority once booting.)

- **Med:** RGB-mode (mode 5) transparency uses raw==0 not bit-15 (`<0x4000`);
  RGB end-code is the `0xC000==0x4000` pattern + 2-consecutive `ec_count`, not
  equality with `0x7FFF`; polygon/line transparency governed by SPD not color
  value (color-0 polys vanish); polygons skip gouraud + half-transparency; MSBON
  should read-back FB and set only MSB, not overwrite color; scaled-sprite
  zoom-point two-axis decode; erase targets the *displayed* (non-draw) buffer at
  swap, not the draw buffer pre-plot; draw-end interrupt on a synthetic timer not
  at list completion; jump field is bits 13:12 only (no skip-draw bit), type 0xB
  is a 2nd user-clip (we end the list).
- **Low:** half-transparent blend formula, coordinate 13/11/13-bit masking,
  user-clip masking, gouraud DDA vs bilinear, EDSR BEF/COPR→LOPR at swap.

### SCSP / MC68EC000 (`scsp/*.rs`, `crates/m68k/`) vs `scsp.inc`, `sound.cpp`

- **Med/High — SNDON does a full 68k reset** every time instead of un-halt;
  Mednafen resets once at power-on and only halts/unhalts (`SetExtHalted`). A
  SNDON-after-running re-resets the sound driver (can stall a game's
  sound-handshake init).
- **Med — per-sample interrupt (SCIPD/MCIPD bit 10, `0x400`) never generated**;
  Mednafen sets it every sample. Sound drivers clocked off it get no tick; main
  CPU `SoundRequest` only ever sourced from Timer A here.
- **Med — sound IRQ level encoding** picks one source by priority vs Mednafen's
  bitwise-OR of all enabled SCILV levels.
- **Low/Med (audio fidelity):** EG model (ms-table vs counter RE), no LFO, FM
  phase / `0x400^FreqNum` vs `+0x400`, loop-mode semantics, DSP MIXS scaling,
  DSP not run per-sample, CDDA routed to output not EXTS, master volume absent,
  timer reload semantics.
- *Consistent:* 68k interrupt delivery (autovector, IPL, NMI), memory map (512
  KiB case), main-interrupt level-latch.

### CD-block (`cd_block.rs`) vs `cdb.cpp` — this session

- **FIXED (`9e0ea9f`):** status report used the stale `self.fad` instead of the
  live `cd_curfad` — reported the head parked at the IP.BIN start after a read.
- **HIRQ register model:** reading HIRQ cleared `DCHG|BFUL|CSCT` *before
  returning* — non-faithful (HIRQ is W1C/sticky on hardware; Mednafen keeps the
  bits). Parallels SCU IST H1/M5. (Tested CSCT-sticky; correct but not the boot
  fix on its own.)
- **Ruled out as the boot cause this session:** IP.BIN content (full 32 KB
  verified valid), the FAD report, Play status (PLAY vs BUSY), and HIRQ bits
  (MPED/CSCT) — none change the loader's reject decision. The recognition →
  auth → region → IP.BIN-read sequence matches Mednafen command-for-command; the
  divergence is the post-IP.BIN decision, which is **not** a CD-block output.
- *Consistent:* command set, buffer/filter/partition engine, read pump, data
  transfer, ISO9660 FS, auth/region — all match Mednafen through the IP.BIN read.

---

## Fix plan & risks

1. Work the **boot-critical queue** above in order, each as its own commit, each
   re-verifying the `bios_boot` golden and re-running the VF2 LLE boot trace.
2. The **SCU interrupt rework (#5)** is the deepest change and the highest-risk
   for the golden — isolate it; expect to regenerate the splash golden only if a
   visual check confirms the new frame is still correct.
3. After the boot path, address VDP1/VDP2 rendering and SCSP fidelity (post-boot
   quality) and the M/L system-bus items as a coherence pass.
4. The **256-cycle batch scheduler (system #3)** is the dominant architectural
   divergence but a large rework; defer unless the targeted interrupt-model
   fixes don't unblock the boot.
</content>
</invoke>
