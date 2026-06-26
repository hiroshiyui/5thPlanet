//! System Control Unit (SCU) — Saturn's bus bridge and DMA engine.
//!
//! Memory-mapped at `0x05FE_0000..=0x05FE_00FF` (cache-through alias of
//! `0x25FE_0000`, which is the canonical address). Holds three DMA
//! channels, three timers, an interrupt-mask / status pair, A-bus
//! configuration, a DSP-control window (task #17 wires it up), and a
//! read-only version register.
//!
//! Register map (offsets from `SCU_BASE`):
//!
//! ```text
//!   0x00..0x14   DMA channel 0 — D0R / D0W / D0C / D0AD / D0EN / D0MD
//!   0x20..0x34   DMA channel 1
//!   0x40..0x54   DMA channel 2
//!   0x60         DSTP — DMA force stop
//!   0x7C         DSTA — DMA status
//!   0x80..0x8C   DSP control ports (PPAF / PPD / PDA / PDD)
//!   0x90         T0C  — timer 0 compare
//!   0x94         T1S  — timer 1 set value
//!   0x98         T1MD — timer 1 mode
//!   0xA0         IMS  — interrupt mask (1 = masked)
//!   0xA4         IST  — interrupt status (write-0-clear)
//!   0xA8         AIACK — A-bus interrupt acknowledge
//!   0xB0         ASR0 — A-bus set 0
//!   0xB4         ASR1 — A-bus set 1
//!   0xB8         AREF — A-bus refresh
//!   0xC4         RSEL — SCU SDRAM/register select
//!   0xC8         VER  — version, reads 0x0000_0004 (read-only)
//! ```
//!
//! DMA: a channel armed via `D*EN` (bit 8) transfers when its start factor
//! (`D*MD` bits 2..0) fires — the manual factor (7) on the `D*EN` go bit, or a
//! hardware event (VBlank-IN/-OUT, HBlank, timers, sound-request,
//! sprite-draw-end) routed through [`Scu::trigger_dma_factor`]. Both direct
//! and indirect (table-driven) modes are supported, honouring the `D*AD`
//! source/destination strides and the `D*MD` RUP/WUP address-update bits.
//! [`Scu::take_pending_dma`] hands a [`DmaRequest`] to the Saturn aggregate's
//! `run_for` loop, which performs the byte movement through `SaturnBus` (the
//! SCU can't reach the bus itself). Cycle-stealing bus timing remains a
//! refinement — the transfer is a synchronous block.

pub const SCU_BASE: u32 = 0x05FE_0000;
pub const SCU_END: u32 = 0x05FE_00FF;

const NUM_CHANNELS: usize = 3;
const CHANNEL_STRIDE: u32 = 0x20;
/// DGO ("DMA Go") enable bit in D*EN.
const DGO_BIT: u32 = 1 << 8;

/// Sources the SCU's interrupt aggregator forwards to the master
/// SH-2 as an `External(level)` IRL assertion. Hardware-fixed priority
/// (no programmability for these in M3) and IST bit assignment per
/// the SCU manual.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Source {
    VBlankIn = 0,
    VBlankOut = 1,
    HBlankIn = 2,
    Timer0 = 3,
    Timer1 = 4,
    DspEnd = 5,
    SoundRequest = 6,
    Smpc = 7,
    Pad = 8,
    Level2DmaEnd = 9,
    Level1DmaEnd = 10,
    Level0DmaEnd = 11,
    DmaIllegal = 12,
    SpriteDrawEnd = 13,
    /// CD-block host-interface interrupt (SCU **external** interrupt 0 — bit 16,
    /// vector 0x50 [= `0x40 + 16`], level 7; `external_tab[0]` in Mednafen
    /// `scu.inc`). The CD-block asserts it as a *level* `(CD HIRQ & HIRQ_Mask)
    /// != 0` ([`set_cd_int`](Scu::set_cd_int)); VF2's GFS file library is driven
    /// by it (its handler advances the file-read state machine — without it the
    /// intro loops re-reading the same files forever). Masked by IMS **bit 15**
    /// (the SCU sign-extends the 16-bit IMS so bit 15 gates all external
    /// interrupts) and re-armed after firing via the AIACK write (0xA8 bit 0),
    /// matching Mednafen's `ABusIProhibit` latch.
    Cd = 16,
}

impl Source {
    /// Bit position within `ist` / `ims`.
    pub const fn bit(self) -> u32 {
        self as u32
    }

    /// SH-2 exception vector number the SCU presents for this source
    /// during the interrupt-acknowledge cycle. Fixed at `0x40 + index`
    /// per the SCU manual's interrupt table — independent of priority
    /// level (e.g. SMPC and PAD share level 8 but use 0x47 / 0x48).
    pub const fn vector(self) -> u8 {
        0x40 + self as u8
    }

    /// Hardware-fixed priority level (1..=15) asserted on the SH-2's
    /// IRL lines when this source fires.
    pub const fn priority(self) -> u8 {
        match self {
            Source::VBlankIn => 15,
            Source::VBlankOut => 14,
            Source::HBlankIn => 13,
            Source::Timer0 => 12,
            Source::Timer1 => 11,
            Source::DspEnd => 10,
            Source::SoundRequest => 9,
            Source::Smpc | Source::Pad => 8,
            Source::Level2DmaEnd | Source::Level1DmaEnd => 6,
            Source::Level0DmaEnd => 5,
            Source::DmaIllegal => 3,
            Source::SpriteDrawEnd => 2,
            Source::Cd => 7, // external_tab[0] (Mednafen scu.inc)
        }
    }
}

/// Scan order used by `take_pending_interrupt`. Highest priority first,
/// so the first match in the scan is the winner.
const ALL_SOURCES: &[Source] = &[
    Source::VBlankIn,
    Source::VBlankOut,
    Source::HBlankIn,
    Source::Timer0,
    Source::Timer1,
    Source::DspEnd,
    Source::SoundRequest,
    Source::Smpc,
    Source::Pad,
    Source::Cd, // level 7 — between Pad (8) and the DMA-end sources (6)
    Source::Level2DmaEnd,
    Source::Level1DmaEnd,
    Source::Level0DmaEnd,
    Source::DmaIllegal,
    Source::SpriteDrawEnd,
];

/// One SCU DMA channel's register set (`D*R/W/C/AD/EN/MD`). The three levels
/// differ only in transfer-count width; the accessors below decode the
/// mode/add fields (direct vs indirect, address-update, increments).
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DmaChannel {
    pub read_addr: u32,
    pub write_addr: u32,
    pub transfer_count: u32,
    pub add_value: u32,
    pub enable: u32,
    pub mode: u32,
    /// Set when the channel has been triggered (manual go or a start-factor
    /// event). Picked up by [`Scu::take_pending_dma`] and cleared.
    triggered: bool,
}

impl DmaChannel {
    /// `D*EN` bit 8 — the channel is armed (enabled). A start factor / the go
    /// bit only triggers a transfer while this is set.
    fn armed(&self) -> bool {
        self.enable & 0x100 != 0
    }
    /// Start factor (`D*MD` bits 2..0): 0 VBlank-IN, 1 VBlank-OUT, 2 HBlank-IN,
    /// 3 Timer0, 4 Timer1, 5 Sound-Req, 6 Sprite-draw-end, 7 manual (the go
    /// bit in `D*EN`).
    fn start_factor(&self) -> u8 {
        (self.mode & 0x7) as u8
    }
    /// `D*MD` bit 24 — indirect (table-driven) vs direct transfer.
    fn indirect(&self) -> bool {
        self.mode & (1 << 24) != 0
    }
    /// `D*MD` bit 16 (read-address update) — whether `D*R` advances past the
    /// transferred region (else it keeps its programmed value).
    fn read_update(&self) -> bool {
        self.mode & (1 << 16) != 0
    }
    /// `D*MD` bit 8 (write-address update).
    fn write_update(&self) -> bool {
        self.mode & (1 << 8) != 0
    }
    /// Read-address increment per source word: `D*AD` bit 8 → 4 bytes, else 0
    /// (fixed source, e.g. a FIFO register).
    fn src_add(&self) -> u32 {
        if self.add_value & 0x100 != 0 { 4 } else { 0 }
    }
    /// Write-address increment: `2^(D*AD & 7)` bytes, with the `0` code
    /// meaning a fixed destination.
    fn dst_add(&self) -> u32 {
        let a = 1u32 << (self.add_value & 0x7);
        if a == 1 { 0 } else { a }
    }
}

/// The System Control Unit: three DMA channels, the interrupt mask/status
/// (IMS/IST) and priority aggregation toward the master SH-2, the dual timers,
/// the A-bus refresh/wait registers, and the embedded [`scu_dsp::Dsp`]. The
/// transfers and the DSP run at the Saturn aggregate, since the SCU can't reach
/// the bus from inside it.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Scu {
    pub channels: [DmaChannel; NUM_CHANNELS],
    pub dstp: u32,
    pub dsta: u32,
    pub t0c: u32,
    pub t1s: u32,
    pub t1md: u32,
    pub ims: u32,
    pub ist: u32,
    pub aiack: u32,
    pub asr0: u32,
    pub asr1: u32,
    pub aref: u32,
    pub rsel: u32,
    /// The SCU's embedded 32-bit DSP. Host software drives it through the
    /// PPAF/PPD/PDA/PDD ports at 0x80/0x84/0x88/0x8C.
    pub dsp: scu_dsp::Dsp,
    /// Set when host software starts the DSP (PPAF EXF bit). The Saturn
    /// aggregate drains this and runs the DSP at the top level, where its
    /// DMA can reach the system bus (it can't from inside the bus).
    dsp_run: bool,
    /// Sources that have been asserted since the last drain. The
    /// Saturn aggregate's `drain_scu_intc` pops one per call and
    /// raises it on the master SH-2's INTC. Distinct from `ist`: `ist`
    /// is software-visible state (cleared by write-0-clear from a handler);
    /// `fresh_assertions` tracks edges so we don't re-fire on the SH-2
    /// every batch while software is still handling the previous one.
    fresh_assertions: u32,
    /// External-interrupt prohibit latch for the CD-block ([`Source::Cd`]):
    /// once the CD interrupt fires it is latched off until software writes the
    /// AIACK register (0xA8 bit 0), exactly like Mednafen's `ABusIProhibit`
    /// (`scu.inc` `ABusIRQCheck` / case 0xA8). Without this the CD interrupt
    /// would re-fire on every per-sector HIRQ edge instead of once per ack.
    /// Serialized machine state.
    cd_prohibit: bool,
    /// Timer 1 down-counter (pixel clocks within a scanline). Reloaded from
    /// `t1s` at each HBLANK start, decremented by the dots elapsed per
    /// [`Scu::tick_timers`] call (Mednafen `Timer1_Counter`, 9-bit).
    timer1_counter: u32,
    /// Sticky "Timer 1 fired this line" flag, cleared at the HBLANK reload
    /// (Mednafen `Timer1_Met`).
    timer1_met: bool,
    /// Previous HBLANK level, for detecting the HBLANK rising edge (HB_Start).
    hb_prev: bool,
    /// Global cycle at the last [`Scu::tick_timers`] call, to derive the dots
    /// elapsed. Updated every call (even while the timers are disabled) so the
    /// first tick after software enables them sees a small delta.
    timer_last_now: u64,
}

/// Snapshot of a queued DMA request handed to the bus drainer. The drainer
/// (`system::drain_dma`) performs the byte movement through the bus,
/// since the SCU itself can't reach it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DmaRequest {
    pub channel: usize,
    pub src: u32,
    pub dst: u32,
    pub bytes: u32,
    /// Source-address increment per word (0 = fixed).
    pub src_add: u32,
    /// Destination-address increment per word (0 = fixed).
    pub dst_add: u32,
    /// Indirect mode: `dst` points at a table of `{size, dst, src}` longword
    /// triplets, the last flagged by bit 31 of its source word.
    pub indirect: bool,
    /// Whether to write the advanced source / destination back to `D*R` / `D*W`.
    pub read_update: bool,
    pub write_update: bool,
}

impl Scu {
    pub fn new() -> Self {
        let mut s = Self::default();
        // Reset start factor is manual (7) for every channel, so a D*EN go
        // before any D*MD write performs a manual transfer (matches the
        // SCU's reset state; software sets D*MD explicitly for event-driven
        // DMA).
        for c in &mut s.channels {
            c.mode = 0x07;
        }
        // Reset interrupt mask = 0xBFFF: every maskable SCU source is masked
        // at power-on; the BIOS unmasks the ones it wants (matches the
        // reference, `scu.inc` SCU_Reset). Resetting to 0 (all unmasked) made
        // us deliver interrupts the real BIOS isn't ready for during early init.
        s.ims = 0xBFFF;
        s
    }

    pub fn read32(&mut self, offset: u32) -> u32 {
        match offset {
            o if o < 0x60 => self.read_channel(o),
            0x60 => self.dstp,
            0x7C => self.dsta,
            0x80 => self.dsp_ppaf_read(),    // program control / status
            0x84 => self.dsp.regs.pc as u32, // PPD is write-only; report PC
            0x88 => self.dsp.regs.ra as u32, // PDA is write-only; report RA
            0x8C => self.dsp_pdd_read(),     // data-RAM data port (RA auto-inc)
            0x90 => self.t0c,
            0x94 => self.t1s,
            0x98 => self.t1md,
            0xA0 => self.ims,
            0xA4 => self.ist,
            0xA8 => self.aiack,
            0xB0 => self.asr0,
            0xB4 => self.asr1,
            0xB8 => self.aref,
            0xC4 => self.rsel,
            0xC8 => 0x0000_0004, // version: SCU revision
            _ => 0,
        }
    }

    pub fn write32(&mut self, offset: u32, val: u32) {
        match offset {
            o if o < 0x60 => self.write_channel(o, val),
            0x60 => self.dstp = val,
            0x7C => self.dsta = val,
            0x80 => self.dsp_ppaf_write(val), // program control: LEF / EXF
            0x84 => self.dsp_ppd_write(val),  // program-RAM data port (PC++)
            0x88 => self.dsp.regs.ra = val as u8, // data-RAM address port
            0x8C => self.dsp_pdd_write(val),  // data-RAM data port (RA auto-inc)
            0x90 => self.t0c = val,
            0x94 => self.t1s = val,
            0x98 => self.t1md = val,
            0xA0 => self.ims = val & 0xBFFF, // IMS is 16-bit; bit 14 is unused (scu.inc)
            // IST is write-AND-to-clear: a written 0 bit clears the flag,
            // a written 1 bit leaves it untouched (Mednafen `IPending &= DB`).
            0xA4 => self.ist &= val,
            0xA8 => {
                self.aiack = val;
                // A-bus interrupt acknowledge: writing bit 0 clears the
                // external-interrupt prohibit latch so the CD interrupt can
                // re-fire (Mednafen scu.inc case 0xA8 → `ABusIProhibit = 0`).
                // The next `set_cd_int` re-asserts it if the CD is still active.
                if val & 1 != 0 {
                    self.cd_prohibit = false;
                }
            }
            // A-bus set / refresh / SDRAM-select registers. Like the LLE oracle
            // (Mednafen `scu.inc`), these are **stored, not applied** — the SCU
            // wait-state / refresh / SRAM-vs-SDRAM behaviour they configure is
            // not part of the reference's timing model, so applying them would
            // diverge the trace (the M13 "don't add accuracy the oracle lacks"
            // rule). Only the hardware write masks (reserved bits read 0) are
            // modelled, matching Mednafen.
            0xB0 => self.asr0 = val & 0xFFFD_FFFD,
            0xB4 => self.asr1 = val & 0xF00D_FFFD,
            0xB8 => self.aref = val & 0x1F,
            0xC4 => self.rsel = val & 0x1,
            // 0xC8 VER is read-only.
            _ => {}
        }
    }

    fn read_channel(&self, offset: u32) -> u32 {
        let ch = (offset / CHANNEL_STRIDE) as usize;
        let in_ch = offset % CHANNEL_STRIDE;
        if ch >= NUM_CHANNELS {
            return 0;
        }
        let c = &self.channels[ch];
        match in_ch {
            0x00 => c.read_addr,
            0x04 => c.write_addr,
            0x08 => c.transfer_count,
            0x0C => c.add_value,
            0x10 => c.enable,
            0x14 => c.mode,
            _ => 0,
        }
    }

    fn write_channel(&mut self, offset: u32, val: u32) {
        let ch = (offset / CHANNEL_STRIDE) as usize;
        let in_ch = offset % CHANNEL_STRIDE;
        if ch >= NUM_CHANNELS {
            return;
        }
        let c = &mut self.channels[ch];
        match in_ch {
            0x00 => c.read_addr = val,
            0x04 => c.write_addr = val,
            // Channel 0 carries a 20-bit count; channels 1 and 2 carry
            // 12 bits. Mask conservatively to the wider one — software
            // writing larger values to ch1/2 would have been clipped
            // by hardware anyway and we'd surface a real-world bug if
            // we silently let it through unmasked.
            0x08 => {
                c.transfer_count = if ch == 0 {
                    val & 0x000F_FFFF
                } else {
                    val & 0x0000_0FFF
                };
            }
            0x0C => c.add_value = val,
            0x10 => {
                c.enable = val;
                // Only a channel set to the *manual* start factor (7) fires on
                // the D*EN go bit. Channels configured for a hardware start
                // factor (0..6) are merely armed here and wait for that event
                // (see `trigger_factor`) — enabling them must NOT start a
                // transfer immediately. Indirect transfers take their size
                // from the table, so the count guard only applies to direct.
                if val & DGO_BIT != 0
                    && c.start_factor() == 7
                    && (c.indirect() || c.transfer_count > 0)
                {
                    c.triggered = true;
                }
            }
            0x14 => c.mode = val,
            _ => {}
        }
    }

    pub fn read8(&mut self, offset: u32) -> u8 {
        let word = self.read32(offset & !3);
        (word >> (8 * (3 - (offset & 3)))) as u8
    }
    pub fn read16(&mut self, offset: u32) -> u16 {
        ((self.read8(offset) as u16) << 8) | self.read8(offset + 1) as u16
    }

    // ---- SCU-DSP host ports (PPAF/PPD/PDA/PDD) ---------------------------
    // Control-register (PPAF) bit layout per the SCU manual: bits 0..7 = PC,
    // bit 15 LEF (load PC), 16 EXF (execute), 18 EF (end IRQ), 19 VF, 20 CF,
    // 21 ZF, 22 SF, 23 T0F (DMA busy).

    /// Pack the DSP flags into the PPAF status bits.
    fn dsp_flags_bits(&self) -> u32 {
        let f = &self.dsp.regs.flags;
        (u32::from(f.exec) << 16)
            | (u32::from(f.end) << 18)
            | (u32::from(f.v) << 19)
            | (u32::from(f.c) << 20)
            | (u32::from(f.z) << 21)
            | (u32::from(f.s) << 22)
            | (u32::from(f.t0) << 23)
    }

    /// PPAF read: `(PC+1) | flags`. Reading clears the overflow (VF) and
    /// program-end (EF) flags, matching the hardware's read-to-acknowledge.
    fn dsp_ppaf_read(&mut self) -> u32 {
        let v = ((self.dsp.regs.pc as u32).wrapping_add(1) & 0xFF) | self.dsp_flags_bits();
        self.dsp.regs.flags.v = false;
        self.dsp.regs.flags.end = false;
        v
    }

    /// PPAF write: LEF (bit 15) loads the PC; EXF (bit 16) starts execution
    /// (the run is performed at the Saturn aggregate). ZF/SF are writable.
    fn dsp_ppaf_write(&mut self, val: u32) {
        if val & (1 << 15) != 0 {
            self.dsp.regs.pc = (val & 0xFF) as u8;
        }
        self.dsp.regs.flags.z = val & (1 << 21) != 0;
        self.dsp.regs.flags.s = val & (1 << 22) != 0;
        if val & (1 << 16) != 0 {
            let pc = self.dsp.regs.pc;
            self.dsp.start(pc);
            self.dsp_run = true;
        }
    }

    /// PPD write: load one microcode word at the current PC, then PC++.
    fn dsp_ppd_write(&mut self, val: u32) {
        let pc = self.dsp.regs.pc;
        self.dsp.program[pc as usize] = val;
        self.dsp.regs.pc = pc.wrapping_add(1);
    }

    /// PDD read: read the data-RAM word the RA pointer addresses, then RA++.
    /// RA is a flat 8-bit index across the four 64-word banks.
    fn dsp_pdd_read(&mut self) -> u32 {
        let ra = self.dsp.regs.ra;
        let v = self.dsp.data_ram[((ra >> 6) & 3) as usize][(ra & 0x3F) as usize];
        self.dsp.regs.ra = ra.wrapping_add(1);
        v
    }

    /// PDD write: write the data-RAM word the RA pointer addresses, then RA++.
    fn dsp_pdd_write(&mut self, val: u32) {
        let ra = self.dsp.regs.ra;
        self.dsp.data_ram[((ra >> 6) & 3) as usize][(ra & 0x3F) as usize] = val;
        self.dsp.regs.ra = ra.wrapping_add(1);
    }

    /// Pop the "DSP should run" request set by a PPAF EXF write. The Saturn
    /// aggregate runs the DSP (so its DMA can reach the system bus).
    pub fn take_dsp_run(&mut self) -> bool {
        core::mem::take(&mut self.dsp_run)
    }
    pub fn write8(&mut self, offset: u32, val: u8) {
        let aligned = offset & !3;
        let shift = 8 * (3 - (offset & 3));
        let cur = self.read32(aligned);
        let mask = !(0xFFu32 << shift);
        // Byte writes can't trigger DMA — the DGO check lives in the
        // 32-bit write path. RMW just patches the byte without going
        // through write32's side-effect logic.
        let new = (cur & mask) | ((val as u32) << shift);
        match aligned {
            o if o < 0x60 => self.write_channel_raw(o, new),
            _ => self.write32(aligned, new),
        }
    }
    pub fn write16(&mut self, offset: u32, val: u16) {
        self.write8(offset, (val >> 8) as u8);
        self.write8(offset + 1, val as u8);
    }

    /// Channel register write that does NOT honour the DGO trigger.
    /// Used by byte / halfword writes (which build up a 32-bit value
    /// piece-by-piece and shouldn't fire DMA mid-construction).
    fn write_channel_raw(&mut self, offset: u32, val: u32) {
        let ch = (offset / CHANNEL_STRIDE) as usize;
        let in_ch = offset % CHANNEL_STRIDE;
        if ch >= NUM_CHANNELS {
            return;
        }
        let c = &mut self.channels[ch];
        match in_ch {
            0x00 => c.read_addr = val,
            0x04 => c.write_addr = val,
            0x08 => {
                c.transfer_count = if ch == 0 {
                    val & 0x000F_FFFF
                } else {
                    val & 0x0000_0FFF
                };
            }
            0x0C => c.add_value = val,
            0x10 => c.enable = val, // no trigger
            0x14 => c.mode = val,
            _ => {}
        }
    }

    /// Cheap hot-path probe: is any DMA channel triggered? Sampled once per
    /// master instruction (`step_cpus`) to keep the `drain_dma` call cold.
    #[inline]
    pub fn dma_pending(&self) -> bool {
        self.channels.iter().any(|ch| ch.triggered)
    }

    /// Pop the next channel that has a queued DMA. The caller is
    /// expected to perform the actual bus transfer and then update
    /// the channel's `read_addr` / `write_addr` / `transfer_count` to
    /// reflect completion via [`finish_dma`].
    pub fn take_pending_dma(&mut self) -> Option<DmaRequest> {
        for (i, ch) in self.channels.iter_mut().enumerate() {
            if ch.triggered {
                ch.triggered = false;
                return Some(DmaRequest {
                    channel: i,
                    src: ch.read_addr,
                    dst: ch.write_addr,
                    bytes: ch.transfer_count,
                    src_add: ch.src_add(),
                    dst_add: ch.dst_add(),
                    indirect: ch.indirect(),
                    read_update: ch.read_update(),
                    write_update: ch.write_update(),
                });
            }
        }
        None
    }

    /// Trigger every armed channel whose start factor matches `factor` (0..6;
    /// the SCU manual's hardware DMA-start events — see [`DmaChannel::
    /// start_factor`]). The Saturn aggregate calls this from the matching
    /// event (VBlank-IN, sprite-draw-end, …); the queued transfers drain the
    /// same way a manual DMA does. Channels stay armed, so they re-fire on the
    /// next event.
    pub fn trigger_dma_factor(&mut self, factor: u8) {
        for c in &mut self.channels {
            if c.armed() && c.start_factor() == factor {
                c.triggered = true;
            }
        }
    }

    /// Mark a DMA channel as having completed: zero its remaining count, and
    /// — only when the corresponding update flag (`D*MD` RUP/WUP) is set —
    /// store the post-transfer source / destination so software reading
    /// `D*R` / `D*W` sees the addresses past the moved block. Then raise the
    /// channel's "DMA end" interrupt source.
    pub fn finish_dma(&mut self, channel: usize, final_src: u32, final_dst: u32) {
        let c = &mut self.channels[channel];
        if c.read_update() {
            c.read_addr = final_src;
        }
        if c.write_update() {
            c.write_addr = final_dst;
        }
        c.transfer_count = 0;
        // Do NOT clear the D*EN enable (bit 8) here. In our model that bit IS
        // `armed()`, and a hardware-factor channel (start factor 0..6) re-fires
        // on every matching event for as long as it stays armed — Mednafen's
        // `Enable` persists across completion (`SCU_DoDMAEnd` never clears it).
        // The one-shot is the separate `triggered` flag (cleared in
        // `take_pending_dma`), so a manual transfer already won't re-fire;
        // clearing the enable only broke event-driven re-arming (movie/FMV DMA).
        let source = match channel {
            0 => Source::Level0DmaEnd,
            1 => Source::Level1DmaEnd,
            2 => Source::Level2DmaEnd,
            _ => return,
        };
        self.raise(source);
    }

    /// Assert an interrupt source: set its IST bit and mark it as a
    /// fresh assertion the Saturn drainer should forward to the master
    /// SH-2. Software clears IST manually via the standard write-0-clear path.
    pub fn raise(&mut self, source: Source) {
        let bit = 1 << source.bit();
        self.ist |= bit;
        self.fresh_assertions |= bit;
    }

    /// Timer enable (T1MD bit 0, "TENB"): gates both SCU timers.
    #[inline]
    pub fn timers_enabled(&self) -> bool {
        self.t1md & 1 != 0
    }

    /// Advance the SCU sub-line timers and the HBLANK-IN interrupt from the
    /// current raster state — a port of Mednafen `SCU_SetHBVB` (`scu.inc`).
    ///
    /// * `now` is the global cycle; `cycles_per_line`/`dots_per_line` convert
    ///   the elapsed time into the pixel-clock delta the Timer-1 counter uses.
    /// * `new_hb` is the live HBLANK level (its rising edge is "HB_Start").
    /// * `timer0_met` is whether the current scanline matches T0C — Timer 1 in
    ///   mode 1 (T1MD bit 8) only fires on the Timer-0 line.
    ///
    /// Raises **HBlank-IN** on the HBLANK rising edge and **Timer 1** when its
    /// down-counter reaches zero (both gated by TENB). Timer 0 itself is raised
    /// by [`system::Saturn::update_video_timing`](crate::system) on its line
    /// compare; this only consumes `timer0_met` for Timer 1's mode gate.
    ///
    /// **Sub-line precision:** our raster runs at `update_video_timing`
    /// granularity, so the Timer-1 H-position lands within ~that window of the
    /// true dot (the dot-exact raster is deferred M12 work). The line selection,
    /// mode, enable, and reload are exact.
    pub fn tick_timers(
        &mut self,
        now: u64,
        cycles_per_line: u64,
        dots_per_line: u32,
        new_hb: bool,
        timer0_met: bool,
    ) {
        let pclocks = {
            let delta = now.saturating_sub(self.timer_last_now);
            (delta.saturating_mul(dots_per_line as u64) / cycles_per_line.max(1)) as u32
        };
        self.timer_last_now = now;
        let hb_start = new_hb && !self.hb_prev;
        self.hb_prev = new_hb;

        if self.timers_enabled() {
            // Count the down-counter toward the T1S H-position. Mednafen
            // event-schedules the tick to land exactly on zero; we run at batch
            // granularity, so we fire on the step that *crosses* zero (and clamp
            // there) rather than requiring an exact-zero sample.
            if pclocks > 0 && self.timer1_counter > 0 {
                if self.timer1_counter <= pclocks {
                    self.timer1_counter = 0;
                    let mode1 = self.t1md & 0x100 != 0;
                    if (!mode1 || timer0_met) && !self.timer1_met {
                        self.timer1_met = true;
                        self.raise(Source::Timer1);
                    }
                } else {
                    self.timer1_counter -= pclocks;
                }
            }
            // Reload from T1S at the start of each HBLANK for the next line.
            if hb_start {
                self.timer1_met = false;
                self.timer1_counter = (self.t1s & 0x1FF).max(1);
            }
        }

        // HBlank-IN fires on the HBLANK rising edge (Mednafen asserts it as a
        // level; we raise the edge, as VBlank-IN/-OUT are handled).
        if hb_start {
            self.raise(Source::HBlankIn);
        }
    }

    /// Assert/deassert the CD-block external interrupt ([`Source::Cd`]) as a
    /// level = `active` (`(CD HIRQ & HIRQ_Mask) != 0`). Mirrors Mednafen's
    /// `RecalcIRQOut` → `ABusIRQCheck` (`cdb.cpp` / `scu.inc`): a fresh SCU
    /// assertion is raised only when the CD becomes active **and** the prohibit
    /// latch is clear, then the latch is set so it does not re-fire until the
    /// handler acknowledges via AIACK (0xA8). Called every master instruction by
    /// [`system`](crate::system) with the live CD HIRQ state, so the interrupt
    /// lands the instant the CD raises an unmasked HIRQ bit — the GFS file
    /// library polls nothing; it is driven by this interrupt.
    pub fn set_cd_int(&mut self, active: bool) {
        static CD_NO_PROHIBIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let no_prohibit =
            *CD_NO_PROHIBIT.get_or_init(|| std::env::var_os("SAT_CD_NO_PROHIBIT").is_some());
        if active && (!self.cd_prohibit || no_prohibit) {
            let bit = 1u32 << Source::Cd.bit();
            self.fresh_assertions |= bit;
            self.ist |= bit;
            if !no_prohibit {
                self.cd_prohibit = true;
            }
        }
    }

    /// The highest-priority unmasked pending source as `(source, level)`.
    /// Sampled once per master instruction so an interrupt lands the exact
    /// cycle `SR.imask` drops below its level; returns `None` when nothing
    /// qualifies.
    ///
    /// Accepting the vector consumes only the fresh edge used by our interrupt
    /// presenter. The software-visible IST latch remains set until the guest
    /// clears it with the register's write-0-clear path.
    pub fn take_pending_interrupt(&mut self, sh2_imask: u8) -> Option<(Source, u8)> {
        // External interrupts (bits 16+, i.e. the CD-block) are masked by IMS
        // **bit 15**: Mednafen computes `IPending & ~(int16)IMask`, sign-
        // extending the 16-bit mask so bit 15 gates all of bits 16..31.
        // Replicate that so the CD interrupt honours its real mask bit while
        // the internal sources (bits 0..13) keep their own mask bits.
        let unmasked = self.fresh_assertions & !(self.ims as i16 as i32 as u32);
        // Hot path: this is sampled once per master instruction (Phase 2B), and
        // almost always nothing is pending — skip the per-source priority walk.
        if unmasked == 0 {
            return None;
        }
        for &source in ALL_SOURCES {
            let bit = 1 << source.bit();
            if unmasked & bit == 0 {
                continue;
            }
            let lvl = source.priority();
            if lvl <= sh2_imask {
                continue;
            }
            self.fresh_assertions &= !bit;
            return Some((source, lvl));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_register_is_read_only_and_returns_four() {
        let mut s = Scu::new();
        assert_eq!(s.read32(0xC8), 0x04);
        s.write32(0xC8, 0xDEAD_BEEF);
        assert_eq!(s.read32(0xC8), 0x04, "VER ignores writes");
    }

    #[test]
    fn cd_external_interrupt_vector_level_mask_and_aiack() {
        let mut s = Scu::new();
        // Vector/level match Mednafen scu.inc (external_tab[0]): 0x50, level 7.
        assert_eq!(Source::Cd.vector(), 0x50);
        assert_eq!(Source::Cd.priority(), 7);

        // Unmask the external interrupts (IMS bit 15 clear); leave room above
        // SR.imask so a level-7 source is deliverable.
        s.write32(0xA0, 0x0000); // all unmasked

        // Asserting the CD level raises a fresh assertion; it is taken at the
        // CD's level/vector when SR.imask is below 7.
        s.set_cd_int(true);
        assert_eq!(s.take_pending_interrupt(0), Some((Source::Cd, 7)));

        // Latched off after firing: re-asserting without an AIACK does NOT
        // re-fire (the ABusIProhibit latch).
        s.set_cd_int(true);
        assert_eq!(s.take_pending_interrupt(0), None, "no re-fire until AIACK");

        // AIACK (0xA8 bit 0) clears the latch → the next assert re-fires.
        s.write32(0xA8, 1);
        s.set_cd_int(true);
        assert_eq!(s.take_pending_interrupt(0), Some((Source::Cd, 7)));

        // IMS bit 15 masks the external interrupt (the SCU sign-extends the
        // 16-bit mask): with bit 15 set, the CD interrupt is not delivered.
        s.write32(0xA8, 1); // re-arm
        s.write32(0xA0, 0x8000); // IMS bit 15 set → externals masked
        s.set_cd_int(true);
        assert_eq!(
            s.take_pending_interrupt(0),
            None,
            "IMS bit 15 masks all external interrupts (incl. the CD-block)"
        );

        // SR.imask >= 7 also gates it (level 7 needs imask < 7).
        s.write32(0xA8, 1);
        s.write32(0xA0, 0x0000);
        s.set_cd_int(true);
        assert_eq!(
            s.take_pending_interrupt(7),
            None,
            "imask 7 gates the level-7 CD int"
        );
        assert_eq!(s.take_pending_interrupt(6), Some((Source::Cd, 7)));
    }

    #[test]
    fn channel_registers_round_trip() {
        let mut s = Scu::new();
        s.write32(0x20, 0x0600_1000); // D1R
        s.write32(0x24, 0x0020_2000); // D1W
        s.write32(0x28, 0x0000_0040); // D1C
        assert_eq!(s.channels[1].read_addr, 0x0600_1000);
        assert_eq!(s.channels[1].write_addr, 0x0020_2000);
        assert_eq!(s.channels[1].transfer_count, 0x40);
    }

    #[test]
    fn channel0_count_is_20_bit_and_channel12_count_is_12_bit() {
        let mut s = Scu::new();
        s.write32(0x08, 0xFFFF_FFFF); // D0C
        s.write32(0x28, 0xFFFF_FFFF); // D1C
        s.write32(0x48, 0xFFFF_FFFF); // D2C
        assert_eq!(s.channels[0].transfer_count, 0x000F_FFFF);
        assert_eq!(s.channels[1].transfer_count, 0x0000_0FFF);
        assert_eq!(s.channels[2].transfer_count, 0x0000_0FFF);
    }

    #[test]
    fn dgo_write_with_nonzero_count_queues_a_dma_request() {
        let mut s = Scu::new();
        s.write32(0x00, 0x0600_0000); // D0R
        s.write32(0x04, 0x0020_0000); // D0W
        s.write32(0x08, 0x100); // D0C
        s.write32(0x10, DGO_BIT); // D0EN with DGO set
        let req = s.take_pending_dma().expect("DMA should be queued");
        assert_eq!(req.channel, 0);
        assert_eq!(req.src, 0x0600_0000);
        assert_eq!(req.dst, 0x0020_0000);
        assert_eq!(req.bytes, 0x100);
        assert!(s.take_pending_dma().is_none(), "queue is single-shot");
    }

    #[test]
    fn dgo_with_zero_count_does_not_trigger() {
        let mut s = Scu::new();
        s.write32(0x10, DGO_BIT);
        assert!(s.take_pending_dma().is_none());
    }

    #[test]
    fn byte_writes_to_d0en_do_not_trigger_mid_construction() {
        let mut s = Scu::new();
        s.write32(0x08, 0x100); // non-zero count
        // Build up DGO-bit via 4 byte writes — the high byte of the
        // big-endian 0x0000_0100 lands at offset 0x12. We must not
        // fire DMA partway through.
        s.write8(0x10, 0x00);
        s.write8(0x11, 0x00);
        s.write8(0x12, 0x01);
        s.write8(0x13, 0x00);
        assert!(
            s.take_pending_dma().is_none(),
            "byte writes must not fire DMA — software is expected to use word writes"
        );
    }

    #[test]
    fn timer_and_interrupt_registers_use_hardware_offsets() {
        // T0C/T1S/T1MD at 0x90/0x94/0x98, IMS at 0xA0, IST at 0xA4 — the
        // hardware map (SCU User's Manual, cross-checked with MAME). Getting
        // IMS at 0xA0 (not 0xB0) is what lets the BIOS unmask the SMPC
        // interrupt; an off-by-0x10 map silently masked it.
        let mut s = Scu::new();
        s.write32(0x90, 0x0000_1234); // T0C
        s.write32(0x94, 0x0000_5678); // T1S
        s.write32(0x98, 0x0000_0001); // T1MD
        s.write32(0xA0, 0x0000_FF00); // IMS — masked to 0xBFFF (bit 14 unused)
        assert_eq!(s.read32(0x90), 0x0000_1234);
        assert_eq!(s.read32(0x94), 0x0000_5678);
        assert_eq!(s.read32(0x98), 0x0000_0001);
        assert_eq!(
            s.read32(0xA0),
            0x0000_BF00,
            "IMS at 0xA0 (bit 14 masked off)"
        );
        assert_eq!(
            s.ims, 0x0000_BF00,
            "0xA0 write reaches the mask field, masked to 0xBFFF"
        );
    }

    #[test]
    fn dsp_program_load_and_data_ram_ports_round_trip() {
        let mut s = Scu::new();
        // PPD (0x84): load two program words at PC 0/1.
        s.write32(0x84, 0x1111_1111);
        s.write32(0x84, 0x2222_2222);
        assert_eq!(s.dsp.program[0], 0x1111_1111);
        assert_eq!(s.dsp.program[1], 0x2222_2222);
        // PDA (0x88) sets RA; PDD (0x8C) writes data RAM with auto-increment.
        s.write32(0x88, 0x00);
        s.write32(0x8C, 0xDEAD_0000);
        s.write32(0x8C, 0xBEEF_0001);
        assert_eq!(s.dsp.data_ram[0][0], 0xDEAD_0000);
        assert_eq!(s.dsp.data_ram[0][1], 0xBEEF_0001);
        // Read them back via PDA/PDD.
        s.write32(0x88, 0x00);
        assert_eq!(s.read32(0x8C), 0xDEAD_0000);
        assert_eq!(s.read32(0x8C), 0xBEEF_0001);
    }

    #[test]
    fn dsp_ppaf_start_sets_exec_and_run_request() {
        let mut s = Scu::new();
        // LEF (bit15) | EXF (bit16) | PC=0x05 → load PC and start.
        s.write32(0x80, (1 << 15) | (1 << 16) | 0x05);
        assert_eq!(s.dsp.regs.pc, 0x05);
        assert!(s.dsp.regs.flags.exec, "EXF starts the DSP");
        assert!(s.take_dsp_run(), "PPAF EXF raises the run request");
        assert!(!s.take_dsp_run(), "run request is one-shot");
    }

    #[test]
    fn finish_dma_writes_back_final_addresses_only_when_update_enabled() {
        let mut s = Scu::new();
        // D*MD with RUP (bit16) + WUP (bit8) + manual factor (7): addresses
        // update to the post-transfer values.
        s.channels[0].mode = (1 << 16) | (1 << 8) | 7;
        s.channels[0].read_addr = 0x0600_0000;
        s.channels[0].write_addr = 0x0020_0000;
        s.channels[0].transfer_count = 0x100;
        s.finish_dma(0, 0x0600_0100, 0x0020_0100);
        assert_eq!(s.channels[0].read_addr, 0x0600_0100);
        assert_eq!(s.channels[0].write_addr, 0x0020_0100);
        assert_eq!(s.channels[0].transfer_count, 0);

        // With RUP/WUP clear, the addresses keep their programmed values.
        let mut s = Scu::new(); // mode defaults to factor 7, RUP/WUP clear
        s.channels[1].read_addr = 0x0600_0000;
        s.channels[1].write_addr = 0x0020_0000;
        s.channels[1].transfer_count = 0x100;
        s.finish_dma(1, 0x0600_0100, 0x0020_0100);
        assert_eq!(
            s.channels[1].read_addr, 0x0600_0000,
            "RUP clear → unchanged"
        );
        assert_eq!(
            s.channels[1].write_addr, 0x0020_0000,
            "WUP clear → unchanged"
        );
        assert_eq!(s.channels[1].transfer_count, 0);
    }

    #[test]
    fn finish_dma_raises_the_channel_specific_end_source() {
        let mut s = Scu::new();
        s.ims = 0; // reset masks all sources; unmask to test delivery
        s.channels[1].transfer_count = 0x10;
        s.finish_dma(1, 0, 0);
        // IST bit set, fresh assertion ready, take_pending returns it.
        assert_ne!(s.ist & (1 << Source::Level1DmaEnd.bit()), 0);
        let (src, lvl) = s.take_pending_interrupt(0).unwrap();
        assert_eq!(src, Source::Level1DmaEnd);
        assert_eq!(lvl, 6);
    }

    #[test]
    fn event_driven_channel_stays_armed_after_completion() {
        // The D*EN enable (bit 8 = `armed()`) persists across completion, like
        // Mednafen's `Enable` (`SCU_DoDMAEnd` never clears it): a hardware-factor
        // channel re-fires on every matching event. `finish_dma` must NOT disarm
        // it — the per-transfer one-shot is the separate `triggered` flag.
        let mut s = Scu::new();
        s.channels[0].mode = 0; // start factor 0 (VBlank-IN), direct
        s.channels[0].enable = DGO_BIT; // armed (bit 8), no immediate go
        s.channels[0].transfer_count = 0x10;
        s.trigger_dma_factor(0);
        assert!(
            s.take_pending_dma().is_some(),
            "armed channel fires on its event"
        );
        s.finish_dma(0, 0, 0);
        assert!(s.channels[0].armed(), "enable persists across completion");
        s.trigger_dma_factor(0);
        assert!(
            s.take_pending_dma().is_some(),
            "still armed → re-fires on the next event"
        );
    }

    #[test]
    fn take_pending_interrupt_resolves_by_priority() {
        let mut s = Scu::new();
        s.ims = 0; // reset masks all sources; unmask to test delivery
        s.raise(Source::SpriteDrawEnd); // prio 2
        s.raise(Source::VBlankIn); // prio 15
        s.raise(Source::DmaIllegal); // prio 3
        let (src, _) = s.take_pending_interrupt(0).unwrap();
        assert_eq!(src, Source::VBlankIn, "highest priority wins");
        let (src, _) = s.take_pending_interrupt(0).unwrap();
        assert_eq!(src, Source::DmaIllegal);
        let (src, _) = s.take_pending_interrupt(0).unwrap();
        assert_eq!(src, Source::SpriteDrawEnd);
        assert!(s.take_pending_interrupt(0).is_none());
    }

    #[test]
    fn take_pending_interrupt_honours_sh2_imask() {
        let mut s = Scu::new();
        s.ims = 0; // reset masks all sources; unmask to test SH-2 imask gating
        s.raise(Source::Smpc); // priority 8
        assert!(
            s.take_pending_interrupt(8).is_none(),
            "imask 8 blocks level 8"
        );
        assert!(
            s.take_pending_interrupt(7).is_some(),
            "imask 7 allows level 8"
        );
    }

    #[test]
    fn take_pending_interrupt_honours_ims_per_source() {
        let mut s = Scu::new();
        s.ims = 1 << Source::VBlankIn.bit();
        s.raise(Source::VBlankIn);
        s.raise(Source::HBlankIn);
        let (src, _) = s.take_pending_interrupt(0).unwrap();
        assert_eq!(src, Source::HBlankIn, "VBlankIn masked, HBlankIn allowed");
    }

    #[test]
    fn taking_an_interrupt_consumes_edge_but_keeps_ist_latched() {
        let mut s = Scu::new();
        s.ims = 0; // reset masks all sources; unmask to test acknowledge
        s.raise(Source::DspEnd);
        let _ = s.take_pending_interrupt(0).unwrap();
        assert_eq!(
            s.ist & (1 << Source::DspEnd.bit()),
            1 << Source::DspEnd.bit(),
            "vector acceptance leaves the software-visible IST bit latched"
        );
        // Re-draining without a fresh raise returns nothing.
        assert!(s.take_pending_interrupt(0).is_none());
        // After a new raise, the same source fires again.
        s.raise(Source::DspEnd);
        assert!(s.take_pending_interrupt(0).is_some());
    }

    #[test]
    fn a_masked_source_keeps_its_ist_bit_until_it_is_actually_taken() {
        // Masking gates delivery, not the visible status latch.
        let mut s = Scu::new();
        s.ims = 1 << Source::VBlankOut.bit();
        s.raise(Source::VBlankOut);
        assert!(
            s.take_pending_interrupt(0).is_none(),
            "masked: not delivered"
        );
        assert_ne!(
            s.ist & (1 << Source::VBlankOut.bit()),
            0,
            "masked source's IST bit stays set (not acknowledged)"
        );
    }

    #[test]
    fn dma_request_carries_stride_and_mode_flags() {
        let mut s = Scu::new();
        s.write32(0x00, 0x0600_0000); // D0R
        s.write32(0x04, 0x0020_0000); // D0W
        s.write32(0x08, 0x40); // D0C
        // D0AD: src +4 (bit 8), dst code 2 → 1<<2 = 4 bytes.
        s.write32(0x0C, 0x100 | 0x2);
        // D0MD: manual (7) + indirect (bit24) + RUP (bit16) + WUP (bit8).
        s.write32(0x14, (1 << 24) | (1 << 16) | (1 << 8) | 7);
        s.write32(0x10, DGO_BIT); // trigger (indirect ignores the count guard)
        let req = s.take_pending_dma().expect("queued");
        assert_eq!(req.src_add, 4, "D*AD bit 8 → +4 source stride");
        assert_eq!(req.dst_add, 4, "D*AD code 2 → 2^2 = 4 byte dest stride");
        assert!(req.indirect);
        assert!(req.read_update);
        assert!(req.write_update);
    }

    #[test]
    fn dma_stride_zero_codes_mean_fixed_addresses() {
        let mut s = Scu::new();
        s.write32(0x08, 0x10);
        // D0AD: src bit 8 clear → fixed source; dst code 0 → 2^0 = 1 → fixed.
        s.write32(0x0C, 0x000);
        s.write32(0x14, 7); // manual
        s.write32(0x10, DGO_BIT);
        let req = s.take_pending_dma().expect("queued");
        assert_eq!(req.src_add, 0, "fixed source (e.g. a FIFO register)");
        assert_eq!(req.dst_add, 0, "code 0 → fixed destination");
    }

    #[test]
    fn dst_stride_doubles_per_code() {
        // D*AD destination code 0..7 → 2^code bytes, with code 0 meaning fixed.
        let mut s = Scu::new();
        for (code, expected) in [(0u32, 0u32), (1, 2), (2, 4), (3, 8), (7, 128)] {
            s.channels[0].add_value = code;
            assert_eq!(s.channels[0].dst_add(), expected, "code {code}");
        }
    }

    #[test]
    fn indirect_channel_triggers_even_with_zero_count() {
        // The count guard only applies to direct transfers — indirect takes its
        // size from the table, so a zero programmed count must still fire.
        let mut s = Scu::new();
        s.write32(0x08, 0); // count 0
        s.write32(0x14, (1 << 24) | 7); // indirect + manual
        s.write32(0x10, DGO_BIT);
        assert!(
            s.take_pending_dma().is_some(),
            "indirect fires with count 0"
        );
    }

    #[test]
    fn hardware_start_factor_only_fires_on_the_matching_event() {
        let mut s = Scu::new();
        // Arm channel 0 for start factor 0 (VBlank-IN); enabling must not fire.
        s.write32(0x14, 0); // D0MD: factor 0
        s.write32(0x08, 0x10);
        s.write32(0x10, DGO_BIT); // armed
        assert!(
            s.take_pending_dma().is_none(),
            "factor-0 channel waits for the event"
        );
        // A non-matching event leaves it armed.
        s.trigger_dma_factor(3); // Timer0 — wrong factor
        assert!(s.take_pending_dma().is_none());
        // The matching event triggers it; the channel stays armed for re-fire.
        s.trigger_dma_factor(0); // VBlank-IN
        assert!(s.take_pending_dma().is_some());
        s.trigger_dma_factor(0);
        assert!(
            s.take_pending_dma().is_some(),
            "armed channel re-fires on the next event"
        );
    }

    #[test]
    fn unarmed_channel_ignores_hardware_factor_events() {
        let mut s = Scu::new();
        s.write32(0x14, 2); // factor 2 (HBlank), but not armed (D*EN bit 8 clear)
        s.trigger_dma_factor(2);
        assert!(
            s.take_pending_dma().is_none(),
            "unarmed channel never triggers"
        );
    }

    #[test]
    fn byte_writes_to_channel_registers_patch_without_triggering() {
        let mut s = Scu::new();
        // Build D0R = 0x0600_1000 byte-by-byte (big-endian) and check no trigger.
        s.write32(0x08, 0x10); // non-zero count
        s.write32(0x14, 7); // manual factor
        s.write8(0x00, 0x06);
        s.write8(0x01, 0x00);
        s.write8(0x02, 0x10);
        s.write8(0x03, 0x00);
        assert_eq!(
            s.channels[0].read_addr, 0x0600_1000,
            "byte writes assemble the word"
        );
        // A byte write to D0EN's DGO byte must NOT trigger.
        s.write8(0x12, 0x01); // bit 8 of the big-endian enable word
        assert!(s.take_pending_dma().is_none(), "byte writes never fire DMA");
    }

    #[test]
    fn byte_writes_to_count_honour_the_per_channel_mask() {
        let mut s = Scu::new();
        // Channel 1 carries a 12-bit count; a wider byte-assembled value clips.
        s.write8(0x28, 0xFF);
        s.write8(0x29, 0xFF);
        s.write8(0x2A, 0xFF);
        s.write8(0x2B, 0xFF);
        assert_eq!(
            s.channels[1].transfer_count, 0x0000_0FFF,
            "ch1 count masked to 12 bits"
        );
    }

    #[test]
    fn read8_and_read16_slice_the_register_word_big_endian() {
        let mut s = Scu::new();
        s.write32(0x90, 0x1234_5678); // T0C
        assert_eq!(s.read8(0x90), 0x12);
        assert_eq!(s.read8(0x91), 0x34);
        assert_eq!(s.read8(0x93), 0x78);
        assert_eq!(s.read16(0x90), 0x1234);
        assert_eq!(s.read16(0x92), 0x5678);
    }

    #[test]
    fn abus_config_registers_round_trip_through_their_hardware_masks() {
        let mut s = Scu::new();
        // The A-bus config registers drop their reserved bits on write, matching
        // Mednafen (ASR0 & 0xFFFDFFFD, ASR1 & 0xF00DFFFD, AREF & 0x1F, RSEL & 1).
        s.write32(0xB0, 0xFFFF_FFFF);
        s.write32(0xB4, 0xFFFF_FFFF);
        s.write32(0xB8, 0xFFFF_FFFF);
        s.write32(0xC4, 0xFFFF_FFFF);
        assert_eq!(s.read32(0xB0), 0xFFFD_FFFD);
        assert_eq!(s.read32(0xB4), 0xF00D_FFFD);
        assert_eq!(s.read32(0xB8), 0x0000_001F);
        assert_eq!(s.read32(0xC4), 0x0000_0001);
    }

    #[test]
    fn dstp_and_dsta_round_trip() {
        let mut s = Scu::new();
        s.write32(0x60, 0x0000_0001); // DSTP — force stop
        s.write32(0x7C, 0xDEAD_BEEF); // DSTA — status (modelled as storage)
        assert_eq!(s.read32(0x60), 0x0000_0001);
        assert_eq!(s.read32(0x7C), 0xDEAD_BEEF);
    }

    #[test]
    fn aiack_write_without_bit0_does_not_clear_the_cd_prohibit_latch() {
        let mut s = Scu::new();
        s.write32(0xA0, 0x0000); // unmask externals
        s.set_cd_int(true);
        assert!(s.take_pending_interrupt(0).is_some());
        // Writing AIACK with bit 0 *clear* leaves the prohibit latch set.
        s.write32(0xA8, 0x0000_0002);
        s.set_cd_int(true);
        assert!(
            s.take_pending_interrupt(0).is_none(),
            "bit 0 clear → latch unchanged"
        );
        assert_eq!(s.read32(0xA8), 0x0000_0002, "AIACK stored");
    }

    #[test]
    fn ims_write_masks_bit14_off() {
        let mut s = Scu::new();
        s.write32(0xA0, 0xFFFF_FFFF);
        assert_eq!(
            s.read32(0xA0),
            0x0000_BFFF,
            "IMS is 16-bit with bit 14 unused"
        );
    }

    #[test]
    fn out_of_range_channel_offset_reads_zero_and_drops_writes() {
        let mut s = Scu::new();
        // Offset 0x5C is channel 2's reserved tail (in_ch 0x1C) → reads 0.
        assert_eq!(s.read32(0x5C), 0);
        s.write32(0x5C, 0xDEAD_BEEF); // dropped, no panic
        assert_eq!(s.read32(0x5C), 0);
        // An unmapped offset above the version register reads 0.
        assert_eq!(s.read32(0xF0), 0);
    }

    #[test]
    fn dsp_ppaf_read_reports_flags_and_clears_v_and_end() {
        let mut s = Scu::new();
        s.dsp.regs.pc = 0x0A;
        s.dsp.regs.flags.exec = true;
        s.dsp.regs.flags.end = true;
        s.dsp.regs.flags.v = true;
        s.dsp.regs.flags.z = true;
        let v = s.read32(0x80);
        // Low byte = (PC + 1) & 0xFF.
        assert_eq!(v & 0xFF, 0x0B);
        // Flag bits per dsp_flags_bits: EXF=16, EF=18, ZF=21.
        assert_ne!(v & (1 << 16), 0, "EXF reported");
        assert_ne!(v & (1 << 18), 0, "EF reported");
        assert_ne!(v & (1 << 19), 0, "VF reported");
        assert_ne!(v & (1 << 21), 0, "ZF reported");
        // Reading clears VF and EF (read-to-acknowledge).
        let v2 = s.read32(0x80);
        assert_eq!(v2 & (1 << 19), 0, "VF cleared by the previous read");
        assert_eq!(v2 & (1 << 18), 0, "EF cleared by the previous read");
    }

    #[test]
    fn dsp_ppaf_write_loads_pc_and_zf_sf_without_starting() {
        let mut s = Scu::new();
        // LEF (bit15) loads PC; ZF (bit21) / SF (bit22) are writable; no EXF.
        s.write32(0x80, (1 << 15) | (1 << 21) | (1 << 22) | 0x07);
        assert_eq!(s.dsp.regs.pc, 0x07, "LEF loaded the PC");
        assert!(s.dsp.regs.flags.z, "ZF writable");
        assert!(s.dsp.regs.flags.s, "SF writable");
        assert!(!s.take_dsp_run(), "no EXF → no run request");
    }

    #[test]
    fn dsp_pdd_addresses_the_right_bank_via_ra() {
        let mut s = Scu::new();
        // RA top 2 bits select the bank; low 6 the word. RA = 0x80 → bank 2, word 0.
        s.write32(0x88, 0x80); // PDA sets RA
        s.write32(0x8C, 0xC0DE_0000); // PDD writes, RA auto-increments
        assert_eq!(s.dsp.data_ram[2][0], 0xC0DE_0000);
        assert_eq!(s.dsp.regs.ra, 0x81, "RA auto-incremented past the write");
    }

    #[test]
    fn ist_writes_are_write_zero_to_clear() {
        let mut s = Scu::new();
        s.raise(Source::Timer0);
        s.raise(Source::SoundRequest);
        assert_eq!(
            s.ist,
            (1 << Source::Timer0.bit()) | (1 << Source::SoundRequest.bit())
        );
        // Clear only Timer0 (IST at 0xA4) by writing that bit as 0 and all
        // bits to retain as 1.
        s.write32(0xA4, !(1 << Source::Timer0.bit()));
        assert_eq!(s.ist, 1 << Source::SoundRequest.bit());
        // Writing 0 clears every pending bit.
        s.write32(0xA4, 0);
        assert_eq!(s.ist, 0);
    }
}
