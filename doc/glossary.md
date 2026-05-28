# SEGA Saturn architecture glossary

Reference for terms, chip names, and acronyms that appear in the
codebase, in the SH-2 / SH7604 / Saturn manuals, and in commit
messages. Linked entries refer to other terms in this file.

Anything Saturn-specific is documented under SEGA's own naming first
(e.g. `VDP2`, `SCU`, `INTBACK`); generic terms (cache line, write-
through, NOP) only get an entry if they have a project-specific shade
of meaning.

---

## A

**A-Bus** — One of the Saturn's three external buses (A, B, C). A-Bus
carries cartridge / CD-block traffic. Saturn-side address range
`0x0500_0000..0x05FF_FFFF` is shared with the B-bus; [VDP1], [VDP2],
the [CD-block], and the [SCU] now carve out their own sub-windows from
it (dispatched ahead of the remaining `StubRegisterBank` catch-all),
with [SCSP] still to come. The SH-2 BSC's [ASR0]/[ASR1] registers
configure A-bus wait states.

**ASR0 / ASR1** — A-bus Set registers in the [SCU]. Configure wait
states and bus width for the A-bus chip-select windows. Stored
verbatim in M3; SH-2 BSC integration is queued for later.

---

## B

**B-Bus** — The other external bus alongside [A-Bus]. Carries
[VDP1] / [VDP2] / [SCSP] traffic in addresses `0x0500_0000+`. Same
M3 stub as A-bus.

**Backup RAM** — 32 KiB battery-backed save memory at
`0x0018_0000..0x001F_FFFF`. Stored in `crates/saturn/src/memory.rs`
as `Ram`; mirrors across its 512 KiB window.

**BGON** — VDP2 Screen Display Enable register (`0x05F8_0020`). Bits
0..5 enable [NBG]0–3 / [RBG]0–1; bits 8..12 ([TPON]) make each layer's
palette code 0 a solid colour instead of transparent.

**BIOS** — Saturn boot ROM, 512 KiB at `0x0000_0000..0x000F_FFFF`
(mirrored to `0x0010_0000`). Copyrighted by SEGA — see `bios/README.md`
for the don't-commit policy.

**BSC** — Bus State Controller. SH7604 on-chip peripheral at
`0xFFFFFF40+` that configures wait states for each external chip
select. Stub in M3 (`sh2::onchip::bsc::Bsc`); real wait-state math
deferred until a target game shows it matters.

---

## C

**Cache** — SH7604 on-chip 4 KiB unified data/instruction cache.
4-way set-associative, 16-byte lines, 64 sets, LRU replacement.
Controlled by CCR (cache control register, 8-bit). Cache lines store
both tag and data; on a miss the SH-2 fetches 4 × 32-bit words from
the bus to fill the line. Writes are write-through with optional
in-place update of resident lines. See `crates/sh2/src/cache.rs`.

**Cache-through** — Address aliasing on SH-2: `0x0xxx_xxxx` is the
cached form, `0x2xxx_xxxx` is the cache-through alias of the same
physical memory. The CPU's `classify(addr)` function strips the top
3 bits before handing the address to the bus, so the bus never has
to disambiguate cached vs. cache-through.

**CCR** — Cache Control Register, 8-bit at `0xFFFFFE92`. Bits: CE
(enable), ID (instruction disable), OD (data disable), TW (two-way
mode override), CP (purge — write-only).

**CD-block** — Saturn's CD-ROM controller subsystem, built around an
[SH-1] running internal firmware. Handles disc reading, sub-Q, error
correction, and audio CD playback. M4 adds an **address-space presence
stub** at `0x0589_8000` (`crates/saturn/src/cd_block.rs`): defined HIRQ
/ CR register reads ("drive present, no disc, ready") so BIOS init
doesn't hang on open bus. The real SH-1 + firmware is M6.

**Chunky** vs. **planar** — Pixel-storage modes. VDP1 / VDP2 use
chunky (one pixel = N consecutive bits in memory); some legacy modes
use planar (each bit-plane stored separately). M3's VDP2 renderer
only handles chunky.

**CHCR** — Channel Control Register. Per-DMA-channel register in the
[SCU]. Bits 0–2 select start factor; bit 8 (DGO) is the manual-fire
trigger.

**Coscheduling** — Running the master and slave [SH-2]s in lock-step
with each other and the rest of the chip set. Implemented via the
[Scheduler]'s "smallest deadline wins" rule.

**COMREG** — SMPC Command Register at `0x0010_001F`. Software writes
a command byte; SMPC queues it for the Saturn aggregate to process
between scheduler batches.

**CRAM** — Color RAM. VDP2's 4 KiB palette memory at
`0x05F0_0000..0x05F0_0FFF`. 1024 entries, 16 bits each (RGB555).
Modeled in `crates/saturn/src/vdp2/cram.rs`.

**CRAOFA / CRAOFB** — Colour-RAM Address Offset registers (VDP2,
`0x05F8_00E4` / `0x05F8_00E6`). 3 bits per layer; the value `<< 8` is
the high bits of that layer's [CRAM] index, selecting one of eight
256-entry banks. A paletted dot's colour is `CRAM[(NxCAOS << 8) | dot]`.
The BIOS splash puts NBG3's silver palette in bank 3 (`CRAM 0x300+`),
so ignoring this draws the wrong (dark) bank — see the renderer's
`nbg_color_ram_offset` / `rbg_color_ram_offset`.

**Cycle-stealing** — DMA mode where the controller takes single bus
cycles between SH-2 accesses rather than holding the bus for the
entire transfer. M3 models DMA as synchronous block-transfer only;
cycle-stealing accuracy is a later refinement.

---

## D

**Delay slot** — On SH-2, the instruction immediately following a
delayed branch (`BRA`, `BSR`, `BRAF`, `BSRF`, `JMP`, `JSR`, `RTS`,
`RTE`, `BT/S`, `BF/S`) executes *before* the branch is taken. Certain
ops (other branches, SR-modifying ops, PC-relative loads) are illegal
in a slot and raise [vector 6](#vectors). Tracked in
`Cpu::pending_branch`; see `interpreter::Cpu::step`.

**DGO** — "DMA Go" bit (bit 8 of `D*EN` in the [SCU]). Setting it
with a non-zero transfer count triggers a manual-mode DMA. Must be
written as part of a 32-bit `D*EN` store — byte/halfword writes
deliberately don't fire (software builds the register up piece-by-
piece and we'd otherwise trigger mid-construction).

**DIV0S / DIV0U / DIV1** — SH-2 hardware-divide instructions
implementing non-restoring division across 32 cycles of `DIV1`
iterations after `DIV0S` (signed) or `DIV0U` (unsigned). See the
single-step trace in `crates/sh2/tests/opcodes_arith.rs`.

**DIVU** — On-chip 32×32 / 64×32 signed divider in SH7604 at
`0xFFFFFF00+`. Writing DVDNT (or DVDNTL for the 64-bit form) triggers
the divide; result lands in DVDNTL (quotient) + DVDNTH (remainder).
DVCR.OVF set on /0, `INT_MIN / -1`, or 64-bit overflow. See
`crates/sh2/src/onchip/divu.rs`.

**DMA** — Direct Memory Access. Saturn has DMA in the [SH-2] on-chip
DMAC (2 channels) and in the [SCU] (3 channels). The SCU's three
channels are the heavy-lift transfers; on-chip DMAC is for SH-2-
internal motion. SCU DMA is the M3 task #2 deliverable.

**DMAC** — Direct Memory Access Controller. SH7604 on-chip at
`0xFFFFFFA0+`. Two channels. Distinct from the [SCU] DMAC. Register
storage only in M3; full implementation deferred.

**Dual SH-2** — The Saturn has two SH-2 SH7604s on one shared bus.
The master runs from power-on; the slave is held in reset until the
[SMPC] `SSHON` command releases it. Both have their own caches and
on-chip peripherals.

**DVCR** — Divide Control Register in the [DIVU]. Bit 0 = OVF
(overflow occurred), bit 1 = OVFIE (interrupt enable).

---

## E

**EXTEN** — VDP2 EXTernal ENable register. Out-of-scope minutia for M3.

---

## F

**Framebuffer** — VDP1 has a 256 KiB dual framebuffer at
`0x05C8_0000`: the plotter draws into the *draw* buffer while VDP2
composites the *display* buffer, swapped at the frame boundary (see
[PTMR]). VDP2 composites its NBG/RBG layers + the VDP1 framebuffer per
pixel into the final RGBA8888 output.

**FNV-1a** — Hash function used by `harness::state_digest` to
fingerprint CPU + memory state in the ROM regression harness.
64-bit, deterministic, stable across platforms.

**FRT** — Free-Running Timer. SH7604 on-chip 16-bit counter at
`0xFFFFFE10+`. Many Saturn games use it for fine-grained timing.
Implemented in `crates/sh2/src/onchip/frt.rs`.

---

## H

**HBlank** — Horizontal blanking interval. VDP fires an interrupt at
each line transition; the [SCU] aggregates it into the SH-2 INTC.
Reflected in [TVSTAT] bit 2 by `Saturn::update_video_timing`.

**HCNT** — VDP2 horizontal-dot counter, read-only at `0x05F8_0008`.
A free-running raster position the BIOS can poll. Derived from the
global cycle alongside [VCNT] / [TVSTAT].

**High Work RAM** — 1 MiB at `0x0600_0000..0x06FF_FFFF`. Faster than
[Low Work RAM] (1-cycle vs 3-cycle wait states). Saturn programs
typically run code from here.

---

## I

**IMS / IST** — SCU Interrupt Mask / Status registers. Each bit is a
specific source; IMS=1 suppresses, IST records pending (write-1-to-
clear). Wired into the SH-2 master INTC via `Saturn::drain_scu_intc`,
which forwards the highest-priority unmasked source whose level exceeds
the master's `SR.imask`.

**INTBACK** — SMPC command **0x10**. Returns SMPC status + peripheral
data (region/area code, RTC, controllers) in [OREG]. M4 returns a
"no controller connected, North-America region" status response. The
command is **not instantaneous**: the SMPC holds [SF] busy for its
execution time (~250 µs ≈ 7150 SH-2 cycles) before filling OREG and
clearing SF — the BIOS polls SF in a wait loop, and clearing it too
early derails the boot. Keyboard/full peripheral protocol is M5+.

**INTC** — Interrupt Controller. Two layers: one on-chip per SH-2
(`crates/sh2/src/onchip/intc.rs`) handles internal sources (DIVU
overflow, DMAC end, FRT, NMI). The [SCU]'s INTC aggregates external
sources (VBlank, HBlank, SCSP, etc.) into a single line into the
master SH-2's on-chip INTC.

**IPRA / IPRB** — Interrupt Priority Registers in the SH-2 INTC.
4-bit priority nibbles per source. Higher number = higher priority;
sources with priority ≤ SR.imask are masked.

**IREG** — SMPC Input REGister(s). Software writes command arguments
to IREG0..IREG6 before writing [COMREG].

**IRL** — External-line interrupt input. Saturn IRL1..IRL15 are
level-triggered; the SH-2 auto-vectors via VBR + (64 + level) × 4.

---

## L

**Load-use stall** — SH-2 pipeline interlock: a register loaded from
memory isn't visible to the *immediately following* instruction. The
consumer stalls 1 cycle. Tracked via `Cpu::load_dest_pending` in the
interpreter; see `crates/sh2/tests/pipeline_timing.rs`.

**Low Work RAM** — 1 MiB at `0x0020_0000..0x002F_FFFF`. Slower than
[High Work RAM] (3-cycle vs 1-cycle wait states). Some BIOS code
lives here; games tend to copy hot code into high WRAM.

---

## M

**MAC** — Multiply-and-Accumulate. SH-2's `MAC.W` / `MAC.L`
instructions read two operands from memory (each via post-increment),
multiply, and add to `MACH:MACL`. S-bit in SR enables saturation
(48-bit for L, 32-bit for W). Workhorse for inner loops on DSP-style
code.

**MACH / MACL** — Multiply Accumulator High / Low. Two 32-bit
SH-2 system registers that hold the 64-bit accumulator for [MAC] and
the result of 32×32 multiplies (`DMULS/U.L`).

**Master** — The primary [SH-2]. Boots from the BIOS reset vector;
runs the bulk of game code. Distinguished from [Slave].

**MOV.L @(d, PC)** — SH-2 PC-relative load. Address is computed as
`(PC_of_instr + 4 + disp × 4) & ~3`. The "+4" base is critical and
trips up implementers — see the doc comment in
`interpreter::Cpu::execute` for the canonical handling.

---

## N

**NBG0..NBG3** — Normal Background layers in VDP2. Four flat
backgrounds composited with the [RBG] layers and the [VDP1]
framebuffer. M3's renderer handles only NBG0.

**NMI** — Non-Maskable Interrupt. SH-2 vector 11. Bypasses SR.imask
(modeled as level 16 in `sh2::onchip::intc`).

**NMIREQ** — SMPC command **0x18**. Asserts an [NMI] on the master
[SH-2] (routed via the on-chip INTC). The BIOS uses an `imask=15`
busy-wait that only an NMI can break.

**NOP** — `0x0009` on SH-2. Useful as filler in test programs because
its encoding is fixed and it takes exactly 1 cycle.

---

## O

**On-chip peripherals** — SH7604 builds INTC, DMAC, DIVU, FRT, BSC,
WDT, SCI, UBC right into the CPU package. Mapped at
`0xFFFFFE00..0xFFFFFFFF`. The CPU intercepts accesses to that range
before they reach the external bus (`Cpu::mem_*` checks `OnChip::owns`).

**OREG** — SMPC Output REGister(s). SMPC writes command responses to
OREG0..OREG31 for software to read after polling SF.

---

## P

**Pipeline (SH7604)** — 5-stage in-order: IF (Instruction Fetch),
ID (Decode), EX (Execute), MA (Memory Access), WB (Write Back). Most
ALU ops are 1 cycle; loads have a 1-cycle delay before the loaded
register is usable. M1 models the cycle-relevant interlocks via a
scoreboard in `sh2::pipeline::Pipeline`.

**PR** — Procedure Register. SH-2 holds the return address for
`BSR` / `BSRF` / `JSR` here. `RTS` jumps to PR (with a delay slot).

**PTMR / PTM** — [VDP1] Plot Trigger register (`0x05D0_0004`); PTM =
bits 1:0. `0b00` idle, `0b01` "draw by request" (plot once on this
write), `0b10` "automatic" (re-render the command list every frame at
the [framebuffer] swap). The BIOS splash uses automatic mode — drawing
only on the register write left one buffer empty and strobed the logo.

---

## R

**RBG0 / RBG1** — Rotation Background layers in VDP2. Like [NBG]s but
mapped through a rotation parameter table (affine transform) before
sampling. Fully rendered (M5): bitmap or 4×4-plane tile field, per-line
coefficient table, and screen-over modes — `crates/saturn/src/vdp2/
rotation.rs` + `renderer.rs`.

**RESENAB / RESDISA** — SMPC commands **0x19 / 0x1A**. Enable /
disable the reset button (which, when enabled, makes the SMPC NMI the
master on press). No-ops for us beyond dropping [SF]. The USA BIOS
issues RESDISA early in boot.

**ROM** — Read-Only Memory. The Saturn BIOS, cartridge contents, and
CD-block firmware are all ROMs in our model.

**RTE** — Return from Exception. SH-2 instruction that pops PC then
SR from the stack (via R15) and jumps to the popped PC. Has a delay
slot. Used by the BIOS to return from `TRAPA` / interrupts.

**RTS** — Return from Subroutine. Pops PC from PR. Delay slot.

---

## S

**SaturnBus** — `crates/saturn/src/bus.rs` — the workspace's
implementation of `sh2::Bus`. Owns all Saturn memory regions and
peripherals, dispatches by address.

**Scheduler** — `crates/saturn/src/scheduler.rs` — event-driven
runner. Each `SchedEntity` reports a `next_deadline()`; the scheduler
always advances the entity with the smallest deadline. Determinism
contract: ties resolve to insertion order.

**SCI** — Serial Communication Interface. SH7604 on-chip. Saturn
uses it minimally. Stub-only in `crates/sh2/src/onchip/sci.rs`.

**SCSP** — Saturn Custom Sound Processor. Audio chip with built-in
MC68EC000 plus a vector DSP. Out of scope until M4.

**SCU** — System Control Unit. Saturn's bus bridge between the SH-2s
and everything not on the SH-2 bus. Holds 3 DMA channels, an
interrupt aggregator, timers, and the [SCU-DSP]. M3 task #2 lands the
DMA half; tasks #3 + #4 land the rest.

**SCU-DSP** — 32-bit DSP embedded in the SCU. Own ISA, own microcode
RAM (256 × 32-bit), four banks of 64 × 32-bit data RAM. Used for
matrix math in 3D games and some BIOS init paths. M3 task #4
delivers a standalone `scu_dsp` crate parallel to `sh2`.

**SETSL / SSHON** — SMPC command 0x02. Releases the [slave] SH-2
from its power-on halt. Tracked by `Sh2Entity::halted` in the
scheduler.

**SETSM / SSHOFF** — SMPC command 0x03. Halts the slave.

**SETTIME** — SMPC command **0x16**. Initialises the SMPC's clock state.
Accepted as a no-op that drops SF; full clock-state support is M5+.

**SETSMEM** — SMPC command 0x17. Stores backup-memory bytes. No-op.

**SF** — SMPC Status Flag at `0x0010_0063`. Goes to 1 when COMREG is
written and queues a command; drops to 0 once the command is
processed. Polling SF is how software waits for SMPC to finish.

**SH-1** — Hitachi SH-1. The CPU in the Saturn's CD-block. Different
ISA from SH-2 (no MAC.L, no division unit, simpler pipeline). M5.

**SH-2** — Hitachi SH-2 SH7604. The Saturn's main CPUs (×2). 32-bit
RISC at 28.6 MHz. Full ISA in `crates/sh2/src/isa.rs`.

**Slave** — The secondary [SH-2]. Held in reset at power-on; released
by SMPC [SSHON]. Shares the bus with the [master].

**SMPC** — System Manager + Peripheral Control. Power management,
sub-CPU control, peripheral I/O. Lives at `0x0010_0000+`. See
`crates/saturn/src/smpc.rs`.

**SR** — Status Register. SH-2 system register with T (true bit, set
by CMP / used by BT/BF), S (saturation), I[3:0] (interrupt mask),
Q and M (division state). Bits outside the documented set are masked
on write by `Sr::WRITE_MASK`.

**SSHON / SSHOFF** — see [SETSL] / [SETSM].

**Stall** — Cycle(s) the CPU loses waiting for something. Three flavors
in M2+: bus stalls (from `Bus::read*/write*` returning a non-zero
count), interlock stalls (load-use + MAC-read), and DMA cycle-stealing
(not modeled in M3).

---

## T

**T-bit** — Bit 0 of [SR]. Set by `CMP/*` instructions and consumed
by `BT` / `BF` / `BT/S` / `BF/S`.

**TCR (DMA)** — Transfer Count Register. Per-DMA-channel byte count.
Channel 0 carries 20 bits; channels 1+2 carry 12.

**TPON (NxTPON / R0TPON)** — "Transparent-pen as solid" bits in VDP2
[BGON] (bits 8..12, one per background layer). When set, palette **code
0 is drawn as the opaque colour** `CRAM[offset]` instead of being
treated as transparent. The BIOS splash sets it on NBG3 so the metal's
code-0 dots fill with silver rather than showing the backdrop through
them — see the renderer's `nbg_transparent_pen_solid`.

**TVMD** — VDP2 TV Mode register. Selects resolution, interlace,
border colour. Master switch — bit 15 enables video output entirely.

**TVSTAT** — VDP2 TV Status register, read-only at `0x05F8_0004`.
Bit 3 = VBLANK, bit 2 = HBLANK, bit 1 = ODD (field parity), bit 9 =
PAL. Maintained live by `Saturn::update_video_timing` from the global
cycle; the BIOS reads it after [INTBACK] to track the raster.

**TRAPA** — SH-2 software trap. `TRAPA #imm` pushes SR + PC, vectors
through `VBR + imm × 4`. The first SMPC interactions in BIOS init are
TRAPA calls into BIOS handler tables.

---

## V

**VBlank-in / VBlank-out** — Vertical blanking interval interrupts.
`Saturn::update_video_timing` raises VBlank-IN on the active→VBLANK
scanline edge ([VCNT] crossing line 224); the [SCU] INTC aggregates it
into the master SH-2's INTC.

**VBR** — Vector Base Register. SH-2 exception/interrupt table base.
Exception N's handler address is loaded from `VBR + N × 4`.

**VCNT** — VDP2 vertical-line (scanline) counter, read-only at
`0x05F8_000A`. 0..262 in a 263-line NTSC frame; the BIOS polls it to
synchronize with the raster. `Saturn::update_video_timing` derives it
live from the global cycle (≈1813 cycles per line) so it isn't a
frozen stub.

**VDP1** — Video Display Processor 1. Draws sprites and polygons into a
256 KiB dual [framebuffer], composited with VDP2's layers by VDP2's
compositor. `crates/saturn/src/vdp1/` is a **full plotter** (M5):
VRAM (512 KiB at `0x05C0_0000`), the dual frame buffer (`0x05C8_0000`),
11 registers at `0x05D0_0000`, and a command-list rasteriser
(`plotter.rs`) for textured / scaled / distorted sprites, polygons and
lines with gouraud shading and the colour-calc modes. Draw is kicked by
[PTMR]; draw-end latches `EDSR.CEF` and raises the SCU sprite-draw-end
interrupt.

**VDP2** — Video Display Processor 2. Background generator with 4
[NBG] + 2 [RBG] layers, [VDP1] sprite-layer compositing, and the final
video output. Owns VRAM (512 KiB at `0x05E0_0000`) and [CRAM] (4 KiB at
`0x05F0_0000`). The renderer (`crates/saturn/src/vdp2/renderer.rs`) is a
full multi-layer compositor: NBG0–3 (tile/bitmap, 4/8bpp + RGB), RBG0/1
rotation, priority + colour calculation, windows, per-line scroll/zoom,
[CRAOFA]-banked palettes, and [TPON] handling.

**Vectors** — SH-2 exception/interrupt vector numbers. Loaded from
`VBR + N × 4`. Notable ones:
- 0/1: power-on reset PC / SP
- 2/3: manual reset PC / SP
- 4: general illegal instruction
- 6: slot-illegal instruction
- 9: CPU address error
- 11: NMI
- 12: user break
- 32..63: TRAPA #(N - 32)
- 64..255: external interrupts (auto-vector)

**VRAM** — Video RAM. VDP2 has 512 KiB at `0x05E0_0000`, split into
4 banks (A0, A1, B0, B1) for cycle-pattern parallel access.

---

## W

**WDT** — Watchdog Timer. SH7604 on-chip at `0xFFFFFE80+`. Stub in
`crates/sh2/src/onchip/wdt.rs`.

**Work RAM** — See [Low Work RAM] and [High Work RAM].

**Write-through** — Cache policy where every write reaches the bus,
plus the cached line (if resident) gets updated in place. What
SH7604 does, and what our `cache::Cache::write_through_*` models.
