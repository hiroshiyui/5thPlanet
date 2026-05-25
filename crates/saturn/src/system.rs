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
use crate::scheduler::{EntityId, Scheduler, Sh2Entity};
use crate::smpc::Command as SmpcCommand;

/// Max scheduler cycles between SMPC-pending checks. Small enough that
/// BIOS code polling SF after a command doesn't spin for a meaningful
/// fraction of a frame; large enough that the inner-loop overhead of
/// poking SMPC every tick isn't paid 28 million times per second.
const SMPC_POLL_QUANTUM: u64 = 256;

/// INTBACK status-command execution time. Hardware (and Yabause) keep
/// SF busy ~250 µs before the response is ready; at the 28.6 MHz SH-2
/// master clock that's ≈7150 cycles. The BIOS polls SF in a wait loop
/// and only proceeds correctly once it clears at the right time.
const INTBACK_EXEC_CYCLES: u64 = 7150;

/// NTSC raster timing at the 28.6 MHz SH-2 master clock. One 60 Hz
/// frame is 263 lines; the BIOS polls VDP2 `VCNT`/`TVSTAT` to track the
/// raster, so these registers must move with the global cycle.
const CYCLES_PER_FRAME: u64 = 476_932;
const LINES_PER_FRAME: u64 = 263;
const ACTIVE_LINES: u64 = 224;
const CYCLES_PER_LINE: u64 = CYCLES_PER_FRAME / LINES_PER_FRAME; // ≈1813

/// One emulated SEGA Saturn — a Saturn-shaped memory map populated with
/// a caller-supplied BIOS image, plus master and slave SH-2 cores wired
/// into a shared event-driven scheduler.
pub struct Saturn {
    pub bus: SaturnBus,
    pub scheduler: Scheduler<Sh2Entity>,
    master_id: EntityId,
    slave_id: EntityId,
}

impl Saturn {
    /// Construct with a real BIOS image. Both CPUs start with default
    /// register state; call [`reset`] to load PC/SP from the BIOS reset
    /// vector before stepping.
    pub fn new(bios: Vec<u8>) -> Self {
        let bus = SaturnBus::new(bios);
        let mut scheduler = Scheduler::new();
        let master_id = scheduler.add(Sh2Entity::new(Cpu::new()));
        let slave_id = scheduler.add(Sh2Entity::new(Cpu::new()));
        Self {
            bus,
            scheduler,
            master_id,
            slave_id,
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
        } = self;
        scheduler.entity_mut(*master_id).cpu.reset(bus);
        scheduler.entity_mut(*slave_id).cpu.reset(bus);
        // Real Saturn power-on: slave is held in reset until the BIOS
        // sends SMPC SETSL. Mirror that here.
        scheduler.entity_mut(*slave_id).set_halted(true);
        scheduler.entity_mut(*master_id).set_halted(false);
    }

    /// Halt the slave SH-2. Triggered by SMPC `SSHOFF`.
    pub fn halt_slave(&mut self) {
        self.scheduler.entity_mut(self.slave_id).set_halted(true);
    }

    /// Release the slave SH-2 from halt. Triggered by SMPC `SSHON`.
    /// The slave resumes from whatever PC/SP it was last left at — on
    /// real hardware this is the BIOS reset vector, which is also what
    /// our [`reset`] sets up.
    pub fn release_slave(&mut self) {
        self.scheduler.entity_mut(self.slave_id).set_halted(false);
    }

    pub fn slave_is_halted(&self) -> bool {
        self.scheduler.entity(self.slave_id).is_halted()
    }

    pub fn master(&self) -> &Cpu {
        &self.scheduler.entity(self.master_id).cpu
    }
    pub fn master_mut(&mut self) -> &mut Cpu {
        &mut self.scheduler.entity_mut(self.master_id).cpu
    }
    pub fn slave(&self) -> &Cpu {
        &self.scheduler.entity(self.slave_id).cpu
    }
    pub fn slave_mut(&mut self) -> &mut Cpu {
        &mut self.scheduler.entity_mut(self.slave_id).cpu
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
        let cpu = &mut scheduler.entity_mut(*master_id).cpu;
        // Mirror Sh2Entity::step: publish the current cycle to the bus so
        // time-varying peripheral reads (SMPC SF INTBACK completion) settle
        // at the exact instruction that reads them.
        bus.cycle = cpu.pipeline.cycles;
        cpu.step(bus)
    }

    /// Debug-only: run the SMPC/SCU drains once (the same set `run_for`
    /// performs between scheduler batches).
    pub fn debug_drain(&mut self) {
        self.update_video_timing();
        self.drain_smpc();
        self.drain_scu_dma();
        self.drain_scu_intc();
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
        let vblank = line >= ACTIVE_LINES;
        let mut tvstat = prev & !0x000E; // clear VBLANK | HBLANK | ODD
        if vblank {
            tvstat |= 0x0008; // VBLANK
        }
        if line_cycle * 5 >= CYCLES_PER_LINE * 4 {
            tvstat |= 0x0004; // HBLANK — approx: last ~20% of each line
        }
        if frame & 1 == 1 {
            tvstat |= 0x0002; // ODD field
        }
        self.bus.vdp2.regs.write16(0x00A, line as u16); // VCNT
        self.bus.vdp2.regs.write16(0x004, tvstat);

        // Raise VBlank-IN once, on the transition into the VBLANK region,
        // and let the CD-block emit its once-per-frame periodic status
        // report on the same edge (the reference drives the CD-block once
        // per frame; frame-locking the report keeps its cadence matched).
        if vblank && (prev & 0x0008) == 0 {
            self.bus.scu.raise(crate::scu::Source::VBlankIn);
            self.bus.cd_block.frame_tick();
        }
    }

    /// Advance global time by at least `cycles` cycles, interleaving
    /// the two CPUs by deadline order and polling SMPC + SCU between
    /// scheduler batches.
    pub fn run_for(&mut self, cycles: u64) {
        let target = self.now().saturating_add(cycles);
        while self.now() < target {
            let remaining = target - self.now();
            let batch = remaining.min(SMPC_POLL_QUANTUM);
            self.scheduler.run_for(batch, &mut self.bus);
            self.update_video_timing();
            self.drain_smpc();
            self.drain_scu_dma();
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
                    // Fill OREG and raise the SMPC interrupt now, so the
                    // response is ready the moment SF drops. But INTBACK
                    // takes ~250 µs to execute on hardware, so keep SF busy
                    // until then — `settle_intback` clears it on the exact
                    // instruction that reads SMPC past the completion cycle.
                    // Clearing SF immediately makes the BIOS's tight SF-poll
                    // (0x1D5A..0x1D64) exit too early and derail (Yabause
                    // reference-diff).
                    self.respond_to_intback();
                    let done_at = self.now().saturating_add(INTBACK_EXEC_CYCLES);
                    self.bus.smpc.intback_complete_at = Some(done_at);
                    continue; // do NOT mark_command_done (SF stays busy)
                }
                // Other commands are recognised but have no
                // emulator-side effect yet (SETTIME / SETSMEM / etc.)
                // — they get real implementations as the corresponding
                // peripherals land in later milestones.
                _ => {}
            }
            self.bus.smpc.mark_command_done();
        }
    }

    /// Populate OREG0..31 with an INTBACK status response describing
    /// "no controller in either port, North-America region, valid RTC".
    /// Then raise the SCU's SMPC interrupt source so a BIOS handler can
    /// read it.
    ///
    /// Layout follows the SMPC manual's INTBACK status-response format:
    ///
    /// ```text
    ///   OREG0      status: bit7 STE (RTC valid), bit6 RESD (reset disabled)
    ///   OREG1..7   RTC, BCD: year-hi, year-lo, weekday<<4|month, day,
    ///              hour, minute, second  (SEVEN bytes — OREG7 is seconds)
    ///   OREG8      cartridge code
    ///   OREG9      area code (region)    — 0x04 = North America
    ///   OREG10..11 system status 1 / 2
    ///   OREG12..   peripheral data (per-port headers)
    /// ```
    ///
    /// Getting OREG9 right matters: the BIOS reads it as the hardware
    /// area code and halts (imask=15 spin) on a region mismatch.
    fn respond_to_intback(&mut self) {
        let s = &mut self.bus.smpc;
        // OREG0 — STE = 1 (RTC valid), RESD = 0 (reset button enabled).
        s.oreg[0] = 0x80;
        // OREG1..7 — BCD RTC, seven bytes. Values arbitrary but BCD-valid;
        // OREG3 packs weekday (bits 7..4) and month (bits 3..0).
        s.oreg[1] = 0x20; // year hi
        s.oreg[2] = 0x26; // year lo
        s.oreg[3] = 0x05; // weekday 0, month 5
        s.oreg[4] = 0x18; // day
        s.oreg[5] = 0x12; // hour
        s.oreg[6] = 0x00; // minute
        s.oreg[7] = 0x00; // second
        // OREG8 — cartridge code (no cartridge).
        s.oreg[8] = 0x00;
        // OREG9 — area code. 0x04 = North America (NTSC), matching the
        // USA BIOS image. Japan = 0x01, Europe PAL = 0x0C.
        s.oreg[9] = 0x04;
        // OREG10..11 — system status 1 / 2. Nominal: no special state.
        s.oreg[10] = 0x00;
        s.oreg[11] = 0x00;
        // OREG12.. — peripheral data. Report no peripheral in either
        // port via the 0xF0 "port empty" header tag.
        s.oreg[12] = 0xF0; // port 1 header: no peripheral
        s.oreg[13] = 0xF0; // port 2 header: no peripheral
        for i in 14..31 {
            s.oreg[i] = 0;
        }
        // OREG31 — end-of-data marker per the manual.
        s.oreg[31] = 0xF0;

        // Surface the SMPC interrupt (maskable, via the SCU). INTBACK
        // signals completion on the maskable MIRQ line, NOT an NMI —
        // verified against the srg320 Saturn HW model (INTBACK uses
        // MIRQ_N; only SRES/NMIREQ/CKCHG assert the master NMI).
        self.bus.scu.raise(crate::scu::Source::Smpc);
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
            let mut src = req.src;
            let mut dst = req.dst;
            let mut remaining = req.bytes;
            while remaining >= 4 {
                let (val, _) = self.bus.read32(src, AccessKind::Dma);
                self.bus.write32(dst, val, AccessKind::Dma);
                src = src.wrapping_add(4);
                dst = dst.wrapping_add(4);
                remaining -= 4;
            }
            while remaining > 0 {
                let (val, _) = self.bus.read8(src, AccessKind::Dma);
                self.bus.write8(dst, val, AccessKind::Dma);
                src = src.wrapping_add(1);
                dst = dst.wrapping_add(1);
                remaining -= 1;
            }
            self.bus.scu.finish_dma(req.channel, src, dst);
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
        crate::vdp2::render_frame(&self.bus.vdp2, out);
        self.run_for(VBLANK_CYCLES);
    }
}
