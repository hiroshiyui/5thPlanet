# 5thPlanet Roadmap

Accuracy-first SEGA Saturn emulator in Rust. Milestones are scoped tightly so
the foundation is solid before the next chip is added.

The full design rationale lives in `/home/yhh/.claude/plans/temporal-strolling-hollerith.md`.

## Milestone 1 — Cycle-accurate SH-2 (SH7604) core ✅ complete

Single-chip deliverable: a standalone `sh2` library crate validated by unit
tests and ROM regressions, ready to be wired into a bus/scheduler later.

| # | Task | Status |
|---|------|--------|
| 1 | Workspace + `sh2` skeleton + `Bus` trait + `Cpu` struct | ✅ done |
| 2 | Full SH-2 ISA table (~142 ops) + decoder + decoder unit tests | ✅ done |
| 3 | Interpreter — first batch of ~20 core opcodes (MOV/ALU/CMP/branches with delay slots) + integration tests | ✅ done |
| 4 | Remaining ~120 opcodes, group by group, with tests alongside | ✅ done |
| 5 | Pipeline / cycle model: 5-stage scoreboard, load-use stalls, multiply latency, branch costs, interlock timing tests | ✅ done |
| 6 | Cache (4 KiB 4-way LRU) + on-chip peripherals (INTC, DMAC, DIVU, FRT; BSC/WDT/SCI/UBC as register stubs) | ✅ done |
| 7 | Exception + interrupt dispatch (reset, illegal, slot-illegal, address error, NMI, TRAPA, external via INTC) | ✅ done |
| 8 | ROM regression harness + committed golden state hashes | ✅ done |

### What landed (`cargo test -p sh2` → 131 tests, 0 failures)

- 37 lib unit tests (decoder, cache, divu, frt, intc, onchip aggregator)
- 16 opcode integration tests for the core MOV/ALU/CMP/branch batch
- 18 arithmetic tests (ADDC/ADDV/MAC/DIV1/EXT/multiplies)
- 12 data-transfer addressing-mode tests
- 5 logical (AND/OR/XOR/NOT/TST + TAS)
- 6 shift / rotate (incl. SHLLn / SHLRn)
- 9 system (LDC/STC/LDS/STS + TRAPA round-trip)
- 11 pipeline timing tests (load-use, branch costs, MAC-read framework)
- 4 on-chip routing tests (CPU drives DIVU via real MOV.L)
- 8 exception/interrupt tests (illegal, slot-illegal, NMI, external, masking)
- 5 ROM regression tests with committed golden hashes

The "SingleStepTests vector corpus" originally proposed as a gate was dropped:
no public SH-2 corpus exists yet, and the per-opcode unit tests + ROM hashes
cover the same ground without the generator infrastructure overhead.

## Milestone 2 — Saturn bus, dual SH-2, event-driven scheduler ✅ complete

Pairs the M1 SH-2 with a Saturn-shaped memory map, a second SH-2 (slave), and
an event-driven scheduler that decides which CPU advances next. Wires the M1
cache structure into live fetch/data paths so cycle counts on cache-resident
code (which is most Saturn code) start matching hardware. Scope is deliberately
**no graphics, no audio, no BIOS boot, no SDL2** — those wait for M3 once
there's something to render.

| # | Task | Status |
|---|------|--------|
| 1 | Extend `sh2::Cache` with line data storage + write-through update API | ✅ done |
| 2 | Wire cache into `Cpu::mem_read*/mem_write*` (cached vs cache-through dispatch) | ✅ done |
| 3 | Saturn bus + typed region structs + memory-map dispatch | ✅ done |
| 4 | Event-driven `Scheduler` with `SchedEntity` trait | ✅ done |
| 5 | `Saturn` system aggregate + dual SH-2 integration test | ✅ done |

### What landed (`cargo test --workspace` → 156 tests, 0 failures)

- 137 `sh2` tests (M1's 131 + 1 cache-storage + 5 cache-wiring)
- 9 `saturn::bus_routing` — every region round-trips, BIOS mirrors, unmapped is open bus
- 7 `saturn::scheduler` — determinism, fairness, real-`Sh2Entity` cosched on `SaturnBus`
- 3 `saturn::dual_sh2` — master writes sentinel → slave observes within budget;
  fairness drift bounded; reset-vector load from BIOS image works

The cache-wiring tests serve double-duty as both "task #2 done" and the M2
verification gate "second read of the same address from master costs fewer
cycles" — the `CountingBus` directly proves the hit path.

## Milestone 3 — SCU, SMPC, VDP2 minimal, SDL2: BIOS to splash ✅ scaffolding complete

Goal: **the SEGA logo on screen.** A real BIOS image boots, the splash
renders, and an SDL2 window displays it. Stands up the system bridge
(SCU + DMA + interrupt aggregator), the slave-release path (SMPC), the
display generator (VDP2 minimal — one NBG layer), the SCU-DSP, and the
frontend shell.

| # | Task | Status |
|---|------|--------|
| 1 | SMPC — registers + `SETSL`/`SETSM` slave hold-release | ✅ done |
| 2 | SCU registers + DMA channels (3 channels, synchronous transfer) | ✅ done |
| 3 | SCU interrupt aggregator + wiring into SH-2 master INTC | ✅ done |
| 4 | `scu_dsp` crate — 32-bit DSP core (ISA, decoder, interpreter, opcode tests) | ✅ done |
| 5 | VDP2 register bank + VRAM (512 KiB) + CRAM (4 KiB) | ✅ done |
| 6 | VDP2 minimal renderer — one NBG layer (bitmap + 4-cell tile, 8/16/32 bpp via CRAM) | ✅ done |
| 7 | SDL2 frontend skeleton — window, run loop, framebuffer texture upload | ✅ done |
| 8 | BIOS boot integration — load real BIOS, hash splash framebuffer against committed golden | ✅ done (regression baseline) |

### What landed (`cargo test --workspace` → 240 tests, 0 failures)

- 8 unit + 7 integration tests for SMPC (slave halt-on-reset, SSHON release, SSHOFF re-halt, SF transitions, IREG/OREG round-trip)
- 8 unit + 8 integration tests for SCU (DMA round-trips, INTC priority resolution, IST W1C semantics, DMA-end raising the right per-channel source, end-to-end DMA → master SH-2 vectors)
- `scu_dsp` (decoder, ALU, MVI, JMP cond+uncond, END/ENDI, runaway-microcode
  step cap). **Update (post-M4 coverage pivot):** M3's DSP was a placeholder
  (ALU-only, partly-invented encoding). Now a complete, spec-correct core —
  full VLIW operation word (ALU + X/Y/D1 data-move buses + multiplier),
  correct ALU op map, MVI/JMP/LPS/BTM with delay slots, END/ENDI, and DMA
  decoded into a queued request (`feat(scu_dsp): complete the DSP core`).
  2 decoder + 19 opcode tests. **Remaining (increment 2):** wire the four SCU
  host ports (PPAF/PPD/PDA/PDD) to drive program-load / start / data-RAM
  access, run the DSP from the SCU, execute its DMA over the system bus, and
  raise the SCU DSP-end interrupt on ENDI.
- 14 VDP2 unit tests + 6 integration through the bus + 6 renderer unit tests + 3 `Saturn::run_frame` integration
- 1 BIOS-boot regression test (gated on BIOS presence; asserts against committed golden hash)

### M3 close-out — honest reality

All 8 task scaffolds shipped; the SDL2 frontend opens cleanly and the test
suite is green. **The "SEGA logo on screen" goal is not yet met.** A real
Saturn BIOS image boots into an early init poll loop (master spins at PC
0x000002B2/0x000002B6 with `SR.imask = 15`, which masks even the
VBlank-IN we now raise at frame boundary). The BIOS is waiting on
peripheral data — most likely an SMPC `INTBACK` response or CD-block
status handshake, neither of which M3 modelled. With those landed in M4
the same harness will start showing meaningful framebuffer content.

The committed golden hash `0x2A0B972960C5E325` is the all-black current
output. It's still a useful regression baseline: if any of the 8 M3
components silently drift, this hash flips and the BIOS-boot test fails
loudly. The visual confirmation step in the original task description
becomes the entry criterion for M4's BIOS-splash effort rather than the
exit criterion for M3.

### Verification gates (all green)

1. `cargo test --workspace` — 240 tests pass.
2. `cargo test -p scu_dsp` — DSP per-opcode tests pass.
3. `cargo test -p saturn --test scu` — DMA transfers, INTC priority.
4. `cargo test -p saturn --test smpc` — `SETSL` releases the halted slave.
5. `cargo test -p saturn --test vdp2_render` — hand-crafted scene renders to known RGBA bytes.
6. `cargo test -p saturn --test bios_boot` — splash framebuffer hash matches golden (currently the all-black "stuck in BIOS init poll" hash).
7. Manual: `cargo run -p fifth_planet -- bios/Sega\ Saturn\ BIOS\ (USA).bin` opens an SDL2 window; window stays open and accepts close/Esc. Screen is black (not yet splash — see close-out notes above).

### Explicitly out of scope for M3

- VDP1 (sprites/polygons) — M4
- SCSP + M68k + audio — M4
- SMPC `INTBACK` peripheral data + keyboard input — M4
- CD-block (SH-1) handshake — M5
- Save states — M4 or M5 once the peripheral set stabilises
- Cycle-stealing DMA accuracy — refinement for whichever later milestone surfaces a game that needs it
- Multiple NBG/RBG layer compositing, transparency, line-scroll, mosaic, window planes — M4+

## Milestone 4 — Finish the M3 stretch: SEGA splash on screen 🚧 active

M3 shipped all the scaffolding but the BIOS parks in an early init poll
because it's waiting on peripheral data we don't model. M4 is laser-
focused on closing that gap. Everything originally sketched for M4
(VDP1, audio, save states, keyboard input) defers to M5 — keeping the
scope tight matches the project's one-chip-at-a-time discipline.

| # | Task | Status |
|---|------|--------|
| 1 | SMPC `INTBACK` — full response (no-controller status, OREG fill, raise SCU SMPC source) | ✅ done (incl. NMIREQ + 0x1A) |
| 2 | CD-block presence stub at `0x05_8980_00+` — defined "no disc, ready" reads, OK command responses | ✅ done |
| 3 | VDP1 register + VRAM + framebuffer stub at `0x05_C000_00`/`0x05_D000_00` (no rendering) | ✅ done |
| 4 | VDP2 register-decode fidelity — renderer reads `MPOFN`/`MPABN0..MPCDR0`/scroll from regs instead of constants | ✅ done |
| 5 | Iterate-to-splash — trace BIOS, fix the next blocker, repeat until splash renders | 🚧 in progress |
| 6 | Commit splash framebuffer hash as the new golden + visual confirmation via SDL2 | pending |

### Task #5 — REFOCUS: the goal is the SEGA splash *visible on screen*

The reference-diff work below (Yabause then MAME) was valuable — it proved
the SH-2 core/cache/SMPC/SCU/bus correct and fixed many real register/
protocol bugs — but it drifted into chasing MAME's exact instruction stream
and cycle phase, which is **not the goal and not ground truth**. Re-centering:

- **Goal:** boot a real BIOS to the SEGA logo, confirmed visually via SDL2.
- **Where we are:** the master parks at `0x060108BA` spinning on the WRAM
  flag `[0x060408A4]`, with **VDP2 display off (`TVMD=0`)** — so the screen
  is black (the current `bios_boot` golden is that black frame). The splash
  graphics are never loaded because the BIOS never gets past this wait.
- **The single concrete blocker:** `[0x060408A4]` is armed, on real hardware
  / the references, from inside an **interrupt-handler path** (VBlank-IN
  handler `0x06000840` → … → low-BIOS routine `0x2364`). Our VBlank handler
  *runs* but doesn't reach the code that writes the flag, so the main loop
  never proceeds to enable the display and draw the logo.
- **How to attack it (spec-first, not MAME-matching):** our cycle model is
  spec-correct (see the cycle-timing note below), so the fix is to find the
  genuine divergence in the *interrupt-handler path* — why our handler takes
  a different branch and skips the `[0x060408A4]` write — judged against the
  hardware manuals (SH7604 / VDP2 / SCU / SMPC), using the references only as
  a hint, never as the authority. Then: display-on → splash renders →
  task #6 (golden + SDL2 visual confirmation).

--- historical log (reference-diff narrative) ---

This task became a deep BIOS-boot debug. A headless build of the
**Yabause** reference emulator (kept locally, never committed) was
patched to log the master SH-2 PC stream, and our trace was diffed
against it instruction-by-instruction. That **proved the SH-2 core,
cache, SMPC, SCU, and bus correct** (bit-exact for 8.7M instructions)
and pinpointed each boot blocker to the exact instruction. Fixes that
landed from it:

- `fix(sh2)` — route `CCR` (`0xFFFFFE92`) to the cache; the BIOS could
  not enable the I-cache, so all cached code ran ~8× slow.
- `fix(saturn)` — correct the SMPC command codes (INTBACK=0x10,
  NMIREQ=0x18, SETTIME=0x16, RESENAB/RESDISA=0x19/0x1A); the swapped
  codes made INTBACK fire a spurious NMI.
- `fix(saturn)` — correct the INTBACK OREG status layout (7-byte RTC,
  area code at OREG9 = 0x04).
- `fix(saturn)` — model INTBACK execution time (~250 µs) instead of
  clearing SF instantly; the early SF-clear made the BIOS's poll exit
  a frame too soon.
- `feat(saturn)` — model VDP2 raster timing (`VCNT`/`TVSTAT`) live from
  the global cycle, so the BIOS can synchronize with the display.

Further fixes that landed (second debug pass, reference-diffed against a
60M-instruction Yabause trace):

- `feat(saturn)` — settle the INTBACK `SF` flag **on read** at the exact
  completion cycle (bus gains a per-instruction `cycle` field), so the
  BIOS's tight per-instruction SF-poll exits at the right instant rather
  than at the 256-cycle drain quantum.
- `fix(sh2)` — `LDC Rm,SR` / `LDC.L @Rm+,SR` are **not** illegal in a
  delay slot (only PC-rewriting ops are). `RTS; LDC Rm,SR` is the
  BIOS's "restore SR on return" idiom; flagging it illegal vectored the
  master into its `imask=15` dead-wait at `0x06000952`.
- `feat(saturn)` — back **SCSP sound RAM** (512 KiB at `0x05A00000`) so
  the BIOS's sound-RAM write-verify init completes (was open bus).
- `fix(saturn)` — the SCU presents **fixed interrupt vectors** (`0x40 +
  source index`) via external-vector-fetch, not the SH-2 auto-vector
  `64+level`. VBlank-IN was vectoring to `0x4F` (a stub) instead of
  `0x40` (the real handler), so the BIOS's per-frame work never ran.

The master now completes full BIOS init, runs the real VBlank handler
each frame, and its PC trace matches the Yabause reference **bit-exact
(modulo poll-loop iteration counts) for ~23.7M instructions**.

Then the **CD-block host-interface command protocol** landed
(`feat(saturn)`), replacing the data-port-only presence stub: correct
register base/offsets (`HIRQ@0x08`, `CR1..CR4@0x18..0x24`; the old stub
sat at the `0x0589_8000` data FIFO and left the command registers as
open bus), the power-on `"CDBLOCK"` signature, write-AND-to-clear `HIRQ`,
command dispatch (Get Status / Get Hardware Info / Get TOC / Init / End
Transfer, with a status-report default + `CMOK`), unsolicited periodic
status reports, and a disc-present (`PAUSE`) status — matching Yabause's
dummy CD core, which returns "disc present, spinning" so BIOS init
proceeds.

A follow-up pass (`feat(saturn)`) matched the CD-block's **register and
report behavior** to the reference exactly. Instrumenting Yabause's CD
events showed the BIOS issues **no CD commands during boot** — it only
reads the periodic status reports + HIRQ. Fixes:
- HIRQ reads recompute buffer/disc state: `BFUL` is always clear (no data
  buffered) → HIRQ reads `0xFFF7` not `0xFFFF`; `DCHG` is re-asserted from
  the disc-changed flag so a write-1-to-clear only sticks until the next
  read.
- Get Hardware Info clears the disc-changed flag.

Then the periodic report was moved onto **sub-frame scheduler-entity
timing** (`feat(saturn)`). The first cut frame-locked it to the VBlank
edge, but the reference doesn't: Yabause drives `Cs2Exec` **every
scanline** (yabause.c:798) and fires the report when its `_periodiccycles`
accumulator crosses `_periodictiming` (cs2.c:980) — landing at a
cycle-exact point *within* the frame, not at VBlank-IN. So `frame_tick`
became `CdBlock::tick(cycles)`, a free-running accumulator that emits one
report per `PERIODIC_CYCLES` (~one frame) and carries the remainder
forward (exact long-run cadence). A new `CdBlockEntity` `SchedEntity` —
wrapped with the SH-2s in a heterogeneous `SaturnEntity` enum — runs the
timer at scanline granularity alongside the cores; it owns no CD state and
reaches the CD-block through the bus. (This also future-proofs the
scheduler for the real SH-1, which becomes a CPU entity in M6.)

Verified: in steady state our `CR1=0x2100 CR2=0x4101 CR3=0x0100
CR4=0x0096 HIRQ=0xFFF7` match the reference bit-for-bit, now with
sub-frame report phase.

**Next blocker — the VBlank-IN handler, NOT the CD periodic.** The master
still parks at the high-WRAM VBlank-wait `0x060108BA`, spinning on a flag
at `0x060408A4`, display disabled (`TVMD=0`). The sub-frame CD timing
above was expected to unblock a CD-firmware liveness poll, but the park is
**unchanged** by it — and a fresh probe shows the real gate clearly: over
~1M single-steps the **VBlank-IN handler (`0x0600_0840`) does run** (it's
entered several times, SCU `IST` shows VBlank pending) yet `[0x060408A4]`
is **never armed**, so the wait loop never releases. The blocker is
therefore inside the VBlank-IN handler / display-enable path (what
condition the handler checks before arming the frame flag), not the CD
periodic. That handler indexes a callback table (`[0x06000960]`); the next
step is to trace why its `0x060408A4` store is skipped — candidates are an
unsatisfied SMPC/region check, an INTBACK-phase dependency, or a
still-missing VDP1/VDP2 status the handler gates on.

**Second reference added: MAME** (`mameref/`, never committed, alongside
`yabref/`). MAME v0.287's Saturn driver is built BIOS-only on its HLE CD
core (we lack the low-level CD-block SH-1 firmware) and runs headless; its
master-SH-2 PC trace (`mameref/pctrace.sh` → `/tmp/mame_pc.log`) diffs
against ours just like Yabause's. The Yabause harness binary was lost (only
its instrumented source survives), so MAME is now the live runnable
cross-check. See `mameref/README-5thplanet.md` for build/run/trace details
and the Saturn source map.

**OREG10 lead — disproven; the park is a wrong-path symptom.** MAME's
INTBACK status reply (`smpc.cpp resolve_intback`) sets `OREG10` ≈ `0x34`
(MSHNMI/SYSRES/SOUNDRES) and packs SMEM into `OREG12..15`, vs our `0x00` /
port-empty headers. Setting our `OREG10 = 0x34` does **not** move the park
(still `0x060108BA`, flag still 0), so `OREG10` is not the gate. The far
more useful finding came from watch/breakpoint experiments in MAME:

- MAME's master **never executes any `0x0601xxxx` high-WRAM code** in ~19M
  instructions — it stays in **low BIOS**, looping a routine at `0x00002364`
  (called from `0x2094`/`0x20CE`, in both normal `sr=0x01` and interrupt
  `sr=0xF1` context) that **is what writes `[0x060408A4]`** (values like
  `0x2F`/`0xE6`/`0xD6`, after an initial `0` clear at `0x2B0`).
- A breakpoint at `0x060108BA` in MAME **never fires** — so our park is a
  dead-end branch our master takes that the reference never does. The true
  divergence is **upstream**: our master wrongly leaves the low-BIOS path
  into a WRAM loop, never calling the `0x2364` routine that would feed the
  flag it then waits on.

Both masters run identical low-BIOS code for ~8.78M instructions.

**Resync diff built + first divergence pinned and fixed.**
`mameref/resync_diff.py` walks our master PC trace against MAME's (both
masked to physical addresses), resyncing across poll-loop count differences
(and isolated reset-logging off-by-ones) to report the first genuine branch
divergence. It pinned BIOS **`0x00002304`**: a `memcmp` of the CD-block
`CR1..CR4` (copied to WRAM `0x06001FD8`) against the expected `"CDBLOCK"`
signature at BIOS `0x4DB0`. MAME still read the signature there; we read a
periodic status report (`21 00 41 01 01 00 00 96`) and derailed. Two
CD-block bugs clobbered the signature too early — fixed (see
`fix(saturn): hold the CD-block signature until a real command`):
unsolicited periodics from power-on (now gated behind the first command,
matching MAME's HLE), and `execute()` firing on a lone CR4 write (now
requires all four CRs, MAME's `cmd_pending == 0xf`).

After the fix the divergence moved to BIOS **`0x00004216`** — the BIOS ORs
the CD-block **HIRQ** (`*(0x4264) == 0x25890008`) into a WRAM accumulator
(`0x060003A4`) and tests **CMOK (bit 0)**. MAME read `HIRQ = 0`; we read
`0x0BE1`. Root cause: our HIRQ power-on/read model, fixed (see
`fix(saturn): power-on HIRQ all-clear + clear DCHG on read`):

- We initialized HIRQ to `0xFFFF`; MAME inits `hirqreg = 0` — every flag
  (incl. CMOK) clear at power-on, set only by events.
- Our HIRQ read *re-asserted* DCHG from `disk_changed` (a Yabause guess);
  MAME's `hirq_r` *always clears* DCHG and clears BFUL/CSCT from buffer
  state. Matched that.

That advanced the first divergence to BIOS **`0x00003398`**, with matched
instructions jumping **8.78M → 15.84M** (nearly 2×, through all of CD
init). The new divergence is a different class: ours takes a **VBlank-IN
interrupt** (→ handler `0x06000840`) one instruction before MAME does — a
timing *phase* difference, not a control-flow bug.

**Raster timing corrected + cycle-exact VBlank** (see `fix(saturn): correct
NTSC frame length + cycle-exact VBlank-IN`). Two causes of the VBlank phase
drift:

- `CYCLES_PER_FRAME` was `476_932`; MAME's screen `set_raw` + clock ratios
  (SH-2 `MASTER_CLOCK_352/2`, dot clock `MASTER_CLOCK_320/8` → `64/15`
  cycles/dot) give `427 × 263 × 64/15 = 479_151`. The ~2200-cycle/frame
  shortfall drifted our VBlank earlier each frame. Corrected; VBlank-IN edge
  now computed from the frame length (no per-line rounding).
- `run_for` clamps each batch to the next VBlank-IN edge, raising the
  interrupt within one instruction of the exact raster cycle.

The first divergence advanced **10.2M → 15.63M** instructions (drain=1), and
drain=1/drain=256 now agree (~frame 21). **The residual is the
cycle-accuracy frontier:** still a VBlank landing one instruction off, but
now from instruction-level **cycle-count differences vs MAME** accumulating
over ~21 frames — not raster drift. Closing it would mean matching MAME's
per-instruction SH-2 cycle costs exactly (memory wait states, pipeline
interlocks), which is deep and of uncertain value (MAME's cycle model is
not itself ground truth). Diminishing returns from MAME PC-diffing here.

**`run_frame` park — full-system trace built; it's the same phase wall.**
A full-system tracer now records the master PC in *scheduler* order (master
+ slave + CD-block interleaved — the real `run_frame` path), via
`Scheduler::run_for_traced` + `Saturn::run_for_traced` +
`gen_fullsystem_pc_trace`. Diffing it against MAME: the full-system path
tracks MAME for **~10M instructions (frame ~39)**, then hits the **same
VBlank interrupt-phase divergence** (ours → handler `0x06000840`, MAME
continues) as the master-only path. So `run_frame`'s park (`~0x060108C2`) is
**not** a discrete control-flow bug — it's the accumulated interrupt-phase
error from cycle-count drift vs MAME, the same cycle-accuracy frontier.

PC-trace diffing has reached its limit: it can't align across an async
interrupt landing on a different instruction (a larger resync window to skip
handler excursions is O(window) per mismatch and too slow). **Next options,
both substantial:** (a) an interrupt-aware resync that skips matched handler
excursions on each side to confirm whether *any* real control-flow
divergence hides past the phase noise; or (b) SH-2 per-instruction
cycle-cost / wait-state fidelity work to close the drift — of uncertain
value, since MAME's cycle model is not ground truth either. A pragmatic
alternative is to stop chasing exact MAME-match and instead check whether
the boot *recovers* and renders the splash despite phase differences.

**3-way back-review (ours ↔ Yabause ↔ MAME).** With MAME the primary
reference, the originally-Yabause-derived code was cross-reviewed and the
divergences aligned to MAME (`fix(saturn): align CD-block + SMPC INTBACK to
MAME`): no-disc CD report returns zero geometry (was FAD-150 disc-present);
Get TOC sets the TRANS status bit; Get Session Info returns `0x0100/0`
(was `0xFFFF/0xFFFF`); Get HW Info no longer touches disc-changed; INTBACK
SF-busy is request-derived (~16 µs status / +700 µs peripheral, was a fixed
250 µs); INTBACK `OREG10 = 0x34`. Verified consistent (no change needed):
SMPC command codes, region code `OREG9 = 0x04`, HIRQ init/read, signature
persistence, status-byte values. The full-system resync advanced
9.98M → 10.58M with these.

The one deferred structural item is now **done** (`feat(saturn): staged
INTBACK peripheral protocol`): INTBACK uses MAME's staged sequence — a
status phase (RTC/region/system-status/SMEM/`OREG31=0x10`, `SR = 0x40 |
(stage<<5)`) followed by host-driven CONTINUE/BREAK on IREG0 and peripheral
phases (`OREG0/1 = 0xF0` no-controller, `SR = 0xC0|pmode` then `0x80|pmode`),
replacing the inline single-shot response. No boot regression (the BIOS path
is insensitive — divergence still the VBlank phase at 10.58M), but the
protocol is now correct for controller reads. The CD-block and SMPC
host-interface fidelity is now aligned to MAME across the board.

**Cycle-timing frontier — investigated; our SH-2 model is spec-correct, so
we do NOT chase exact MAME-match.** Instruction-rate analysis (gap between
VBlank-handler entries) shows MAME runs ~436K master instr/frame vs our
~297K, which initially looked like a systematic over-count. It isn't:
- Our `BF`/`BT` taken = 3 cycles / not-taken = 1 — exactly the SH7604
  manual value; MAME uses the same 3/1.
- The deterministic hot loops match MAME **exactly**: the `DT;BF` delay loop
  at `0x1D3C` = 3,600,000 iterations in both; the `0x2B0` routine = 523,584
  in both. Identical counts ⟹ our per-instruction cycle accounting agrees
  with MAME wherever the path is timing-independent. (MAME's own
  `BUSY_LOOP_HACKS` is disabled, so it isn't fast-forwarding either.)
- The only count divergence is in **variable poll loops** (e.g. the `0x42E8`
  routine, 98,517× ours vs 360,010× MAME) that spin until a polled
  peripheral/flag state flips. Their count depends on *when* that state
  changes — accumulated timing **phase**, not a per-instruction cost we got
  wrong.

Reference emulators are guides, not ground truth: matching MAME's exact
poll-iteration count would over-fit to MAME's timing approximations and make
us *less* faithful to the SH-2 spec, not more. The deterministic cycle model
is correct and stays as-is. Closing the splash from here is a question of
finding any genuine *spec* deviation (peripheral behavior / interrupt
delivery vs the hardware manuals), not of bending cycle costs to mirror MAME.

**Reference-magic audit (`REVIEW(magic)`).** Values that were tuned to a
reference emulator rather than a hardware datasheet are tagged inline with
`REVIEW(magic)` — `grep -rn "REVIEW(magic)" crates` enumerates them, each
with a one-line grounding note. Currently: INTBACK SF-busy timings
(MAME's 8/8/700 µs, 700 a MAME guess), `CYCLES_PER_FRAME` dot/line counts
(MAME `set_raw`; 59.76 vs nominal 59.94 Hz), the HBLANK "last ~20% of line"
approximation, the placeholder RTC bytes, INTBACK `OREG10 = 0x34`, and the
CD Get-HW-Info `CR2/CR4` literals. None gate boot today; revisit a tag only
if a divergence implicates it. Two former magic values were defects and
were fixed (not just tagged): the CD `PERIODIC_CYCLES` stale-duplicate of
the old frame length, and the rounded `CYCLES_PER_US`. Spec-grounded values
(SCU vectors/priorities, SH-2 cycle costs, region code) are deliberately
*not* tagged.

### Verification gates

1. `cargo test --workspace` — all 288 tests green.
2. `cargo test -p saturn --test smpc` — INTBACK populates OREG with a no-controller / North-America-region response and raises the SMPC interrupt; SF clears only after the execution delay.
3. `cargo test -p saturn --test bios_boot` — hash matches the splash golden (currently still the all-black baseline).
4. **Manual M4 exit criterion**: `cargo run -p fifth_planet -- BIOS.bin` shows the SEGA logo. The test suite can't confirm "looks right" — visual confirmation is the gate.

### Explicitly out of scope for M4

- VDP1 sprite/polygon engine (registers stubbed in M4; rendering is M5)
- SCSP + MC68EC000 + audio — M5
- Keyboard input + full SMPC peripheral protocol — M5
- CD-ROM image loading + real SH-1 / CD-block firmware — M6
- Save states — M5+ once the peripheral set stabilises

## Later milestones (queued)

- **M5** — VDP1 sprite/polygon engine, SCSP + M68k + SDL2 audio, SDL2 keyboard mapping via SMPC peripheral data, save states.
- **M6** — CD-block (SH-1), CD-ROM image loading, first commercial game booting.
- **Explicitly never** — JIT / dynarec (accuracy over performance is the project's design axis).
