# 0023. Save-file keying: save states per game disc, internal backup RAM per BIOS

- **Status:** Accepted
- **Date:** 2026-06-26

## Context

The jupiter frontend persists two kinds of save data as sibling files: the machine
**save states** (`.state` / `.<n>.state` — a `bincode` snapshot of the whole
machine, [ADR-0018](0018-save-state-design.md)) and the **internal backup RAM**
(`.bup` — the console's built-in 32 KiB battery-backed memory card). Both were
keyed to the BIOS image path (`Session::save_base`), so every game launched on a
given BIOS shared one set of save-state slots and one backup-RAM file.

Two hardware facts make a single BIOS-keyed namespace wrong for one and right for
the other:

- A **save state** is a snapshot of *one specific game* at a moment. Its header
  carries a disc fingerprint and `load_state` rejects a load onto different media
  (ADR-0018). Sharing `<bios>.0.state` across games meant saving game B to slot 0
  clobbered game A's slot 0, and a cross-game load failed the fingerprint check
  anyway — the slots were effectively unusable across more than one game.
- The **internal backup RAM** is, on real hardware, a *single shared memory card
  built into the console*. The BIOS formats and manages it at power-on (the
  "BackUpRam Format" tag) and every game reads/writes the same card via the BIOS
  Backup Manager. It is a console resource, not a per-game one.

## Decision

We will **key save states to the loaded game disc image**, and **keep the internal
backup RAM (`.bup`) keyed to the BIOS**.

- Save-state paths derive from `Session::state_base()` (computed from the launch
  disc spec `launched_spec` via the testable free function `state_base_for`): the
  disc image path when a disc image is loaded, falling back to the BIOS base for a
  live `cdrom:` drive or a no-disc boot. Each game owns its quicksave and OSD save
  slots beside its disc image (`<disc>.state` / `<disc>.<n>.state`).
- The `.bup` keeps deriving from `save_base` (the BIOS path). A BIOS power-cycle
  re-keys the `.bup` to the new BIOS; save states follow the disc (the same disc is
  re-inserted), independent of the BIOS.

Invariant: **per-game data (save states) keys to the disc; shared-console data
(the internal backup card) keys to the BIOS.** A diff that keys the `.bup` per-disc,
or save states per-BIOS, violates this ADR.

## Consequences

- Save-state slots are correctly isolated per game; the cross-game slot-collision
  and fingerprint-mismatch failures are gone.
- The shared-memory-card model is preserved — a game (and the BIOS Backup Manager)
  sees the one console card, as on hardware.
- **Breaking for existing saves:** pre-existing `<bios>.*.state` files are orphaned
  (games now look for `<disc>.*.state`). No automatic migration; a user who wants
  an old state copies it next to the disc image. Save states are ephemeral, so we
  accept this.
- Save states now write *beside the disc image*; a read-only image directory makes
  a save fail where it previously succeeded next to the BIOS — it degrades
  gracefully (an OSD "Save failed" toast / a stderr line).
- **Does not** address a game complaining about *insufficient backup RAM* (a
  separate axis): the internal card stays 32 KiB. The hardware-faithful path for
  more space is a backup-RAM cartridge (`--cart=bram[4|8|16|32]`), whose contents
  are not yet persisted to their own file — a follow-up.

## Alternatives considered

- **Key the `.bup` per-disc too** (the original proposal). Rejected: it breaks the
  shared-console-card model — the BIOS formats one card at power-on and games
  expect to read each other's data through it, so a per-game `.bup` would show a
  game the wrong (empty/foreign) card and defeat the Backup Manager.
- **Keep everything BIOS-keyed** (status quo). Rejected: it leaves save-state slots
  unusable across more than one game on the same BIOS (collision + fingerprint
  rejection).
- **A per-game subdirectory / index keyed by disc serial.** Rejected as
  over-engineered: the disc image path is already a stable, user-visible key, and
  keying beside the image matches user expectations and the existing `.bup` / state
  sibling-file convention.
