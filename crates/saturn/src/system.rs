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
/// REVIEW(magic): these timings are reference-derived (MAME `smpc.cpp`), NOT
/// from the SMPC datasheet — base 8 µs, +8 if status requested (`IREG0 != 0`),
/// +700 if peripheral requested (`IREG1 & 8`). MAME's own source marks the
/// 700 µs "TODO: is timing correct?", so it's a guess. The BIOS only needs SF
/// to stay busy long enough that its poll loop sees "busy" at least once; the
/// exact duration is unverified against hardware. (Was a fixed ~250 µs,
/// Yabause-style.)
fn intback_busy_us(ireg0: u8, ireg1: u8) -> u64 {
    let mut us = 8;
    if ireg0 != 0 {
        us += 8;
    }
    if ireg1 & 0x08 != 0 {
        us += 700;
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

/// Sub-frame granularity at which the CD-block periodic-firmware timer
/// ticks. One scanline matches the reference (Yabause drives `Cs2Exec`
/// per scanline); the CD-block's own accumulator carries the remainder, so
/// this sets the *phase resolution* of the periodic report, not its cadence.
const CD_TICK_CYCLES: u64 = CYCLES_PER_LINE;

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
}

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
        scheduler.entity_mut(slave_id).sh2_mut().set_is_slave(true);
        let cd_id = scheduler.add(SaturnEntity::CdBlock(CdBlockEntity::new(CD_TICK_CYCLES)));
        Self {
            bus,
            scheduler,
            master_id,
            slave_id,
            cd_id,
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
    /// The slave resumes from whatever PC/SP it was last left at — on
    /// real hardware this is the BIOS reset vector, which is also what
    /// our [`reset`] sets up.
    ///
    /// Resyncs the slave's cycle counter to the current global cycle first:
    /// while halted its `next_deadline` is `u64::MAX` so the scheduler skips
    /// it and its `pipeline.cycles` freezes at the cycle it was halted at. If
    /// it were released without resyncing, the scheduler would see it as far
    /// "behind" the master and run it millions of catch-up cycles in one
    /// batch — time-travelling through stale code and corrupting memory.
    /// (This is what zeroed VF2's freshly-loaded program after its SSHON.)
    pub fn release_slave(&mut self) {
        let now = self.scheduler.now();
        // In cold HLE the slave must be *started* the way the BIOS would on
        // SSH-ON (Yabause `YabauseStartSlave`): jump to the game-provided entry
        // at `[0x06000250]`, with `VBR = 0x06000400` (the BIOS slave vector
        // table) and the slave stack from `[0x060002AC]` (or the BIOS default
        // `0x06001000`). Resuming the slave's stale PC instead made it re-run
        // BIOS init and clobber the loaded game.
        if self.scheduler.entity(self.slave_id).sh2().hle_sys() {
            use sh2::bus::{AccessKind, Bus};
            let entry = self.bus.read32(0x0600_0250, AccessKind::Data).0;
            let alt_sp = self.bus.read32(0x0600_02AC, AccessKind::Data).0;
            let sp = if alt_sp != 0 { alt_sp } else { 0x0600_1000 };
            let slave = self.scheduler.entity_mut(self.slave_id).sh2_mut();
            slave.cpu.hle_jump(entry, sp);
            slave.cpu.regs.vbr = 0x0600_0400;
            if slave.cpu.pipeline.cycles < now {
                slave.cpu.pipeline.cycles = now;
            }
            slave.set_halted(false);
            return;
        }
        let slave = self.scheduler.entity_mut(self.slave_id).sh2_mut();
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

    /// Debug-only: take a breakpoint hit's (R0..R15, code words), if it fired.
    pub fn take_master_bp_hit(&mut self) -> Option<([u32; 16], Vec<u16>)> {
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .take_bp_hit()
    }

    /// Debug-only: arm a full-speed breakpoint on the *slave* SH-2 (M12 — to
    /// catch the slave running a memory-fill over the HLE-booted program).
    pub fn set_slave_bp(&mut self, pc: u32) {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .set_bp(pc);
    }

    /// Debug-only: take the slave breakpoint hit's (R0..R15, code words).
    pub fn take_slave_bp_hit(&mut self) -> Option<([u32; 16], Vec<u16>)> {
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .take_bp_hit()
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
        let cpu = &mut scheduler.entity_mut(*master_id).sh2_mut().cpu;
        // Mirror Sh2Entity::step: publish the current cycle to the bus so
        // time-varying peripheral reads (SMPC SF INTBACK completion) settle
        // at the exact instruction that reads them.
        bus.cycle = cpu.pipeline.cycles;
        cpu.step(bus)
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
        self.drain_scu_dma();
        self.drain_scu_dsp();
        self.drain_vdp1();
        self.drain_scsp();
        self.drain_scu_intc();
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
        // Use the precise frame-derived edge (matches the `run_for` clamp and
        // the reference) rather than the rounded per-line `line >= 224`.
        let vblank = frame_cycle >= Self::VBLANK_IN_CYCLE;
        let mut tvstat = prev & !0x000E; // clear VBLANK | HBLANK | ODD
        if vblank {
            tvstat |= 0x0008; // VBLANK
        }
        // REVIEW(magic): HBLANK as "last ~20% of the scanline" is an
        // invented approximation, not the real H-blank dot count (the VDP2
        // H-blank period is a specific number of dots in the 427-dot line).
        // Harmless unless the BIOS times something off HBLANK; revisit if a
        // divergence points at TVSTAT.HBLANK.
        if line_cycle * 5 >= CYCLES_PER_LINE * 4 {
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
        }

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

    /// Advance global time by at least `cycles` cycles, interleaving
    /// the two CPUs by deadline order and polling SMPC + SCU between
    /// scheduler batches.
    pub fn run_for(&mut self, cycles: u64) {
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let now = self.now();
            let remaining = target - now;
            // Clamp the batch to the next VBlank-IN edge so the interrupt is
            // raised cycle-exactly (`.max(1)` so a batch is never zero when
            // sitting exactly on the edge).
            let batch = remaining
                .min(SMPC_POLL_QUANTUM)
                .min(Self::cycles_to_next_vblank_in(now).max(1));
            self.scheduler.run_for(batch, &mut self.bus);
            self.bus.scsp.run(batch);
            self.update_video_timing();
            self.drain_smpc();
            self.drain_scu_dma();
            self.drain_scu_dsp();
            self.drain_vdp1();
            self.drain_scsp();
            self.drain_scu_intc();
        }
    }

    /// Debug-only: like [`run_for`](Self::run_for), but append the master
    /// SH-2's PC (skipping delay slots, to match reference traces) to `pcs`
    /// as the scheduler steps it. Unlike the master-only single-step tracer,
    /// this runs the *full* scheduler (master + slave + CD-block), so the
    /// master's interrupt phase reflects the real `run_frame` path — needed
    /// to diff the `run_frame` boot park against a reference.
    pub fn run_for_traced(&mut self, cycles: u64, pcs: &mut Vec<u32>) {
        let master_id = self.master_id;
        // Reference traces differ on whether they log branch delay slots:
        // Yabause omits them; MAME logs them. Set PCTRACE_DELAYSLOTS to match
        // a MAME reference trace (which includes the slot PC).
        let log_delay_slots = std::env::var("PCTRACE_DELAYSLOTS").is_ok();
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let now = self.now();
            let remaining = target - now;
            let batch = remaining
                .min(SMPC_POLL_QUANTUM)
                .min(Self::cycles_to_next_vblank_in(now).max(1));
            self.scheduler
                .run_for_traced(batch, &mut self.bus, |entity, id| {
                    if id == master_id {
                        let cpu = &entity.sh2().cpu;
                        if log_delay_slots || !cpu.next_is_delay_slot() {
                            pcs.push(cpu.regs.pc);
                        }
                    }
                });
            self.bus.scsp.run(batch);
            self.update_video_timing();
            self.drain_smpc();
            self.drain_scu_dma();
            self.drain_scu_dsp();
            self.drain_vdp1();
            self.drain_scsp();
            self.drain_scu_intc();
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
                    // INTBACK status phase (MAME `resolve_intback`). Fill the
                    // status OREG and arm the staged-peripheral protocol:
                    // `intback_stage = (IREG1 & 8) >> 3` (1 if peripheral data
                    // was requested), `pmode = IREG0 >> 4`, `SR = 0x40 |
                    // (stage << 5)`. Raise the SMPC interrupt now so the
                    // response is ready the moment SF drops, and keep SF busy
                    // for the request-dependent execution time
                    // (`intback_busy_us`); `settle_intback` clears it on the
                    // exact instruction that reads SMPC past completion.
                    let ireg0 = self.bus.smpc.ireg[0];
                    let ireg1 = self.bus.smpc.ireg[1];
                    self.respond_to_intback_status();
                    let stage = (ireg1 & 0x08) >> 3;
                    self.bus.smpc.intback_stage = stage;
                    self.bus.smpc.pmode = ireg0 >> 4;
                    self.bus.smpc.sr = 0x40 | (stage << 5);
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
                // Remaining commands (clock-change, reset-enable/disable, …)
                // are recognised but have no emulator-side effect yet.
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

    /// Whether the BIOS has engaged the CD-block (issued its first command) —
    /// the clean post-init handoff point for HLE direct boot (see
    /// [`CdBlock::host_engaged`](crate::cd_block::CdBlock::host_engaged)).
    pub fn cd_host_engaged(&self) -> bool {
        self.bus.cd_block.host_engaged()
    }

    /// **HLE direct boot** (ADR-0010): load the disc's 1st-read program into
    /// work RAM and jump the master SH-2 to it, bypassing the BIOS's CD boot
    /// loader. Returns the load address on success, or `None` (a no-op) if
    /// there's no disc / IP.BIN / filesystem to read, so the caller can fall
    /// back to the running BIOS.
    ///
    /// This is the **hybrid** model: it assumes the BIOS has already
    /// initialised the hardware and the `SYS_*` call table (call it once the
    /// BIOS has recognised the disc and is about to drop to its CD player), so
    /// only the final 1st-read load + handoff is high-level-emulated. The game
    /// releases the slave SH-2 itself (via SMPC `SSHON`).
    pub fn hle_boot(&mut self) -> Option<u32> {
        use crate::bus::HIGH_WRAM_BASE;
        use crate::disc::FAD_OFFSET;
        const HWRAM_SIZE: u32 = 0x10_0000;

        // 1st-read file (first root-directory entry): start FAD + byte length.
        let (file_fad, file_len) = self.bus.cd_block.first_read_file()?;
        let file_len = file_len.min(HWRAM_SIZE); // guard against a corrupt size

        // IP.BIN header (FAD 150): 1st-read load address (+0xF0) and master
        // stack (+0xE8; 0 → the conventional BIOS default 0x0600_2000).
        let mut ip = [0u8; 2048];
        if !self.bus.cd_block.disc()?.read_sector(FAD_OFFSET, &mut ip) {
            return None;
        }
        let be = |o: usize| u32::from_be_bytes([ip[o], ip[o + 1], ip[o + 2], ip[o + 3]]);
        let load_addr = be(0xF0);
        if !(HIGH_WRAM_BASE..HIGH_WRAM_BASE + HWRAM_SIZE).contains(&load_addr) {
            return None; // only high-work-RAM load addresses are supported
        }
        let stack = be(0xE8);
        let sp = if stack != 0 { stack } else { 0x0600_2000 };

        // Read the 1st-read file's sectors into an owned buffer (so the disc
        // borrow is released before we write work RAM).
        let nsec = (file_len as usize).div_ceil(2048);
        let mut data = vec![0u8; nsec * 2048];
        {
            let disc = self.bus.cd_block.disc()?;
            let mut sec = [0u8; 2048];
            for i in 0..nsec {
                if disc.read_sector(file_fad + i as u32, &mut sec) {
                    data[i * 2048..(i + 1) * 2048].copy_from_slice(&sec);
                }
            }
        }

        // Copy into high work RAM at the load address.
        let off = (load_addr - HIGH_WRAM_BASE) as usize;
        let ram = self.bus.high_wram.as_mut_slice();
        let end = (off + data.len()).min(ram.len());
        ram[off..end].copy_from_slice(&data[..end - off]);

        // Hand the master SH-2 control of the freshly loaded program.
        self.master_mut().hle_jump(load_addr, sp);
        Some(load_addr)
    }

    /// **Cold HLE direct boot** (ADR-0011): [`hle_boot`](Self::hle_boot) plus the
    /// HLE BIOS system-call environment — install our SYS call table into work
    /// RAM (overriding whatever the BIOS left there) and enable the master's
    /// SYS-dispatch hook, so the game's BIOS system calls run our
    /// [`bios_hle`](crate::bios_hle) implementations instead of the BIOS's
    /// fatal/unimplemented stubs. Returns the 1st-read load address, or `None`
    /// (no-op) if there's nothing bootable.
    pub fn cold_hle_boot(&mut self) -> Option<u32> {
        let addr = self.hle_boot()?;
        crate::bios_hle::install_call_table(&mut self.bus);
        // Enable the SYS-call hook on BOTH cores: the SYS table lives in shared
        // work RAM, so the slave's BIOS init also `JSR`s our installed entry
        // addresses. Without the hook on the slave it would run real BIOS ROM at
        // those addresses (garbage) and fall into the fatal handler.
        self.scheduler
            .entity_mut(self.master_id)
            .sh2_mut()
            .set_hle_sys(true);
        self.scheduler
            .entity_mut(self.slave_id)
            .sh2_mut()
            .set_hle_sys(true);
        Some(addr)
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

    /// Run any DMA transfers that the SCU queued during the last
    /// scheduler batch. For M3 each transfer is synchronous: we move
    /// the full byte count in 32-bit chunks (plus a byte tail) via
    /// `self.bus`, then write back the post-transfer state. Cycle-
    /// stealing accuracy and start factors other than "manual" are
    /// out of M3 scope; whichever later milestone surfaces a game
    /// that needs them will refine this.
    fn drain_scu_dma(&mut self) {
        while let Some(req) = self.bus.scu.take_pending_dma() {
            // The SCU can't read its own bus segment (the BIOS A-bus); a DMA
            // sourced from there is illegal and transfers nothing.
            let bios_src = |a: u32| a & 0x07F0_0000 == 0;
            let (final_src, final_dst) = if req.indirect {
                // Indirect mode: `dst` points at a table of {size, dst, src}
                // longword triplets; the last entry has bit 31 of its source
                // word set. Each triplet is its own transfer.
                let mut index = req.dst;
                // The indirect table is terminated by bit 31 of a triplet's
                // source word. Cap the walk so a stray/zeroed table (no
                // terminator) can't spin the host forever — a real list is far
                // shorter than this.
                const MAX_INDIRECT_TRIPLETS: u32 = 0x1_0000;
                let mut walked = 0u32;
                loop {
                    let (size, _) = self.bus.read32(index, AccessKind::Dma);
                    let (idst, _) = self.bus.read32(index.wrapping_add(4), AccessKind::Dma);
                    let (isrc_raw, _) = self.bus.read32(index.wrapping_add(8), AccessKind::Dma);
                    let last = isrc_raw & 0x8000_0000 != 0;
                    let isrc = isrc_raw & 0x07FF_FFFF;
                    let idst = idst & 0x07FF_FFFF;
                    let count = self.dma_count(req.channel, size);
                    if !bios_src(isrc) {
                        self.scu_transfer(isrc, idst, count, req.src_add, req.dst_add);
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
            } else {
                let count = self.dma_count(req.channel, req.bytes);
                self.scu_transfer(req.src, req.dst, count, req.src_add, req.dst_add)
            };
            self.bus.scu.finish_dma(req.channel, final_src, final_dst);
        }
    }

    /// SCU DMA byte count: a programmed 0 means the channel's maximum
    /// (1 MiB for level 0, 4 KiB for levels 1/2), per the SCU manual.
    fn dma_count(&self, channel: usize, programmed: u32) -> u32 {
        if programmed != 0 {
            programmed
        } else if channel == 0 {
            0x0010_0000
        } else {
            0x0000_1000
        }
    }

    /// One SCU DMA block transfer over the B-bus 16-bit data path, honouring
    /// the `D*AD` source/destination strides. The source is read as 32-bit
    /// words and split into two big-endian 16-bit halves; each half is written
    /// to the destination, which advances by `dst_add` (Work RAM H forces a
    /// 2-byte step). The source advances by `src_add` once per 32-bit word
    /// (`src_add` 0 = a fixed source, e.g. a FIFO register). Returns the
    /// post-transfer `(src, dst)`. The rare unaligned-source data-rotation
    /// case is not modelled.
    fn scu_transfer(
        &mut self,
        mut src: u32,
        mut dst: u32,
        bytes: u32,
        src_add: u32,
        dst_add: u32,
    ) -> (u32, u32) {
        let mut src_shift = ((src & 2) >> 1) ^ 1;
        let mut i = 0u32;
        while i < bytes {
            let (word, _) = self.bus.read32(src & 0x07FF_FFFC, AccessKind::Dma);
            let half = (word >> (src_shift * 16)) as u16;
            self.bus.write16(dst & 0x07FF_FFFE, half, AccessKind::Dma);
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
        if dma.from_dsp {
            let mut dst = self.bus.scu.dsp.regs.wa0 << 2;
            for i in 0..dma.size {
                let idx = (ct.wrapping_add(i as u8) & 0x3F) as usize;
                let word = self.bus.scu.dsp.data_ram[bank][idx];
                self.bus.write32(dst, word, AccessKind::Dma);
                dst = dst.wrapping_add(dma.add);
            }
            if dma.update_addr {
                let wa0 = &mut self.bus.scu.dsp.regs.wa0;
                *wa0 = wa0.wrapping_add(dma.size * (dma.add >> 2));
            }
        } else {
            let mut src = self.bus.scu.dsp.regs.ra0 << 2;
            for i in 0..dma.size {
                let (word, _) = self.bus.read32(src, AccessKind::Dma);
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

    /// Surface any fresh SCU interrupt assertion to the master SH-2.
    /// One source per drain (the highest-priority unmasked one); the
    /// `fresh_assertions` bit clears as part of `take_pending_interrupt`
    /// so we don't re-fire the same source every batch while the SH-2
    /// is still handling it. New raises after the SH-2 acks will fire
    /// the source again because the SCU's `raise()` re-sets the bit.
    fn drain_scu_intc(&mut self) {
        let imask = self.master().regs.sr.imask();
        if let Some((source, level)) = self.bus.scu.take_pending_interrupt(imask) {
            // The SCU presents a fixed vector (0x40 + index) per source
            // during interrupt-acknowledge — not the SH-2 auto-vector
            // 64+level. Vectoring VBlank-IN to 64+15=0x4F would run the
            // generic stub handler instead of the real one at 0x40.
            self.master_mut()
                .onchip
                .intc
                .raise_external(level, source.vector());
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
    /// Writes into `out`, which must be exactly
    /// [`crate::vdp2::FRAMEBUFFER_BYTES`] bytes (RGBA8888 320×224).
    pub fn run_frame(&mut self, out: &mut [u8]) {
        const ACTIVE_CYCLES: u64 = ACTIVE_LINES * CYCLES_PER_LINE;
        const VBLANK_CYCLES: u64 = CYCLES_PER_FRAME - ACTIVE_CYCLES;

        self.run_for(ACTIVE_CYCLES);
        crate::vdp2::render_frame(&self.bus.vdp2, Some(self.bus.vdp1.display_fb()), out);
        self.run_for(VBLANK_CYCLES);
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
            let cd = self.bus.cd_block.take_cd_audio(samples.len());
            for (out, c) in samples.iter_mut().zip(cd) {
                *out = (*out as i32 + c as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
        }
        samples
    }
}
