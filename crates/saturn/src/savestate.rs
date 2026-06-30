//! Save states: a full, deterministic snapshot of the machine, and restore.
//!
//! A save state captures **every volatile bit of state** — both SH-2 cores
//! (registers, pipeline, cache, on-chip peripherals), the MC68EC000, the
//! SCU-DSP and SCSP-DSP, all RAM (work/sound/VRAM/CRAM/framebuffer/backup),
//! every peripheral register bank, the CD-block buffer/partition state, the
//! cartridge's volatile RAM, and the scheduler — so that loading it resumes
//! bit-for-bit. The format is [`bincode`] (compact, deterministic binary).
//!
//! **External media is referenced, not embedded.** The BIOS image, the disc
//! image, and a ROM cart's bytes are read-only and (in the BIOS/game case)
//! copyrighted and potentially hundreds of MB, so they are `#[serde(skip)]`'d
//! out of the snapshot. [`Saturn::load_state`] re-grafts the live media from
//! the running instance, and a small FNV-1a fingerprint of the BIOS/disc in
//! the header guards against restoring a state onto the wrong media.
//!
//! The on-disk layout is `(Header, Saturn)` bincode-encoded; the header's
//! magic + version make a stale or foreign file fail cleanly rather than
//! decode into garbage.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::cartridge::Cartridge;
use crate::memory::BiosRom;
use crate::system::Saturn;

/// File magic: "5thPlanet Save State".
const MAGIC: [u8; 4] = *b"5PSS";
/// Snapshot format version. Bump on any change to a serialized struct's shape;
/// rejects mismatches rather than attempting migration. v2 added the CD-block
/// drive-phase machine fields (`cdb.cpp`-faithful `Drive_Run` port). v3 added
/// the SH-2 INTC cached highest-priority-pending source (`Intc::best`). v4
/// added the VDP1 `Framebuffer::hires8` TVM-layout flag. v5 added the SMPC
/// port-device selection + Shuttle Mouse state (M13 E3). v6 added the
/// `SaturnBus::timing` per-access BSC bus-timing state (M12 task #8) —
/// serialized so a loaded state continues on the same bus timeline as the
/// original (the round-trip determinism contract). v7 added the CD-block
/// `results_read` latch (the Mednafen `ResultsRead` periodic gate). v8 added
/// `Saturn::scsp_debt` (fixed-chunk SCSP feed). v9 added `Vdp1::timing` (the
/// persistent draw-cycle state of the M12 #6 draw-duration model: clip/local
/// registers + the refresh-overhead residue) and
/// `BusTiming::bbus_write_finish` (the exact B-bus deferred-write
/// serialization, the M12 #8 residual). v10 reworked the SH-2 on-chip FRT/WDT
/// to the lazy/event-scheduled model: dropped the per-cycle prescaler
/// accumulators (`Frt::pre`/`Wdt::pre`) and added `OnChip::lastts` (the timer
/// epoch); `OnChip::next_ts` is derived (`#[serde(skip)]`). v11 changed the
/// SH-2 cache LRU representation from a per-set way-order array
/// (`[[u8; WAYS]; SETS]`, true LRU) to the SH7604's 6-bit pseudo-LRU state
/// (`[u8; SETS]`) — the hardware-faithful replacement order (fixes the
/// Sangokushi V instruction-cache-coherency hang). v12 `#[serde(skip)]`'d the
/// presentation-only audio buffers (`Scsp::out`, `CdBlock::cd_audio` +
/// `cd_audio_primed`) — `load_state` clears them, so they no longer bloat the
/// snapshot. v13 added the SMPC port-2 pad state (`Smpc::pad2`) so a 2-player
/// config reports each port's own buttons instead of mirroring player 1. v14
/// added the CD-block FAD-search result latch (`fad_search_fad`/`_spos`/`_pnum`,
/// the Mednafen `COMMAND_GET_FADSRCH` state read back by command 0x56). v15
/// added the SCSP DMA engine state (`ScspCtrl` `dmea`/`drga`/`dtlg` +
/// `dma_exec`/`dma_dir`/`dma_gate`, regs 0x412–0x416). v16 added the SMPC
/// 3D Control Pad analog channels (`Smpc::analog1`/`analog2`, the `[X, Y, L, R]`
/// bytes reported after the digital buttons by a `ThreeDPad` port — M13 E2).
const VERSION: u32 = 16;

/// Fixed-size prologue identifying the format and the media the state was
/// taken against.
#[derive(Serialize, Deserialize)]
struct Header {
    magic: [u8; 4],
    version: u32,
    /// FNV-1a of the BIOS image the state was captured with.
    bios_fp: u64,
    /// FNV-1a of the disc image, or `None` if no disc was inserted.
    disc_fp: Option<u64>,
}

/// Why a [`Saturn::load_state`] failed. Save never fails on valid state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveStateError {
    /// The bytes don't start with the 5thPlanet save-state magic.
    BadMagic,
    /// The file's format version isn't the one this build understands.
    VersionMismatch { found: u32, expected: u32 },
    /// The state was taken against a different BIOS than is loaded now.
    BiosMismatch,
    /// The state was taken against a different disc (or disc vs. no disc).
    DiscMismatch,
    /// The payload couldn't be decoded (truncated / corrupt).
    Decode(String),
}

impl fmt::Display for SaveStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveStateError::BadMagic => write!(f, "not a 5thPlanet save state"),
            SaveStateError::VersionMismatch { found, expected } => {
                write!(f, "save-state version {found} != supported {expected}")
            }
            SaveStateError::BiosMismatch => {
                write!(f, "save state was taken with a different BIOS")
            }
            SaveStateError::DiscMismatch => {
                write!(f, "save state was taken with a different disc")
            }
            SaveStateError::Decode(e) => write!(f, "corrupt save state: {e}"),
        }
    }
}

impl std::error::Error for SaveStateError {}

/// 64-bit FNV-1a over a byte slice (same constants as the SH-2 test harness).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

fn bincode_config() -> impl bincode::config::Config {
    bincode::config::standard()
}

impl Saturn {
    /// Serialize the full machine state to a `bincode` blob. External media
    /// (BIOS / disc / ROM cart) is referenced by fingerprint, not embedded.
    pub fn save_state(&self) -> Vec<u8> {
        let header = Header {
            magic: MAGIC,
            version: VERSION,
            bios_fp: fnv1a(self.bus.bios.image()),
            disc_fp: self.bus.cd_block.disc().map(|d| d.fingerprint()),
        };
        // (&Header, &Saturn) so the decode side reads them back in order.
        bincode::serde::encode_to_vec((&header, self), bincode_config())
            .expect("save state serialization is infallible for in-memory state")
    }

    /// Restore a snapshot produced by [`save_state`], replacing `self`.
    ///
    /// The currently-loaded media (BIOS, disc, ROM cart) must match what the
    /// state was captured with — it is re-grafted onto the decoded state,
    /// which never carries those (skipped) bytes itself.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), SaveStateError> {
        let ((header, mut loaded), _read): ((Header, Saturn), usize) =
            bincode::serde::decode_from_slice(bytes, bincode_config())
                .map_err(|e| SaveStateError::Decode(e.to_string()))?;

        if header.magic != MAGIC {
            return Err(SaveStateError::BadMagic);
        }
        if header.version != VERSION {
            return Err(SaveStateError::VersionMismatch {
                found: header.version,
                expected: VERSION,
            });
        }
        if header.bios_fp != fnv1a(self.bus.bios.image()) {
            return Err(SaveStateError::BiosMismatch);
        }
        let current_disc_fp = self.bus.cd_block.disc().map(|d| d.fingerprint());
        if header.disc_fp != current_disc_fp {
            return Err(SaveStateError::DiscMismatch);
        }

        // Move the live external media into the decoded state (it carries only
        // placeholders for these). Order matters: take from `self` before the
        // `*self = loaded` overwrite below.
        let bios = core::mem::replace(&mut self.bus.bios, BiosRom::new(vec![0xFF]));
        let disc = self.bus.cd_block.take_disc();
        let cart_rom = match &mut self.bus.cartridge {
            Cartridge::Rom { bytes } => core::mem::take(bytes),
            _ => Vec::new(),
        };

        loaded.bus.bios = bios;
        loaded.bus.cd_block.restore_disc(disc);
        if let Cartridge::Rom { bytes } = &mut loaded.bus.cartridge {
            *bytes = cart_rom;
        }
        // Presentation audio buffers may contain samples already generated
        // before the state was saved. Do not replay those stale samples after
        // loading into a new timeline.
        loaded.bus.cd_block.clear_audio_buffer();
        loaded.bus.scsp.clear_output_buffer();

        *self = loaded;
        Ok(())
    }
}
