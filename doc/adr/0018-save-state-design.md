# 0018. Save states are a whole-machine bincode snapshot with external media referenced, not embedded

- **Status:** Accepted
- **Date:** 2026-06-19 (retroactively recorded; the decision dates to M8)

## Context

M8 needs save/load (and, as a side effect, a strong correctness check). The
machine state is large and graph-shaped — two SH-2s with caches, the 68k, two
DSPs, VDP1/2 VRAM/CRAM/framebuffers, SCSP sound RAM, the CD-block engine, the SCU.
Three pieces of state are different in kind: the **BIOS ROM**, the **disc image**,
and a **ROM cartridge's bytes**. These are large, copyrighted, externally
supplied, and — crucially — **already present** at load time (the frontend holds
the live BIOS/disc when it asks to load a state).

Embedding that external media in every snapshot would make saves huge and would
bake copyrighted bytes into save files. The cores (`sh2`, `m68k`, `scu_dsp`) are
also meant to stay dependency-free when used standalone, so serialization must be
optional for them.

## Decision

We will serialize the **whole machine with `bincode`.** Every state type derives
`Serialize`/`Deserialize`; in `saturn` the derive is unconditional, while
`sh2`/`m68k`/`scu_dsp` gate it behind an optional `serde` feature that `saturn`
turns on (keeping the cores dependency-free standalone).

**External media is referenced, not embedded:** `BiosRom.rom`, `CdBlock.disc`,
and `Cartridge::Rom.bytes` are `#[serde(skip)]`'d and **re-grafted from the live
instance** in `load_state`. A magic + monotonic **version** header plus FNV-1a
**BIOS/disc fingerprints** reject a load onto the wrong media or an incompatible
format. **Determinism is the contract:** the round-trip test boots, snapshots,
then runs the snapshot and the original forward by the same budget and asserts
**identical re-snapshots** — so a save/load that silently perturbs state fails CI.

## Consequences

- **Easier:** small save files; no copyrighted media in saves; and the
  determinism round-trip becomes a powerful, always-on correctness check that has
  repeatedly caught state that wasn't serialized (or wasn't serialized
  deterministically).
- **Cost we accept:**
  - A save is loadable **only against the same media** (fingerprint-gated), and
    `load_state` must be handed the live machine to re-graft BIOS/disc/cart — a
    state is not a standalone, media-free artifact.
  - **No cross-version migration**: the `version` field invalidates old saves on a
    format bump (it has reached **v9** as timing/IL fields were added). This is an
    accepted simplicity trade for an emulator in active development.
  - Every new serialized field must round-trip **deterministically**. Arrays > 32
    need `serde-big-array`; scu_dsp's `[[u32;64];4]` data RAM needed a bespoke
    `no_std` flat-tuple codec (big-array is 1-D only). Non-determinism here breaks
    the round-trip test by design.
- **Performance footnote:** the disc fingerprint is computed once at disc
  construction and stored, not recomputed per save/load (a 600–700 MB image made
  every quicksave stall ~1.5 s otherwise) — the precompute-off-the-hot-path
  pattern, regression-tested as bit-identical.

## Alternatives considered

- **Embed the external media in the save.** Rejected: multi-hundred-MB save
  files and copyrighted bytes in every snapshot, for no benefit (the media is
  already present at load).
- **A hand-rolled binary format.** More control over layout and migration, but
  far more code and a manual (de)serializer per type to keep in sync; `bincode` +
  `derive` is faithful and cheap, and the determinism round-trip test guards
  against the "forgot to serialize field X" class that a hand format would also
  face.
- **Versioned migrations across formats.** Rejected for now as premature: the
  machine model still changes often (savestate v6→v9 within M12 alone), so a
  reject-on-mismatch version gate is the honest, low-cost choice until the format
  stabilizes.
