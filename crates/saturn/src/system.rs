//! Top-level Saturn aggregate: bus + scheduler + master/slave SH-2.
//!
//! This is the surface the frontend will hold in M3+. For M2 it stays
//! deliberately thin — `new`/`reset`/`run_for` plus typed accessors for
//! each CPU. Anything chip-specific (VDP2, SCSP, SMPC commands) gets
//! added as a method on `Saturn` when the corresponding peripheral
//! lands, so the frontend doesn't have to reach across module boundaries.

use sh2::Cpu;
use sh2::bus::{AccessKind, Bus};

use crate::SaturnBus;
use crate::scheduler::{CdBlockEntity, EntityId, SaturnEntity, SchedEntity, Scheduler, Sh2Entity};
use crate::smpc::Command as SmpcCommand;

/// Max scheduler cycles between SMPC-pending checks. Small enough that
/// BIOS code polling SF after a command doesn't spin for a meaningful
/// fraction of a frame; large enough that the inner-loop overhead of
/// poking SMPC every tick isn't paid 28 million times per second.
const SMPC_POLL_QUANTUM: u64 = 256;

/// SH-2 master clock (NTSC): 14.318181 MHz crystal × 4 / 2 ≈ 28.6364 MHz.
const SH2_CLOCK_HZ: u64 = 28_636_360;

/// Convert microseconds to SH-2 master cycles at the master clock. (Was a
/// rounded `CYCLES_PER_US = 28`, ~2.2% short; this keeps full precision.)
fn us_to_cycles(us: u64) -> u64 {
    us * SH2_CLOCK_HZ / 1_000_000
}

/// INTBACK SF-busy time in microseconds, computed from the request.
///
/// Reconciled against Mednafen (the LLE reference), which models INTBACK as a
/// 4 MHz SMPC-clock state machine (`mednaref/src/ss/smpc.cpp` `SMPC_Update` /
/// `SMPC_EAT_CLOCKS`): every command eats **92** SMPC clocks of dispatch
/// overhead, the **status** phase (taken when `IREG0 & 0xF`) eats **+952**, and
/// the **peripheral** phase (`IREG1 & 8`) runs a data-dependent per-port /
/// per-byte scan (`JR_EAT`). Converting SMPC clocks → µs (÷4 at 4 MHz):
/// dispatch 92 → 23 µs, status +952 → +238 µs (so a status INTBACK is ~261 µs).
/// The old values (8 / +8 / +700, MAME-derived) made status ~16 µs — ~16× too
/// short, which cleared SF far earlier than the reference and skewed the boot
/// trace. The peripheral phase is still a lump approximation (the full JR scan
/// is unmodeled); replace it with the per-peripheral scan when peripheral
/// INTBACK fidelity is needed.
fn intback_busy_us(ireg0: u8, ireg1: u8) -> u64 {
    let mut clocks = 92; // common command dispatch
    if ireg0 & 0x0F != 0 {
        clocks += 952; // status phase
    }
    let mut us = clocks / 4; // 4 MHz SMPC clock → µs
    if ireg1 & 0x08 != 0 {
        us += 700; // peripheral phase (approximation; see above)
    }
    us
}

/// NTSC raster timing in SH-2 master-clock cycles, derived to match the
/// reference (MAME): the SH-2 runs at `MASTER_CLOCK_352/2` and the 320-mode
/// dot clock at `MASTER_CLOCK_320/8`, with `MASTER_CLOCK_352 = 14.318181 MHz
/// × 4` and `MASTER_CLOCK_320 = × 3.75`. That makes SH-2 cycles per dot =
/// `4 × 352/320 = 64/15`, and a 427-dot × 263-line frame =
/// `427 × 263 × 64/15 ≈ 479_151` cycles (≈59.76 Hz). The BIOS polls VDP2
/// `VCNT`/`TVSTAT` and takes VBlank-IN off this raster, so an inaccurate
/// frame length drifts our interrupts against the reference's by ~2200
/// cycles/frame — visibly diverging the boot trace after ~20 frames.
/// (Was 476_932; corrected against MAME's screen `set_raw`.)
///
/// REVIEW(magic): the 427×263 dot/line counts come from MAME's `set_raw`,
/// not directly from a Saturn datasheet, and the implied 59.76 Hz differs
/// from the nominal NTSC 59.94 Hz. The crystal-derived 64/15 cycles/dot is
/// solid; the htotal/vtotal are the part to verify against VDP2 docs.
const CYCLES_PER_FRAME: u64 = 479_151;
/// Master-clock cycles per second, for advancing the SMPC RTC (1 Hz). NTSC
/// runs ≈59.76 frames/s; the small rounding here is far below RTC resolution.
const CYCLES_PER_SECOND: u64 = (CYCLES_PER_FRAME * 5976) / 100;
const LINES_PER_FRAME: u64 = 263;
const ACTIVE_LINES: u64 = 224;
/// Cycles per scanline ≈ 1822. Used only for sub-frame granularity (CD
/// tick, HBLANK approximation); precise raster edges are computed from
/// `CYCLES_PER_FRAME` directly to avoid per-line integer-rounding drift.
const CYCLES_PER_LINE: u64 = CYCLES_PER_FRAME / LINES_PER_FRAME;

/// TVSTAT HBLANK (bit 2): asserted once the dot counter passes the active
/// display width, per Mednafen's `HTimings` (`vdp2.cpp`): the 320-family line is
/// 427 dots with 320 active (HBLANK ≈ last 25 %), the 352-family line is 455
/// with 352 active (HBLANK ≈ last 22.6 %). The HRESO LSB picks the family
/// (320/640 → 0, 352/704 → 1); the ×2 hi-res modes share the fraction. Pure +
/// testable: `line_cycle / cycles_per_line ≥ active / total`.
fn hblank_active(line_cycle: u64, h_res: u8, cycles_per_line: u64) -> bool {
    let (active, total) = if h_res & 1 == 1 {
        (352u64, 455u64)
    } else {
        (320u64, 427u64)
    };
    line_cycle * total >= active * cycles_per_line
}

/// Sub-frame granularity at which the CD-block periodic-firmware timer
/// ticks. One scanline matches the reference (Yabause drives `Cs2Exec`
/// per scanline); the CD-block's own accumulator carries the remainder, so
/// this sets the *phase resolution* of the periodic report, not its cadence.
const CD_TICK_CYCLES: u64 = CYCLES_PER_LINE;

// ---- SCU DMA execution (operates on `&mut SaturnBus`) ---------------------
//
// These were methods on `Saturn` (borrowing all of `self`); moving them onto
// the bus lets them run both at the batch boundary (`run_for`) *and* from
// inside the per-instruction `step_cpus` loop — the foundation for executing a
// DMA at its trigger cycle and for the faithful cycle-stealing cost (M13 A1/A3).
// Behaviour is unchanged: still a synchronous block transfer at the drain point.

/// Run every DMA the SCU queued. Direct or indirect (table-driven) mode; a
/// transfer sourced from the BIOS A-bus is illegal and moves nothing. Returns
/// the **cycle-stealing cost** — the sum of the per-access bus wait-states the
/// transfer incurs (the same `waits_for` the CPU pays for those addresses), so
/// the caller can stall the triggering CPU for the transfer's duration instead
/// of treating the DMA as free (M13 A3).
fn drain_dma(bus: &mut SaturnBus) -> u64 {
    let mut cost = 0u64;
    while let Some(req) = bus.scu.take_pending_dma() {
        if std::env::var("DMALOG").is_ok() {
            eprintln!(
                "DMA ch{} src={:08X} dst={:08X} bytes={:X} indirect={}",
                req.channel, req.src, req.dst, req.bytes, req.indirect
            );
        }
        let bios_src = |a: u32| a & 0x07F0_0000 == 0;
        let (final_src, final_dst) = if req.indirect {
            // Indirect: `dst` points at {size, dst, src} longword triplets; the
            // last has bit 31 of its source word set. Each triplet is a transfer.
            let mut index = req.dst;
            const MAX_INDIRECT_TRIPLETS: u32 = 0x1_0000;
            let mut walked = 0u32;
            loop {
                let (size, s0) = bus.read32(index, AccessKind::Dma);
                let (idst, s1) = bus.read32(index.wrapping_add(4), AccessKind::Dma);
                let (isrc_raw, s2) = bus.read32(index.wrapping_add(8), AccessKind::Dma);
                cost += (s0 + s1 + s2) as u64;
                let last = isrc_raw & 0x8000_0000 != 0;
                let isrc = isrc_raw & 0x07FF_FFFF;
                let idst = idst & 0x07FF_FFFF;
                let count = dma_count(req.channel, size);
                if !bios_src(isrc) {
                    scu_transfer(bus, isrc, idst, count, req.src_add, req.dst_add, &mut cost);
                }
                index = index.wrapping_add(0xC);
                walked += 1;
                if last || walked >= MAX_INDIRECT_TRIPLETS {
                    break;
                }
            }
            (req.src, index) // indirect leaves the table index advanced
        } else if bios_src(req.src) {
            (req.src, req.dst)
        } else if scu_dma_illegal(req.src, req.dst) {
            // A same-bus or unmapped SCU DMA is illegal: the transfer does not
            // run and the DMA-illegal interrupt is raised (Mednafen
            // `StartDMATransfer` → `SCU_INT_DMA_ILL`). M13 D5.
            bus.scu.raise(crate::scu::Source::DmaIllegal);
            (req.src, req.dst)
        } else {
            let count = dma_count(req.channel, req.bytes);
            scu_transfer(
                bus,
                req.src,
                req.dst,
                count,
                req.src_add,
                req.dst_add,
                &mut cost,
            )
        };
        bus.scu.finish_dma(req.channel, final_src, final_dst);
    }
    cost
}

/// Which of the three SCU DMA buses an address belongs to, or `None` if it maps
/// to no DMA-reachable bus (Mednafen `AddressToBus`, `scu.inc`):
/// `0` A-bus (CS0–CS2: cartridge + CD, `0x0200_0000..=0x058F_FFFF`),
/// `1` B-bus (SCSP + VDP1/VDP2, `0x05A0_0000..=0x05FB_FFFF`),
/// `2` C-bus (High Work RAM, `0x0600_0000..`).
fn scu_dma_bus(addr: u32) -> Option<u8> {
    match addr & 0x07FF_FFFF {
        0x0200_0000..=0x058F_FFFF => Some(0),
        0x05A0_0000..=0x05FB_FFFF => Some(1),
        0x0600_0000..=0x07FF_FFFF => Some(2),
        _ => None,
    }
}

/// True iff an SCU DMA between `src` and `dst` is illegal because both endpoints
/// sit on the **same** DMA bus — the SCU cannot transfer within one bus, and no
/// game relies on it. Mednafen also marks an *unmapped* endpoint (notably Low
/// Work RAM) illegal, but we **permit** those: our bus model treats LWRAM as
/// ordinary RAM, and silently skipping a transfer a game depends on would
/// corrupt data — far worse than not raising a dormant interrupt. So only the
/// unambiguous same-mapped-bus case is enforced (Mednafen
/// `StartDMATransfer` `rb == wb`, minus the `== -1` arms). M13 D5.
fn scu_dma_illegal(src: u32, dst: u32) -> bool {
    matches!(
        (scu_dma_bus(src), scu_dma_bus(dst)),
        (Some(rb), Some(wb)) if rb == wb
    )
}

/// SCU DMA byte count: a programmed 0 means the channel's maximum (1 MiB for
/// level 0, 4 KiB for levels 1/2), per the SCU manual.
fn dma_count(channel: usize, programmed: u32) -> u32 {
    if programmed != 0 {
        programmed
    } else if channel == 0 {
        0x0010_0000
    } else {
        0x0000_1000
    }
}

/// One SCU DMA block transfer over the B-bus 16-bit data path, honouring the
/// `D*AD` strides. Source read as 32-bit words split into big-endian 16-bit
/// halves; the destination advances by `dst_add` (Work RAM H forces a 2-byte
/// step). Returns the post-transfer `(src, dst)`.
fn scu_transfer(
    bus: &mut SaturnBus,
    mut src: u32,
    mut dst: u32,
    bytes: u32,
    src_add: u32,
    dst_add: u32,
    cost: &mut u64,
) -> (u32, u32) {
    let mut src_shift = ((src & 2) >> 1) ^ 1;
    let mut i = 0u32;
    while i < bytes {
        let (word, sr) = bus.read32(src & 0x07FF_FFFC, AccessKind::Dma);
        let half = (word >> (src_shift * 16)) as u16;
        let sw = bus.write16(dst & 0x07FF_FFFE, half, AccessKind::Dma);
        *cost += (sr + sw) as u64;
        src_shift ^= 1;
        if src_shift != 0 {
            src = src.wrapping_add(src_add);
        }
        // Work RAM H (0x0600_0000) forces a fixed 2-byte destination step.
        let step = if dst & 0x0700_0000 == 0x0600_0000 {
            2
        } else {
            dst_add
        };
        dst = dst.wrapping_add(step);
        i += 2;
    }
    (src, dst)
}

/// One emulated SEGA Saturn — a Saturn-shaped memory map populated with
/// a caller-supplied BIOS image, plus master and slave SH-2 cores wired
/// into a shared event-driven scheduler.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Saturn {
    pub bus: SaturnBus,
    pub scheduler: Scheduler<SaturnEntity>,
    master_id: EntityId,
    slave_id: EntityId,
    cd_id: EntityId,
    /// Debug-only: per-instruction master-SH-2 PC + accumulated cycle stream, for
    /// a cost-per-instruction lockstep vs Mednafen's `SS_MASTER_PCSTREAM` (the
    /// master-side analog of the 68k `pcstream`). `#[serde(skip)]`.
    #[serde(skip)]
    master_pcstream: Option<Vec<(u32, u64)>>,
    /// Debug-only (M11 Doukyuusei menu): when the master's low-24 PC equals
    /// `target`, append `(R[reg], cycle)` — a cycle-stamped dispatch-index
    /// sequence for time-aligned diffing vs Mednafen's `SS_LOGSEQ`. `#[serde(skip)]`.
    #[serde(skip)]
    seqlog: Option<SeqLog>,
}

/// `(target_low24_pc, reg_index, records[(reg_value, cycle)])` — see [`Saturn::enable_seqlog`].
type SeqLog = (u32, usize, Vec<(u32, u64)>);

impl Saturn {
    /// Construct with a real BIOS image. Both CPUs start with default
    /// register state; call [`reset`] to load PC/SP from the BIOS reset
    /// vector before stepping.
    pub fn new(bios: Vec<u8>) -> Self {
        let bus = SaturnBus::new(bios);
        let mut scheduler = Scheduler::new();
        // Insertion order is the determinism tie-break: master, then slave,
        // then the CD-block timer. The CD entity goes last so that on a tie
        // (its deadline equal to a CPU's) the CPU steps first.
        let master_id = scheduler.add(SaturnEntity::Sh2(Sh2Entity::new(Cpu::new())));
        let slave_id = scheduler.add(SaturnEntity::Sh2(Sh2Entity::new(Cpu::new())));
        let cd_id = scheduler.add(SaturnEntity::CdBlock(CdBlockEntity::new(CD_TICK_CYCLES)));
        Self {
            bus,
            scheduler,
            master_id,
            slave_id,
            cd_id,
            master_pcstream: None,
            seqlog: None,
        }
    }

    /// Construct with an all-zero BIOS — convenient for tests that
    /// don't need a real boot image.
    pub fn with_blank_bios() -> Self {
        Self::new(vec![0u8; 512 * 1024])
    }

    /// Pull PC and SP for both CPUs from the BIOS reset vector
    /// (`0x00000000` for PC, `0x00000004` for SP) and clear pipeline
    /// state. On real hardware the slave is held in reset until the
    /// master writes the SMPC `SETSL` command — for M2 we bring both
    /// up immediately; SMPC-driven slave hold-down arrives in M3.
    pub fn reset(&mut self) {
        // Destructure self into disjoint borrows so the bus borrow doesn't
        // collide with the scheduler-entity borrow.
        let Self {
            bus,
            scheduler,
            master_id,
            slave_id,
            ..
        } = self;
        scheduler.entity_mut(*master_id).sh2_mut().cpu.reset(bus);
        scheduler.entity_mut(*slave_id).sh2_mut().cpu.reset(bus);
        // The slave SH-2 reads BCR1 bit 15 (SH7604 master/slave bit) = 1, so the
        // BIOS cold-start takes the slave path and does NOT re-initialize work
        // RAM. Without this, an SSHON-released slave re-runs the WRAM init and
        // clobbers the running game (the M11 first-screen blocker). A
        // hardware/pin property that survives reset, so set it here once.
        scheduler
            .entity_mut(*slave_id)
            .sh2_mut()
            .cpu
            .set_bsc_slave(true);
        // Real Saturn power-on: slave is held in reset until the BIOS
        // sends SMPC SETSL. Mirror that here.
        scheduler.entity_mut(*slave_id).sh2_mut().set_halted(true);
        scheduler.entity_mut(*master_id).sh2_mut().set_halted(false);
    }

    /// Halt the slave SH-2. Triggered by SMPC `SSHOFF`.
    pub fn halt_slave(&mut self) {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .set_halted(true);
    }

    /// Release the slave SH-2 from halt. Triggered by SMPC `SSHON`.
    /// Release the slave SH-2 (SMPC `SSHON`). On real hardware `SetActive(true)`
    /// **power-on-resets** the slave — VBR=0, SR.imask=0xF, PC/SP re-fetched
    /// from the reset vector — it does *not* resume whatever PC/SP it held when
    /// last halted (matches Mednafen `sh7095.inc` `SetActive`→`Reset`). For the
    /// first release this is equivalent to our power-on [`reset`] (the slave was
    /// reset then held), but for an `SSHOFF`→`SSHON` re-release it correctly
    /// re-vectors to the BIOS rather than resuming stale mid-execution state.
    ///
    /// Then resync the slave's cycle to the current global cycle: while halted
    /// its `next_deadline` is `u64::MAX`, so the scheduler skips it and its
    /// `pipeline.cycles` freezes; releasing without the bump would make the
    /// scheduler see it as millions of cycles "behind" and run that many
    /// catch-up steps in one batch ("time travel"). Regression:
    /// `dual_sh2::releasing_slave_resyncs_its_cycle_no_time_travel`.
    pub fn release_slave(&mut self) {
        // Destructure for disjoint borrows (cpu.reset needs &mut bus; the
        // entity comes from the scheduler) — same pattern as `reset`.
        let Self {
            bus,
            scheduler,
            slave_id,
            ..
        } = self;
        let now = scheduler.now();
        let slave = scheduler.entity_mut(*slave_id).sh2_mut();
        slave.cpu.reset(bus);
        if slave.cpu.pipeline.cycles < now {
            slave.cpu.pipeline.cycles = now;
        }
        slave.set_halted(false);
    }

    pub fn slave_is_halted(&self) -> bool {
        self.scheduler.entity(self.slave_id).sh2().is_halted()
    }

    pub fn master_is_halted(&self) -> bool {
        self.scheduler.entity(self.master_id).sh2().is_halted()
    }

    pub fn master(&self) -> &Cpu {
        &self.scheduler.entity(self.master_id).sh2().cpu
    }
    pub fn master_mut(&mut self) -> &mut Cpu {
        &mut self.scheduler.entity_mut(self.master_id).sh2_mut().cpu
    }
    pub fn slave(&self) -> &Cpu {
        &self.scheduler.entity(self.slave_id).sh2().cpu
    }
    pub fn slave_mut(&mut self) -> &mut Cpu {
        &mut self.scheduler.entity_mut(self.slave_id).sh2_mut().cpu
    }

    /// Debug-only: start a full-speed master-PC trace (M11 boot investigation).
    /// Records every instruction's PC at `run_for`/`run_frame` speed — so the
    /// VBlank-interrupt-driven boot flow is captured faithfully, unlike the
    /// single-step `debug_step_master` path.
    pub fn enable_master_pc_trace(&mut self) {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .enable_pc_trace();
    }

    /// Debug-only: set the PC range `[lo, hi)` that freezes the master-PC trace
    /// ring (M11/M12). The HLE direct boot uses the low BIOS-RAM idle region.
    pub fn set_master_trace_freeze(&mut self, lo: u32, hi: u32) {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .set_trace_freeze(lo, hi);
    }

    /// Debug-only: drain the recorded master-PC trace.
    pub fn take_master_pc_trace(&mut self) -> Vec<u32> {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .take_pc_trace()
    }

    /// Debug-only: arm the per-instruction master-SH-2 PC+cycle stream (the
    /// master-side `pcstream`; recorded in `step_cpus_hooked`, capped). Drain
    /// with [`Self::take_master_pcstream`].
    pub fn enable_master_pcstream(&mut self) {
        self.master_pcstream.get_or_insert_with(Vec::new);
    }

    /// Debug-only: drain the master PC+cycle stream `(pc, accumulated_cycle)`.
    pub fn take_master_pcstream(&mut self) -> Vec<(u32, u64)> {
        self.master_pcstream.take().unwrap_or_default()
    }

    /// Debug-only (M11 Doukyuusei menu): record `(R[reg], cycle)` every time the
    /// master's low-24 PC equals `target_low24`. Used to capture the menu
    /// controller's dispatch-index sequence, cycle-stamped for time-alignment vs
    /// Mednafen's `SS_LOGSEQ`. Drain with [`Self::take_seqlog`].
    pub fn enable_seqlog(&mut self, target_low24: u32, reg: usize) {
        self.seqlog = Some((target_low24 & 0x00FF_FFFF, reg, Vec::new()));
    }
    /// Drain the accumulated dispatch-index records, leaving the logger armed.
    pub fn take_seqlog(&mut self) -> Vec<(u32, u64)> {
        match self.seqlog.as_mut() {
            Some((_, _, v)) => core::mem::take(v),
            None => Vec::new(),
        }
    }

    /// Debug-only: start / drain a full-speed *slave*-PC trace (M12 slave
    /// dispatch — map the BIOS slave-init path: clear → poll loop?).
    pub fn enable_slave_pc_trace(&mut self) {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .enable_pc_trace();
    }
    pub fn take_slave_pc_trace(&mut self) -> Vec<u32> {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .take_pc_trace()
    }

    /// Debug-only: arm a full-speed breakpoint capturing the master's regs +
    /// code at `pc` (to inspect a transient work-RAM routine; M11).
    pub fn set_master_bp(&mut self, pc: u32) {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .set_bp(pc);
    }

    /// Debug-only: arm a register-guarded master breakpoint — fires at `pc`
    /// only when `R[idx] == val`. Used to stop at a shared routine (the generic
    /// CD-command writer) on the one call carrying a specific argument.
    pub fn set_master_bp_cond(&mut self, pc: u32, idx: usize, val: u32) {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .set_bp_cond(pc, idx, val);
    }

    /// Debug-only: take a master breakpoint hit's (R0..R15, PR, GBR, code words,
    /// probe-value), if it fired. The probe value is the bus read of the address
    /// set via [`set_master_bp_probe`] at the hit cycle (0 if unset).
    pub fn take_master_bp_hit(&mut self) -> Option<crate::scheduler::BpHit> {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .take_bp_hit()
    }

    /// Debug-only: set/clear the master breakpoint memory probe (see
    /// [`crate::scheduler::Sh2Entity::set_bp_probe`]).
    pub fn set_master_bp_probe(&mut self, addr: Option<u32>) {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .set_bp_probe(addr);
    }

    /// Debug-only: arm a full-speed breakpoint on the *slave* SH-2.
    pub fn set_slave_bp(&mut self, pc: u32) {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .set_bp(pc);
    }

    /// Debug-only: arm a register-guarded *slave* breakpoint — fires at `pc`
    /// only when `R[idx] == val` (mirror of [`set_master_bp_cond`]). Used for
    /// slave-crash debugging — e.g. stop at a `JSR @Rn` exactly on the call
    /// where the function-pointer register is null (the Doukyuusei intro crash).
    pub fn set_slave_bp_cond(&mut self, pc: u32, idx: usize, val: u32) {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .set_bp_cond(pc, idx, val);
    }

    /// Debug-only: take the slave breakpoint hit's (R0..R15, PR, GBR, code words,
    /// probe-value). The probe value is the bus read of [`set_slave_bp_probe`]'s
    /// address at the hit cycle (0 if unset).
    pub fn take_slave_bp_hit(&mut self) -> Option<crate::scheduler::BpHit> {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .take_bp_hit()
    }

    /// Debug-only: set/clear the slave breakpoint memory probe (see
    /// [`crate::scheduler::Sh2Entity::set_bp_probe`]).
    pub fn set_slave_bp_probe(&mut self, addr: Option<u32>) {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .set_bp_probe(addr);
    }

    /// Debug-only: arm (or clear, with `None`) a breakpoint on the SCSP's hosted
    /// MC68EC000 sound CPU at `pc`, optionally guarded (`reg`, `val`) where reg
    /// 0-7 = D0-D7, 8-15 = A0-A7. Used to break inside the BIOS sound driver — e.g.
    /// at the voice key-on code — and inspect why a key-on isn't issued (M11 BGM).
    pub fn set_scsp_bp68(&mut self, bp: Option<(u32, Option<(u8, u32)>)>) {
        self.bus.scsp.set_bp68(bp);
    }

    /// Debug-only: take the SCSP 68k breakpoint hit's register snapshot, if fired.
    pub fn take_scsp_bp68_hit(&mut self) -> Option<crate::scsp::M68kBpHit> {
        self.bus.scsp.take_bp68_hit()
    }

    /// Debug-only: step the master SH-2 exactly one instruction, then
    /// drain SMPC/SCU side effects. Used by the reference-emulator PC
    /// trace diff (M4 task #5). Returns the cycles the instruction took.
    pub fn debug_step_master(&mut self) -> u32 {
        let cycles = self.debug_step_master_nodrain();
        self.debug_drain();
        cycles
    }

    /// Debug-only: step the master one instruction WITHOUT draining
    /// SMPC/SCU. Lets trace tooling control drain granularity to
    /// reproduce `run_for`'s batched draining.
    pub fn debug_step_master_nodrain(&mut self) -> u32 {
        let Self {
            bus,
            scheduler,
            master_id,
            ..
        } = self;
        // Phase 2B: sample the SCU IRL before the instruction, exactly as
        // `step_cpus` does on the real run, so the single-step debug path and
        // `run_for` deliver SCU interrupts at the same (per-instruction) point.
        let cd_active = bus.cd_block.irq_active();
        bus.scu.set_cd_int(cd_active);
        let imask = scheduler.entity(*master_id).sh2().cpu.regs.sr.imask();
        if let Some((source, level)) = bus.scu.take_pending_interrupt(imask) {
            scheduler
                .entity_mut(*master_id)
                .sh2_mut()
                .cpu
                .onchip
                .intc
                .raise_external(level, source.vector());
        }
        let cpu = &mut scheduler.entity_mut(*master_id).sh2_mut().cpu;
        // Mirror Sh2Entity::step: publish the current cycle to the bus so
        // time-varying peripheral reads (SMPC SF INTBACK completion) settle
        // at the exact instruction that reads them.
        bus.cycle = cpu.pipeline.cycles;
        cpu.step(bus)
    }

    /// Debug-only: step the **slave** SH-2 one instruction (master frozen),
    /// for inspecting slave code. Returns 0 if the slave is halted. The slave
    /// receives no SCU interrupt (those go to the master), so this is a plain
    /// step; bus writes still trigger the `bw` watchpoint with the slave's PC.
    pub fn debug_step_slave(&mut self) -> u32 {
        let Self {
            bus,
            scheduler,
            slave_id,
            ..
        } = self;
        let s = scheduler.entity_mut(*slave_id).sh2_mut();
        if s.is_halted() {
            return 0;
        }
        bus.cycle = s.cpu.pipeline.cycles;
        bus.step_pc = s.cpu.regs.pc;
        s.cpu.step(bus)
    }

    /// Debug-only: run the SMPC/SCU drains once (the same set `run_for`
    /// performs between scheduler batches).
    pub fn debug_drain(&mut self) {
        // The CD-block runs on its own scheduler entity, but the single-step
        // debug path drives only the master CPU and never enters the
        // scheduler, so advance the CD timer here to track the master's
        // cycle (what `run_for`'s scheduler does automatically). This also
        // keeps `now()` pinned to the master — otherwise the CD entity's
        // un-advanced deadline would become the global-clock minimum.
        self.catch_up_cd_block();
        self.update_video_timing();
        self.drain_smpc();
        let _ = drain_dma(&mut self.bus); // backstop; step_cpus drains+charges per-instruction
        self.drain_scu_dsp();
        self.drain_vdp1();
        self.drain_scsp();
    }

    /// Step the CD-block timer entity until its deadline passes the master's
    /// current cycle. Used only by the single-step debug path; `run_for`
    /// advances the CD entity through the scheduler instead.
    fn catch_up_cd_block(&mut self) {
        let Self {
            bus,
            scheduler,
            master_id,
            cd_id,
            ..
        } = self;
        let master_cycle = scheduler.entity(*master_id).sh2().cpu.pipeline.cycles;
        while scheduler.entity(*cd_id).next_deadline() <= master_cycle {
            scheduler.entity_mut(*cd_id).step(bus);
        }
    }

    /// Recompute the VDP2 raster-timing registers — `VCNT` and the
    /// `TVSTAT` VBLANK/HBLANK/ODD-field bits — from the global cycle,
    /// and raise the SCU VBlank-IN interrupt on the active→VBLANK edge.
    /// The BIOS polls these to synchronize with the display; a static
    /// stub leaves it unable to sync (it spins after INTBACK). Called
    /// between scheduler batches, so the registers track the raster to
    /// ~`SMPC_POLL_QUANTUM` granularity.
    fn update_video_timing(&mut self) {
        let now = self.now();
        let frame = now / CYCLES_PER_FRAME;
        let frame_cycle = now % CYCLES_PER_FRAME;
        let line = (frame_cycle / CYCLES_PER_LINE).min(LINES_PER_FRAME - 1);
        let line_cycle = frame_cycle % CYCLES_PER_LINE;

        let prev = self.bus.vdp2.regs.read16(0x004);
        // Previous scanline (the VCNT register before we overwrite it below) —
        // the edge reference for the SCU Timer-0 line compare.
        let prev_line = self.bus.vdp2.regs.read16(0x00A);
        // Use the precise frame-derived edge (matches the `run_for` clamp and
        // the reference) rather than the rounded per-line `line >= 224`.
        let vblank = frame_cycle >= Self::VBLANK_IN_CYCLE;
        let mut tvstat = prev & !0x000E; // clear VBLANK | HBLANK | ODD
        if vblank {
            tvstat |= 0x0008; // VBLANK
        }
        let h_res = self.bus.vdp2.regs.h_resolution();
        let hblank = hblank_active(line_cycle, h_res, CYCLES_PER_LINE);
        if hblank {
            tvstat |= 0x0004; // HBLANK
        }
        if frame & 1 == 1 {
            tvstat |= 0x0002; // ODD field
        }
        self.bus.vdp2.regs.write16(0x00A, line as u16); // VCNT
        self.bus.vdp2.regs.write16(0x004, tvstat);

        // Raise VBlank-IN once, on the transition into the VBLANK region.
        // The CD-block's periodic status report is no longer fired here —
        // it runs on its own scheduler entity ([`CdBlockEntity`]) at
        // sub-frame granularity, so the report lands at the cycle-exact
        // point within the frame the reference produces it rather than being
        // pinned to this edge.
        if vblank && (prev & 0x0008) == 0 {
            self.bus.scu.raise(crate::scu::Source::VBlankIn);
            // VBlank-IN is SCU DMA start factor 0.
            self.bus.scu.trigger_dma_factor(0);
            // VDP1 swaps its draw/display buffers at the frame boundary and,
            // in automatic-draw mode, re-renders the command list into the
            // back buffer.
            self.bus.vdp1.frame_change(now);
        }
        // VBlank-OUT edge (VBLANK → active, i.e. the start of the next frame's
        // active display): raise the SCU VBlank-OUT interrupt and fire SCU DMA
        // start factor 1. The BIOS installs a VBlank-OUT callback (SCU vector
        // 0x41) that advances its frame counter at `[0x060408A4]`; without this
        // interrupt that counter never ticks and the BIOS boot parks forever
        // polling it (the splash never appears). Mirrors the VBlank-IN edge.
        if !vblank && (prev & 0x0008) != 0 {
            self.bus.scu.raise(crate::scu::Source::VBlankOut);
            self.bus.scu.trigger_dma_factor(1);
            #[cfg(not(test))]
            if std::env::var_os("SAT_INTC_TRACE").is_some() && now > 130_000_000 {
                eprintln!("RAISE VBlankOut now={now} IMS={:08X}", self.bus.scu.ims);
            }
        }

        // SCU Timer 0 — a per-frame line compare. When the timer is enabled
        // (T1MD bit 0, TENB) the SCU raises the Timer-0 interrupt as the raster
        // first reaches scanline T0C (a 10-bit value), letting games schedule a
        // mid-frame (raster-split) interrupt. Edge-detected against the previous
        // scanline so it fires once per frame. Dormant unless software sets
        // TENB, so the BIOS boot path is unaffected. (*SCU User's Manual*,
        // T0C/T1MD.)
        let t0c = (self.bus.scu.t0c & 0x3FF) as u16;
        let timer0_met = line as u16 == t0c;
        if self.bus.scu.timers_enabled() && timer0_met && prev_line != t0c {
            self.bus.scu.raise(crate::scu::Source::Timer0);
        }

        // Timer 1 (sub-line H-position) + the HBlank-IN interrupt, ported from
        // Mednafen `SCU_SetHBVB`: the Timer-1 down-counter decrements by the dots
        // elapsed and fires at H-position T1S (per line, or only on the Timer-0
        // line in mode 1); HBlank-IN fires on the HBLANK rising edge. Both gated
        // by TENB → dormant on the boot path. (M13 D5.)
        let dots_per_line = if h_res & 1 == 1 { 455 } else { 427 };
        self.bus
            .scu
            .tick_timers(now, CYCLES_PER_LINE, dots_per_line, hblank, timer0_met);

        // Complete any in-flight VDP1 plot whose draw duration has elapsed,
        // even between CPU accesses, so draw-end lands at the modelled cycle.
        self.bus.vdp1.settle(now);
    }

    /// Within-frame cycle of the active→VBLANK transition (start of line
    /// [`ACTIVE_LINES`]) — where VBlank-IN is raised. Computed from the frame
    /// length (not `ACTIVE_LINES * CYCLES_PER_LINE`) so per-line integer
    /// rounding doesn't shift the edge off the reference's.
    const VBLANK_IN_CYCLE: u64 = ACTIVE_LINES * CYCLES_PER_FRAME / LINES_PER_FRAME;

    /// Cycles from `now` until the next active→VBLANK edge. `run_for` clamps
    /// its batch to this so the scheduler stops at the edge and VBlank-IN is
    /// raised within one instruction of the exact raster cycle (the master
    /// then takes it at its first instruction boundary past the edge, as on
    /// hardware) rather than up to a full `SMPC_POLL_QUANTUM` late.
    fn cycles_to_next_vblank_in(now: u64) -> u64 {
        let frame_cycle = now % CYCLES_PER_FRAME;
        if frame_cycle < Self::VBLANK_IN_CYCLE {
            Self::VBLANK_IN_CYCLE - frame_cycle
        } else {
            CYCLES_PER_FRAME - frame_cycle + Self::VBLANK_IN_CYCLE
        }
    }

    /// Cycles from `now` until the next VBLANK→active edge (the frame
    /// boundary, start of the next frame's active display) — where VBlank-OUT
    /// is raised. The BIOS's VBlank-OUT callback (SCU vector 0x41) advances its
    /// frame counter, so the batch must stop on this edge for the interrupt to
    /// land cycle-exactly rather than up to a batch late. One edge per frame.
    fn cycles_to_next_vblank_out(now: u64) -> u64 {
        CYCLES_PER_FRAME - (now % CYCLES_PER_FRAME)
    }

    /// Cycles from `now` until the next scheduled peripheral side-effect edge.
    /// This is the local analogue of Mednafen's `next_event_ts` (`ss.cpp`): the
    /// batch is clamped to it so interrupt assertion and the raster registers
    /// settle at the cycle-exact point the reference produces them, rather than
    /// up to a [`SMPC_POLL_QUANTUM`]-cycle batch late.
    ///
    /// Included edges: VBlank-IN, VBlank-OUT, and a pending INTBACK completion
    /// (`smpc.intback_complete_at`). **Deliberately excluded** — do NOT add
    /// without the noted prerequisites:
    /// - **HBlank**: `TVSTAT.HBLANK` here is an invented "last 20% of the
    ///   scanline" approximation (see `update_video_timing`), not the real
    ///   H-blank dot count — clamping to it would lock in a *precise* divergence
    ///   from the reference and add ~526 stops/frame for no diff benefit. Add
    ///   only once the dot count is corrected and a divergence points at HBLANK.
    /// - **SCU DMA**: synchronous/instant in our model (`drain_dma` finishes
    ///   the whole transfer at the boundary) — there is no future completion
    ///   timestamp to clamp to. Making it a timed event is a later model change.
    fn cycles_to_next_event(&self, now: u64) -> u64 {
        let mut next =
            Self::cycles_to_next_vblank_in(now).min(Self::cycles_to_next_vblank_out(now));
        if let Some(t) = self.bus.smpc.intback_complete_at
            && t > now
        {
            next = next.min(t - now);
        }
        // VDP1 sprite-draw-end: an in-flight plot completes at a known cycle, so
        // make the batch land there exactly — the draw-end interrupt fires at the
        // modelled cycle, not up to a batch late (M13 A1, incremental).
        if let Some(t) = self.bus.vdp1.draw_end_cycle()
            && t > now
        {
            next = next.min(t - now);
        }
        // SCU Timer 0 line compare (when enabled, T1MD bit 0): the next match is
        // the start of scanline T0C — this frame if not yet passed, else next —
        // so the Timer-0 interrupt fires at the exact line, not a batch late.
        if self.bus.scu.t1md & 1 != 0 {
            let t0c = (self.bus.scu.t0c & 0x3FF) as u64;
            if t0c < LINES_PER_FRAME {
                let frame_start = (now / CYCLES_PER_FRAME) * CYCLES_PER_FRAME;
                let mut t = frame_start + t0c * CYCLES_PER_LINE;
                if t <= now {
                    t += CYCLES_PER_FRAME;
                }
                next = next.min(t - now);
            }
        }
        next
    }

    /// Size of the next scheduler batch: the smallest of the requested
    /// `remaining` horizon, the [`SMPC_POLL_QUANTUM`] safety ceiling, and the
    /// next scheduled event edge ([`cycles_to_next_event`]). The `.max(1)` keeps
    /// a batch from ever being zero (which would spin `run_for` forever when
    /// `now` sits exactly on an edge). Shared by [`run_for`](Self::run_for) and
    /// [`run_for_traced`](Self::run_for_traced) so the trace tool and the real
    /// run can never compute different batch boundaries.
    fn batch_size(&self, now: u64, remaining: u64) -> u64 {
        remaining
            .min(SMPC_POLL_QUANTUM)
            .min(self.cycles_to_next_event(now).max(1))
    }

    /// Step the SH-2 pair (+ the CD-block firmware timer) up to global cycle
    /// `target`, in **Mednafen's RunLoop order** (`ss.cpp`: `CPU[0].Step()`
    /// then `RunSlaveUntil(CPU[0].timestamp)`): the **master** executes one
    /// instruction, then the **slave** runs until it catches up to the
    /// master's timestamp (overshooting by at most one instruction). The
    /// master therefore always leads by one instruction, so the two cores'
    /// interleaved work-RAM accesses (and inter-CPU handoffs) match the
    /// reference order — Phase-2 alignment. The previous most-behind-first
    /// rule (`Scheduler::pick_behind`) could let the *slave* lead the master,
    /// which diverged timing-sensitive game logic (the VF2 CD-load decision).
    ///
    /// CD-block periodic ticks fire when the master's timestamp passes their
    /// scheduled cycle (peripheral events run against the master clock, as in
    /// Mednafen's event loop). A halted slave is skipped — its release
    /// resyncs its cycle (see [`release_slave`](Self::release_slave)).
    fn step_cpus(&mut self, target: u64) {
        self.step_cpus_hooked(target, |_| {});
    }

    /// [`step_cpus`](Self::step_cpus) with a hook invoked on the master entity
    /// immediately before each master instruction — used by the boot PC tracer
    /// so its trace is produced in the *same* interleave order as the real run
    /// (the `run_frame`/`run_for` consistency lesson of the split-frame fix).
    fn step_cpus_hooked<F: FnMut(&crate::scheduler::Sh2Entity)>(
        &mut self,
        target: u64,
        mut before_master: F,
    ) {
        let Self {
            bus,
            scheduler,
            master_id,
            slave_id,
            cd_id,
            master_pcstream,
            seqlog,
            ..
        } = self;
        // Apply any inter-CPU FRT input-capture (FTI) pulse the just-executed
        // instruction flagged on the bus — pulse the *sibling's* FRT now so it
        // sees the input-capture on its next instruction, not up to a batch
        // (≤256 cy) later (M13 A1, incremental: FTI as a per-instruction event).
        macro_rules! apply_fti {
            () => {
                if core::mem::take(&mut bus.slave_input_capture) {
                    scheduler
                        .entity_mut(*slave_id)
                        .sh2_mut()
                        .cpu
                        .onchip
                        .frt
                        .input_capture();
                }
                if core::mem::take(&mut bus.master_input_capture) {
                    scheduler
                        .entity_mut(*master_id)
                        .sh2_mut()
                        .cpu
                        .onchip
                        .frt
                        .input_capture();
                }
            };
        }
        loop {
            let mcyc = scheduler.entity(*master_id).sh2().cpu.pipeline.cycles;
            if mcyc >= target {
                break;
            }
            // Peripheral (CD-block) events scheduled at or before the master's
            // current timestamp fire before the master advances past them.
            while scheduler.entity(*cd_id).next_deadline() <= mcyc {
                scheduler.entity_mut(*cd_id).step(bus);
            }
            // CD-block external interrupt (Source::Cd, vector 0x50, level 7):
            // assert/deassert the SCU level from the live CD HIRQ before each
            // instruction (Mednafen `RecalcIRQOut`). VF2's GFS file library is
            // driven by this interrupt; without it the intro loops forever.
            let cd_active = bus.cd_block.irq_active();
            bus.scu.set_cd_int(cd_active);
            // Phase 2B: sample the SCU interrupt line at the master's *current*
            // SR.imask, every instruction. The SCU presents the highest-priority
            // unmasked pending source as an IRL the master samples each cycle, so
            // an interrupt becomes deliverable at the exact instruction imask
            // drops below its level — not up to a full batch late, as the old
            // once-per-batch `drain_scu_intc` forwarding did. The SCU's fixed
            // per-source vector (0x40 + index) is latched, not the 64+level
            // auto-vector.
            let imask = scheduler.entity(*master_id).sh2().cpu.regs.sr.imask();
            if let Some((source, level)) = bus.scu.take_pending_interrupt(imask) {
                scheduler
                    .entity_mut(*master_id)
                    .sh2_mut()
                    .cpu
                    .onchip
                    .intc
                    .raise_external(level, source.vector());
            }
            // Master leads by one instruction.
            before_master(scheduler.entity(*master_id).sh2());
            // Debug master PC+cycle stream (cost-lockstep vs Mednafen), capped.
            // Skip delay-slot PCs so the stream is one entry per logical
            // instruction, matching Mednafen's per-`Step` log (the delay slot's
            // cost folds into the branch's cycle delta).
            if let Some(ps) = master_pcstream.as_mut()
                && ps.len() < 16_000_000
            {
                let e = scheduler.entity(*master_id).sh2();
                if !e.cpu.next_is_delay_slot() {
                    ps.push((e.cpu.regs.pc, e.cpu.pipeline.cycles));
                }
            }
            // Dispatch-index seqlog: record R[reg] when the master is about to
            // execute the controller's dispatch PC (cycle-stamped for alignment).
            if let Some((tgt, reg, log)) = seqlog.as_mut() {
                let e = scheduler.entity(*master_id).sh2();
                if (e.cpu.regs.pc & 0x00FF_FFFF) == *tgt && !e.cpu.next_is_delay_slot() {
                    log.push((e.cpu.regs.r[*reg], e.cpu.pipeline.cycles));
                }
            }
            scheduler.entity_mut(*master_id).step(bus);
            apply_fti!(); // master may have pulsed the slave's (or its own) FTI
            // If the master's instruction triggered an SCU DMA, run it now and
            // stall the master for the transfer's cycle-stealing cost — the SCU
            // holds the bus, so the master can't advance during it (M13 A3). The
            // slave then catches up to the *stalled* timestamp, i.e. it runs
            // while the master is DMA-blocked.
            let dma_cost = drain_dma(bus);
            if dma_cost != 0 {
                scheduler
                    .entity_mut(*master_id)
                    .sh2_mut()
                    .cpu
                    .pipeline
                    .cycles += dma_cost;
            }
            let mcyc = scheduler.entity(*master_id).sh2().cpu.pipeline.cycles;
            // Slave catches up to the master's new timestamp.
            while {
                let s = scheduler.entity(*slave_id).sh2();
                !s.is_halted() && s.cpu.pipeline.cycles < mcyc
            } {
                scheduler.entity_mut(*slave_id).step(bus);
                apply_fti!(); // slave may have pulsed the master's FTI
                // A slave-triggered DMA stalls the slave for the same reason.
                let dma_cost = drain_dma(bus);
                if dma_cost != 0 {
                    scheduler
                        .entity_mut(*slave_id)
                        .sh2_mut()
                        .cpu
                        .pipeline
                        .cycles += dma_cost;
                }
            }
        }
        // Trailing CD-block ticks up to the batch target.
        while scheduler.entity(*cd_id).next_deadline() <= target {
            scheduler.entity_mut(*cd_id).step(bus);
        }
    }

    /// Advance global time by at least `cycles` cycles, interleaving
    /// the two CPUs by deadline order and polling SMPC + SCU between
    /// scheduler batches.
    pub fn run_for(&mut self, cycles: u64) {
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let now = self.now();
            let remaining = target - now;
            // Clamp the batch to the next scheduled event edge so peripheral
            // side-effects (VBlank-IN/OUT, INTBACK completion) settle
            // cycle-exactly rather than up to a batch late — see `batch_size`.
            let batch = self.batch_size(now, remaining);
            self.step_cpus(now + batch);
            self.bus.scsp.run(batch);
            self.update_video_timing();
            self.drain_smpc();
            let _ = drain_dma(&mut self.bus); // backstop (per-instruction drain+charge in step_cpus)
            self.drain_scu_dsp();
            self.drain_vdp1();
            self.drain_scsp();
            self.drain_input_capture();
        }
    }

    /// Debug-only: like [`run_for`](Self::run_for), but append the master
    /// SH-2's PC (skipping delay slots, to match reference traces) to `pcs`
    /// as the scheduler steps it. Unlike the master-only single-step tracer,
    /// this runs the *full* scheduler (master + slave + CD-block), so the
    /// master's interrupt phase reflects the real `run_frame` path — needed
    /// to diff the `run_frame` boot park against a reference.
    pub fn run_for_traced(&mut self, cycles: u64, pcs: &mut Vec<u32>) {
        // Reference traces differ on whether they log branch delay slots:
        // Yabause omits them; MAME logs them. Set PCTRACE_DELAYSLOTS to match
        // a MAME reference trace (which includes the slot PC).
        let log_delay_slots = std::env::var("PCTRACE_DELAYSLOTS").is_ok();
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let now = self.now();
            let remaining = target - now;
            let batch = self.batch_size(now, remaining);
            // Same master-leads-slave order as `run_for`, recording each master
            // PC before it steps — so the trace can't diverge from the real run.
            self.step_cpus_hooked(now + batch, |m| {
                let cpu = &m.cpu;
                if log_delay_slots || !cpu.next_is_delay_slot() {
                    pcs.push(cpu.regs.pc);
                }
            });
            self.bus.scsp.run(batch);
            self.update_video_timing();
            self.drain_smpc();
            let _ = drain_dma(&mut self.bus); // backstop (per-instruction drain+charge in step_cpus)
            self.drain_scu_dsp();
            self.drain_vdp1();
            self.drain_scsp();
            self.drain_input_capture();
        }
    }

    /// Pop any command queued by SMPC and apply its emulator-wide side
    /// effect. Called from `run_for` between scheduler batches.
    fn drain_smpc(&mut self) {
        // Fallback: drop SF once a pending INTBACK's execution time has
        // elapsed, in case nothing read SMPC to settle it on-access.
        self.bus.smpc.settle_intback(self.now());
        while let Some(cmd) = self.bus.smpc.take_pending() {
            match cmd {
                SmpcCommand::SshOn => self.release_slave(),
                SmpcCommand::SshOff => self.halt_slave(),
                // SNDON releases the sound 68k from reset (reloading its
                // vectors from the program the main CPU staged into sound
                // RAM); SNDOFF re-holds it.
                SmpcCommand::SndOn => self.bus.scsp.start(),
                SmpcCommand::SndOff => self.bus.scsp.stop(),
                SmpcCommand::NmiReq => {
                    // SMPC NMIREQ asserts NMI on the master SH-2.
                    // NMI bypasses SR.imask so it fires at the next
                    // instruction boundary regardless of mask state —
                    // which is exactly what the BIOS expects: it sets
                    // imask=15 and busy-waits on an NMI handler.
                    self.master_mut()
                        .onchip
                        .intc
                        .raise(sh2::InterruptSource::Nmi);
                }
                SmpcCommand::IntBack => {
                    // INTBACK status phase (Mednafen `resolve_intback`). Fill
                    // the status OREG and arm the staged-peripheral protocol:
                    // `intback_stage = (IREG1 & 8) >> 3` (1 if peripheral data
                    // was requested), `pmode = IREG0 >> 4`. The status-phase SR
                    // is `(SR & ~0x80 & ~NPE) | 0x0F`, with NPE (0x20) set iff
                    // peripheral data was also requested — *not* the old
                    // `0x40 | stage<<5` (bit 6 set, low nibble 0), which the
                    // BIOS's INTBACK state machine reads. Raise the SMPC
                    // interrupt now so the response is ready the moment SF
                    // drops, and keep SF busy for the request-dependent
                    // execution time (`intback_busy_us`); `settle_intback`
                    // clears it on the exact instruction that reads SMPC past
                    // completion.
                    let ireg0 = self.bus.smpc.ireg[0];
                    let ireg1 = self.bus.smpc.ireg[1];
                    self.respond_to_intback_status();
                    let stage = (ireg1 & 0x08) >> 3;
                    self.bus.smpc.intback_stage = stage;
                    self.bus.smpc.pmode = ireg0 >> 4;
                    let npe = if stage != 0 { 0x20 } else { 0x00 }; // SR_NPE: peripheral data follows
                    self.bus.smpc.sr = (self.bus.smpc.sr & !0xA0) | 0x0F | npe;
                    let busy = us_to_cycles(intback_busy_us(ireg0, ireg1));
                    self.bus.smpc.intback_complete_at = Some(self.now().saturating_add(busy));
                    self.bus.scu.raise(crate::scu::Source::Smpc);
                    continue; // do NOT mark_command_done (SF stays busy)
                }
                SmpcCommand::SetTime => {
                    // SETTIME loads the RTC from the seven IREG bytes (same
                    // layout as the INTBACK RTC: year-hi/lo, weekday|month,
                    // day, hour, minute, second).
                    let now = self.now();
                    let ireg = self.bus.smpc.ireg;
                    self.bus.smpc.set_rtc_bcd(ireg, now);
                }
                SmpcCommand::SetSMem => {
                    // SETSMEM writes the four SMPC-backup-memory bytes from
                    // IREG0..3; they're echoed in INTBACK OREG12..15.
                    let ireg = self.bus.smpc.ireg;
                    self.bus.smpc.smem.copy_from_slice(&ireg[0..4]);
                }
                // CKCHG352/CKCHG320 — system-clock change. On hardware the SMPC
                // does a partial system reset (sound CPU off; SCSP/VDP1/VDP2/SCU
                // soft-reset), switches the dot-clock divisor (28 MHz / 26 MHz),
                // then asserts a master-SH-2 NMI after a few VBlanks (Mednafen
                // `smpc.cpp` CMD_CKCHG). The BIOS `ChangeSystemClock` SYS call
                // issues this and *waits for that NMI*; without it the BIOS times
                // out into its fatal handler (this is exactly what VF2's startup
                // hits — SYS slot 0x320). We don't model the 26/28 MHz divisor
                // switch, but we reproduce the observable handshake: halt the
                // slave and NMI the master so the SYS call returns.
                SmpcCommand::CkChg352 | SmpcCommand::CkChg320 => {
                    self.halt_slave();
                    self.master_mut()
                        .onchip
                        .intc
                        .raise(sh2::InterruptSource::Nmi);
                }
                // Remaining commands (reset-enable/disable, …) are recognised
                // but have no emulator-side effect yet.
                _ => {}
            }
            self.bus.smpc.mark_command_done();
        }

        // INTBACK peripheral continuation (MAME `intback_continue_request`),
        // requested by the host writing the CONTINUE bit to IREG0. Fill the
        // peripheral OREG and advance the staged SR: `0xC0 | pmode` ("more
        // data", stage 1 → 2) then `0x80 | pmode` ("last", stage 2 → 0).
        if self.bus.smpc.take_intback_continue() {
            self.respond_to_intback_peripheral();
            let pmode = self.bus.smpc.pmode;
            if self.bus.smpc.intback_stage == 2 {
                self.bus.smpc.sr = 0x80 | pmode;
                self.bus.smpc.intback_stage = 0;
            } else {
                self.bus.smpc.sr = 0xC0 | pmode;
                self.bus.smpc.intback_stage += 1;
            }
            let busy = us_to_cycles(700);
            self.bus.smpc.intback_complete_at = Some(self.now().saturating_add(busy));
            self.bus.scu.raise(crate::scu::Source::Smpc);
        }
    }

    /// Fill OREG0..31 with the INTBACK **status** response (MAME
    /// `resolve_intback`): "North-America region, valid RTC, no special
    /// system state". Peripheral data is *not* here — it comes in the
    /// separate continuation phase ([`respond_to_intback_peripheral`]).
    ///
    /// ```text
    ///   OREG0      bit7 STE (RTC valid) | bit6 RESD (reset disabled)
    ///   OREG1..7   RTC, BCD: year-hi, year-lo, weekday<<4|month, day,
    ///              hour, minute, second
    ///   OREG8      cartridge code (none)
    ///   OREG9      area code — 0x04 = North America (BIOS halts on mismatch)
    ///   OREG10     system status 1 — 0x34 (MSHNMI/SYSRES/SOUNDRES)
    ///   OREG11     system status 2 (CDRES) — 0
    ///   OREG12..15 SMEM (SMPC backup memory) — 0 (none stored)
    ///   OREG16..30 undefined — 0xFF (MAME)
    ///   OREG31     0x10 — echo of the issued command (INTBACK)
    /// ```
    fn respond_to_intback_status(&mut self) {
        let now = self.now();
        let s = &mut self.bus.smpc;
        s.oreg[0] = 0x80;
        // OREG1..7 — live RTC, advanced from the (host- or SETTIME-set) base
        // by the emulated seconds elapsed.
        let rtc = s.rtc_oreg(now, CYCLES_PER_SECOND);
        s.oreg[1..8].copy_from_slice(&rtc);
        s.oreg[8] = 0x00; // cartridge
        // OREG9 area code — meaningful: the BIOS halts on a region mismatch.
        s.oreg[9] = s.region;
        // REVIEW(magic): OREG10 = 0x34 (MSHNMI|SYSRES|SOUNDRES, dot-select 0)
        // is taken literally from MAME `resolve_intback`. The bit meanings are
        // spec-defined (SMPC manual), but this exact post-reset value is
        // unverified vs hardware and had no observed boot effect.
        s.oreg[10] = 0x34; // system status 1
        s.oreg[11] = 0x00; // system status 2
        // OREG12..15 — SMPC backup memory (SMEM), as written by SETSMEM.
        s.oreg[12..16].copy_from_slice(&s.smem);
        // OREG16..30 — undefined; MAME writes 0xFF.
        for o in s.oreg.iter_mut().take(31).skip(16) {
            *o = 0xFF;
        }
        s.oreg[31] = 0x10; // issued-command echo
    }

    /// Fill OREG with one INTBACK **peripheral** phase (MAME
    /// `read_saturn_ports`): with no controller connected, both port-status
    /// bytes are `0xF0` (peripheral count 0). OREG31 echoes the command.
    fn respond_to_intback_peripheral(&mut self) {
        let s = &mut self.bus.smpc;
        // Port 1: one directly-connected standard digital pad (ID 0x02 = type
        // 0, 2 data bytes), reporting the active-low inverse of the pressed
        // mask. Port 2: no peripheral.
        let pressed = s.pad1;
        s.oreg[0] = 0xF1; // direct connection, 1 device
        s.oreg[1] = 0x02; // standard digital pad
        s.oreg[2] = !((pressed >> 8) as u8); // first data byte (active low)
        s.oreg[3] = !(pressed as u8) | 0x07; // second data byte (low 3 bits unused)
        s.oreg[4] = 0xF0; // port 2: no peripheral
        s.oreg[31] = 0x10;
    }

    /// Set the port-1 digital-pad state (a `saturn::smpc::pad` pressed mask).
    /// The frontend calls this each frame from the host keyboard.
    pub fn set_pad1(&mut self, pressed: u16) {
        self.bus.smpc.pad1 = pressed;
    }

    /// Set the SMPC area (region) code reported to the BIOS via INTBACK
    /// (see [`crate::smpc::region`]). Must match the BIOS build region or the
    /// BIOS halts.
    pub fn set_region(&mut self, region: u8) {
        self.bus.smpc.region = region;
    }

    /// Seed the RTC from the host clock (seconds since the Unix epoch). The
    /// frontend calls this at startup so the Saturn shows real wall-clock time
    /// like a console with a charged battery; the core otherwise runs from a
    /// deterministic default date.
    pub fn set_rtc_unix(&mut self, unix_secs: u64) {
        let now = self.now();
        self.bus.smpc.set_rtc_unix(unix_secs, now);
    }

    /// Insert a disc image into the CD-block. The drive moves to PAUSE at the
    /// start of the disc; the BIOS/game can then read the TOC, query sessions,
    /// and (in later M7 phases) read sectors and boot the game.
    pub fn insert_disc<S: crate::disc::SectorSource + 'static>(&mut self, source: S) {
        self.bus.cd_block.insert_disc(source);
    }

    /// Eject the current disc (inverse of [`insert_disc`]): the drive returns
    /// to the empty-tray `NODISC` state and flags a disc change. Used by the
    /// frontend's eject menu item.
    pub fn eject_disc(&mut self) {
        self.bus.cd_block.eject();
    }

    /// Whether a disc is currently inserted.
    pub fn has_disc(&self) -> bool {
        self.bus.cd_block.has_disc()
    }

    /// Demo/debug hook: command the CD drive to play `sectors` sectors of CD-DA
    /// from FAD `fad` (see [`crate::cd_block::CdBlock::dbg_play_cdda`]). The
    /// decoded Red Book audio mixes into [`Self::take_audio`] as the machine
    /// runs — drives an audio disc without the BIOS issuing Play.
    pub fn dbg_play_cdda(&mut self, fad: u32, sectors: u32) {
        self.bus.cd_block.dbg_play_cdda(fad, sectors);
    }

    /// Demo/debug: play the disc's first CD-DA track through the live mixed audio
    /// (see [`crate::cd_block::CdBlock::dbg_play_first_audio_track`]). Returns
    /// whether an audio track was found. Wired to the frontend's "play CD audio"
    /// key so an audio disc plays without the BIOS issuing Play.
    pub fn dbg_play_first_audio_track(&mut self) -> bool {
        self.bus.cd_block.dbg_play_first_audio_track()
    }

    /// Plug a cartridge into the rear expansion slot (Extension RAM, backup
    /// RAM, or game ROM). The cart-ID byte at `0x04FF_FFFF` updates so the
    /// BIOS/game probes the right cart; the default slot is empty.
    pub fn insert_cartridge(&mut self, cart: crate::cartridge::Cartridge) {
        self.bus.cartridge = cart;
    }

    /// The internal battery-backed backup RAM as raw 32 KiB of data bytes
    /// (unpacked) — write this to a host file on exit to emulate the battery.
    pub fn internal_backup(&self) -> &[u8] {
        self.bus.backup.bytes()
    }

    /// Restore the internal backup RAM from a persisted image (e.g. loaded on
    /// startup). Length-clamped to the 32 KiB capacity.
    pub fn load_internal_backup(&mut self, bytes: &[u8]) {
        self.bus.backup.load(bytes);
    }

    /// Run the SCU-DSP when host software has started it (PPAF EXF). Run at
    /// the aggregate, not inside the SCU, because the DSP's own DMA moves
    /// data between its data RAM and the A/B-bus — which only reachable from
    /// here. Steps to END (bounded), performing each requested DMA, then
    /// raises the SCU DSP-end interrupt if the program ended with ENDI.
    fn drain_scu_dsp(&mut self) {
        if !self.bus.scu.take_dsp_run() {
            return;
        }
        // Cap protects the host from a microcode bug that never reaches END.
        const DSP_STEP_CAP: u32 = 100_000;
        let mut steps = 0;
        while !self.bus.scu.dsp.stopped() && steps < DSP_STEP_CAP {
            self.bus.scu.dsp.step();
            if let Some(dma) = self.bus.scu.dsp.take_dma() {
                self.exec_dsp_dma(dma);
                self.bus.scu.dsp.regs.flags.t0 = false;
            }
            steps += 1;
        }
        if self.bus.scu.dsp.end_interrupt_pending {
            self.bus.scu.dsp.end_interrupt_pending = false;
            self.bus.scu.raise(crate::scu::Source::DspEnd);
        }
    }

    /// Forward a finished VDP1 plot to the SCU as the sprite-draw-end
    /// interrupt. The plotter runs synchronously inside the PTMR bus
    /// write and flags completion; we drain it here (drain-at-aggregate),
    /// matching how SMPC/SCU side effects are surfaced.
    fn drain_vdp1(&mut self) {
        if self.bus.vdp1.take_draw_end() {
            self.bus.scu.raise(crate::scu::Source::SpriteDrawEnd);
            // Sprite-draw-end is SCU DMA start factor 6.
            self.bus.scu.trigger_dma_factor(6);
        }
    }

    /// Forward the SCSP's main-CPU sound interrupt (e.g. timer A via
    /// `MCIPD`/`MCIEB`) to the SCU `SoundRequest` source. Level-triggered:
    /// stays raised while the SCSP holds it, until software clears `MCIPD`.
    fn drain_scsp(&mut self) {
        if self.bus.scsp.take_main_interrupt() {
            self.bus.scu.raise(crate::scu::Source::SoundRequest);
            // Sound-request is SCU DMA start factor 5.
            self.bus.scu.trigger_dma_factor(5);
        }
    }

    /// Perform a DSP-requested DMA over the system bus. Transfers `size`
    /// 32-bit words between the DSP data-RAM bank (at its CT pointer) and the
    /// A/B-bus addressed by RA0 (in) / WA0 (out), incrementing by `add` bytes
    /// per word; RA0/WA0 are written back unless the request held them.
    fn exec_dsp_dma(&mut self, dma: scu_dsp::DmaRequest) {
        let bank = (dma.dsp_bank & 3) as usize;
        let ct = self.bus.scu.dsp.regs.ct[bank];
        // WA0/RA0 hold full SCU-bus addresses that include the SH-2 cache-through
        // region bit (e.g. `0x25A5_0000` = sound RAM `0x05A5_0000 | 0x2000_0000`).
        // `SaturnBus` only maps the physical `0x05xx_xxxx` regions, so strip the
        // high bits to the 27-bit A/B-bus space before the access — otherwise the
        // read returns open bus (0) and the write is dropped. The sibling
        // [`scu_transfer`] strips the same region bits and additionally forces
        // longword alignment (`& 0x07FF_FFFC`); here `wa0`/`ra0 << 2` are already
        // 4-aligned, so only the region mask is applied. This was the BIOS
        // boot-animation BGM root: the jingle sample is staged into VDP1 VRAM and
        // copied into sound RAM `0x5_0000` by an SCU-DSP DMA, which silently moved
        // zeros without the mask, so the keyed voice played silence. The mask is
        // applied only at the bus access; `wa0`/`ra0` keep their full
        // cache-through addresses for the `update_addr` writeback below.
        const BUS_MASK: u32 = 0x07FF_FFFF;
        if dma.from_dsp {
            let mut dst = self.bus.scu.dsp.regs.wa0 << 2;
            for i in 0..dma.size {
                let idx = (ct.wrapping_add(i as u8) & 0x3F) as usize;
                let word = self.bus.scu.dsp.data_ram[bank][idx];
                self.bus.write32(dst & BUS_MASK, word, AccessKind::Dma);
                dst = dst.wrapping_add(dma.add);
            }
            if dma.update_addr {
                let wa0 = &mut self.bus.scu.dsp.regs.wa0;
                *wa0 = wa0.wrapping_add(dma.size * (dma.add >> 2));
            }
        } else {
            let mut src = self.bus.scu.dsp.regs.ra0 << 2;
            for i in 0..dma.size {
                let (word, _) = self.bus.read32(src & BUS_MASK, AccessKind::Dma);
                let idx = (ct.wrapping_add(i as u8) & 0x3F) as usize;
                self.bus.scu.dsp.data_ram[bank][idx] = word;
                src = src.wrapping_add(dma.add);
            }
            if dma.update_addr {
                let ra0 = &mut self.bus.scu.dsp.regs.ra0;
                *ra0 = ra0.wrapping_add(dma.size * (dma.add >> 2));
            }
        }
    }

    /// Apply pending inter-CPU FRT input-capture (FTI) triggers (`SaturnBus`
    /// flags set by a 16-bit write to the slave/master FTI region): pulse the
    /// target core's FRT input capture, setting `FTCSR.ICF`. This is how a
    /// Saturn CPU wakes the other — e.g. VF2's master writes the slave FTI
    /// region to release its slave's `ICF`-polling dispatch loop. The target
    /// usually polls `ICF` with interrupts masked, so the input-capture
    /// interrupt itself need not be delivered.
    fn drain_input_capture(&mut self) {
        if std::mem::take(&mut self.bus.slave_input_capture) {
            self.scheduler
                .entity_mut(self.slave_id)
                .sh2_mut()
                .cpu
                .onchip
                .frt
                .input_capture();
        }
        if std::mem::take(&mut self.bus.master_input_capture) {
            self.scheduler
                .entity_mut(self.master_id)
                .sh2_mut()
                .cpu
                .onchip
                .frt
                .input_capture();
        }
    }

    /// Global cycle as tracked by the scheduler.
    pub fn now(&self) -> u64 {
        self.scheduler.now()
    }

    /// Run one NTSC frame (≈476 932 SH-2 cycles at 60 Hz) and produce
    /// the rendered framebuffer. The VDP2 raster registers (VCNT /
    /// TVSTAT) and the VBlank-IN interrupt are maintained by
    /// [`Self::update_video_timing`] from the run loop, so this just
    /// runs the active region, snapshots the frame at the active→VBLANK
    /// boundary, and runs the VBLANK region.
    ///
    /// Writes RGBA8888 into `out`, which must be at least
    /// [`crate::vdp2::FRAMEBUFFER_BYTES`] bytes. Returns the active display size
    /// `(width, height)` from TVMD (320/352/640/704 × 224/240/256[×2]); the
    /// pixels are packed tightly with row stride = `width`, so the caller uploads
    /// `width × height` with a `width × 4` pitch.
    /// Advance one NTSC frame of emulation **without rendering** — the compute
    /// half of [`Self::run_frame`]. The frontend's render-pipeline worker
    /// composites the frame on another core from a cloned VDP snapshot, so the
    /// main thread only needs to advance the machine here. Identical emulation
    /// to `run_frame` (rendering is observe-only); the displayed pixels are
    /// produced by `render_frame` elsewhere, bit-for-bit.
    pub fn advance_frame(&mut self) {
        self.run_for(CYCLES_PER_FRAME);
    }

    pub fn run_frame(&mut self, out: &mut [u8]) -> (usize, usize) {
        // Split at the EXACT VBlank-IN edge (`VBLANK_IN_CYCLE`), NOT
        // `ACTIVE_LINES * CYCLES_PER_LINE`. The latter is ~194 cycles short
        // (per-line integer truncation — the very drift `VBLANK_IN_CYCLE`'s
        // doc-comment warns about), so it forces a drain at a point that is
        // NOT an event-clamp edge, forwarding SCU interrupts ~194 cycles early
        // and diverging master-SH-2 execution from `run_for(CYCLES_PER_FRAME)`.
        // That divergence sent VF2 down a different CD-read path that stalled.
        // Splitting at the VBlank-IN edge (which `run_for` clamps to anyway)
        // makes `run_frame` == `run_for(full frame)` + a read-only render at
        // the active→VBLANK boundary.
        // Advance the WHOLE frame in one `run_for`, then render. Do NOT split
        // into run_for(active)+run_for(vblank): `run_for`'s `SMPC_POLL_QUANTUM`
        // batch grid (hence its drain/interrupt-forwarding points) is anchored
        // to each call's *start*, so a split re-anchors the grid and forwards
        // SCU interrupts at different cycles than one continuous `run_for` —
        // diverging master-SH-2 execution. That divergence sent VF2 down a
        // different CD-read path that dead-ended (GetSectorData over-request →
        // no DRDY). A single `run_for(CYCLES_PER_FRAME)` makes `run_frame`
        // bit-identical to the headless `run_for` path (verified via sdbg).
        // The framebuffer is snapshotted at the frame boundary; the VDP state
        // a game commits at VBlank is what it intends to display.
        self.run_for(CYCLES_PER_FRAME);
        crate::vdp2::render_frame(&self.bus.vdp2, Some(self.bus.vdp1.display_fb()), out)
    }

    /// Take the SCSP's generated audio for this period (interleaved L,R at
    /// 44.1 kHz). The frontend queues it to the audio device each frame.
    pub fn take_audio(&mut self) -> Vec<i16> {
        let mut samples = self.bus.scsp.take_audio();
        // Mix in any CD-DA (Red Book) audio the CD-block decoded this span — at
        // the aggregate, so neither chip borrows the other. Both streams are
        // interleaved 16-bit stereo at 44.1 kHz, so they line up frame-for-frame
        // (the CD FIFO absorbs the 75 Hz sector granularity). CD audio mixes at
        // full level for now; SCSP CD-input level/pan fidelity is a refinement.
        if !samples.is_empty() {
            let cd = self.bus.cd_block.take_cd_audio_buffered(samples.len());
            for (out, c) in samples.iter_mut().zip(cd) {
                *out = (*out as i32 + c as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
        }
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CYCLES_PER_FRAME, SH2_CLOCK_HZ, Saturn, dma_count, hblank_active, intback_busy_us,
        scu_dma_bus, scu_dma_illegal, us_to_cycles,
    };

    #[test]
    fn us_to_cycles_uses_the_full_precision_master_clock() {
        // 1 s of µs → ~one second of master cycles (28.636360 MHz).
        assert_eq!(us_to_cycles(1_000_000), SH2_CLOCK_HZ);
        // 261 µs (a status INTBACK) → 261 × clock / 1e6.
        assert_eq!(us_to_cycles(261), 261 * SH2_CLOCK_HZ / 1_000_000);
    }

    #[test]
    fn intback_busy_time_matches_the_mednafen_smpc_clock_model() {
        // No status, no peripheral → just the 92-clock dispatch (÷4 → 23 µs).
        assert_eq!(intback_busy_us(0, 0), 23);
        // Status phase requested (IREG0 low nibble) → (92 + 952)/4 = 261 µs.
        assert_eq!(intback_busy_us(0x01, 0), 261);
        // Peripheral phase (IREG1 bit 3) adds the +700 µs lump.
        assert_eq!(intback_busy_us(0x01, 0x08), 261 + 700);
        assert_eq!(intback_busy_us(0x00, 0x08), 23 + 700);
    }

    #[test]
    fn dma_count_zero_means_the_channel_maximum() {
        // A non-zero programmed count passes through unchanged.
        assert_eq!(dma_count(0, 0x40), 0x40);
        assert_eq!(dma_count(1, 0x10), 0x10);
        // 0 → channel max: 1 MiB for level 0, 4 KiB for levels 1/2.
        assert_eq!(dma_count(0, 0), 0x0010_0000);
        assert_eq!(dma_count(1, 0), 0x0000_1000);
        assert_eq!(dma_count(2, 0), 0x0000_1000);
    }

    #[test]
    fn scu_dma_bus_classifies_the_three_buses() {
        assert_eq!(scu_dma_bus(0x0200_0000), Some(0)); // cartridge (A-bus)
        assert_eq!(scu_dma_bus(0x0589_0000), Some(0)); // CD-block (A-bus)
        assert_eq!(scu_dma_bus(0x05E0_0000), Some(1)); // VDP2 VRAM (B-bus)
        assert_eq!(scu_dma_bus(0x05A0_0000), Some(1)); // SCSP (B-bus)
        assert_eq!(scu_dma_bus(0x0600_0000), Some(2)); // High Work RAM (C-bus)
        assert_eq!(scu_dma_bus(0x0020_0000), None); // Low Work RAM — not DMA-reachable
        assert_eq!(scu_dma_bus(0x0590_0000), None); // unmapped gap
    }

    #[test]
    fn scu_dma_illegal_flags_same_bus_and_unmapped() {
        // Cross-bus transfers (the normal case) are legal.
        assert!(!scu_dma_illegal(0x05E0_0000, 0x0600_0000)); // VRAM → HWRAM
        assert!(!scu_dma_illegal(0x0589_0000, 0x0600_0000)); // CD → HWRAM
        // Same-mapped-bus transfers are illegal.
        assert!(scu_dma_illegal(0x05C0_0000, 0x05E0_0000)); // B-bus → B-bus
        assert!(scu_dma_illegal(0x0200_0000, 0x0589_0000)); // A-bus → A-bus
        assert!(scu_dma_illegal(0x0600_0000, 0x0601_0000)); // C-bus → C-bus
        // An unmapped endpoint (Low Work RAM) is permitted, not flagged — our
        // model treats it as ordinary RAM and must not skip the transfer.
        assert!(!scu_dma_illegal(0x0020_0000, 0x0600_0000)); // LWRAM → HWRAM
        assert!(!scu_dma_illegal(0x0020_0000, 0x0020_1000)); // LWRAM → LWRAM
    }

    #[test]
    fn vblank_edge_helpers_partition_the_frame() {
        // From frame start, the next VBlank-IN is exactly VBLANK_IN_CYCLE away,
        // and the next VBlank-OUT is a full frame away.
        assert_eq!(
            Saturn::cycles_to_next_vblank_in(0),
            Saturn::VBLANK_IN_CYCLE
        );
        assert_eq!(Saturn::cycles_to_next_vblank_out(0), CYCLES_PER_FRAME);
        // Just past VBlank-IN, the next one is in the following frame.
        let past = Saturn::VBLANK_IN_CYCLE + 10;
        assert_eq!(
            Saturn::cycles_to_next_vblank_in(past),
            CYCLES_PER_FRAME - past + Saturn::VBLANK_IN_CYCLE
        );
        // VBlank-OUT counts down to the frame boundary.
        assert_eq!(Saturn::cycles_to_next_vblank_out(past), CYCLES_PER_FRAME - past);
    }

    #[test]
    fn hblank_asserts_past_the_active_display_width() {
        // 320-family (HRESO LSB 0): 320 active dots of 427. Using cycles_per_line
        // = total dots makes the boundary land exactly at `active`.
        assert!(!hblank_active(319, 0, 427), "still in active display");
        assert!(
            hblank_active(320, 0, 427),
            "HBLANK at the active-display edge"
        );
        assert!(hblank_active(426, 0, 427), "HBLANK through to line end");
        // 640 hi-res shares the 320 family (HRESO=2, LSB 0).
        assert!(!hblank_active(319, 2, 427));
        assert!(hblank_active(320, 2, 427));
        // 352-family (LSB 1): 352 active of 455.
        assert!(!hblank_active(351, 1, 455), "still active in 352 mode");
        assert!(hblank_active(352, 1, 455), "HBLANK at the 352-mode edge");
        // 704 hi-res shares the 352 family (HRESO=3, LSB 1).
        assert!(hblank_active(352, 3, 455));
    }
}
