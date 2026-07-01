# 0028. Backup-RAM cartridge as multiple virtual memory cards

- **Status:** Accepted
- **Date:** 2026-07-01

## Context

The Saturn's rear expansion slot can hold a **battery backup-RAM cartridge**
(`Cartridge::Bram`, up to 4 MiB) that games use for extra save space when the
console's built-in 32 KiB backup RAM is full (e.g. strategy titles like
Sangokushi V). [ADR-0023](0023-save-file-keying.md) settled how save media is
keyed: **save states key to the game disc** (per-game), the **internal backup
RAM `.bup` keys to the BIOS** (a single shared console card). Until now the
backup *cartridge* held its saves only in RAM — lost on exit.

We first added a single host-persisted file (`<bios>.crtbup`, keyed to the BIOS
like the internal `.bup`). But a single cartridge image is the wrong model: a
real player accumulates *multiple* physical memory carts and swaps whichever one
they need into the slot — exactly the PlayStation memory-card experience. One
file forces every game's saves to share one 4 MiB card and offers no way to
create a fresh card, keep several in parallel, or discard one.

The core already exposes the cartridge's raw battery buffer
(`Saturn::cartridge_backup`/`load_cartridge_backup` → `Cartridge::bram_bytes`/
`load_bram_bytes`); the missing piece is a frontend policy for *which* image
backs the slot and a way to manage a set of them. The OSD already has the
building blocks: a slot-picker screen (the save-state `Slots` screen), a
`Cartridge` settings screen, and the core-free action/context pattern
([ADR-0008](0008-frontend-osd-software-composite.md)).

## Decision

We will model the backup-RAM cartridge as **a set of virtual memory cards**,
each a separate host file, managed from an OSD submenu.

- **Files.** Card `n` persists to `<save_base>.<n>.crtbup`, keyed to the BIOS
  (`save_base`), mirroring the internal `.bup` — both are console-level battery
  cards, not per-game (ADR-0023). A fixed set of `BACKUP_CARDS` slots (8) is
  exposed. This supersedes the interim single `<bios>.crtbup` scheme.
- **Active card.** The frontend config gains `cart_card` (default 0): the card
  currently in the slot. Every place the cartridge battery is loaded or saved
  (startup, exit, BIOS power-cycle, cartridge swap, headless) routes through
  `<save_base>.<cart_card>.crtbup`.
- **Manager submenu.** The OSD `Cartridge` screen gains a **Backup RAM Cards…**
  entry into a manager screen listing every slot with a used/empty mark and the
  active marker. Each slot opens a per-card screen offering, per the slot's
  state:
  - **Create & Use** (empty slot) — write a fresh formatted image and make it
    active.
  - **Select** (used slot) — persist the outgoing card, make this one active,
    load it into the live cart (plugging the Backup-RAM cart first if needed).
  - **Delete** (used slot) — remove the file; if it is the active card, eject
    the backup cart so there is no active-but-fileless state.
- **Core stays media-agnostic.** All filesystem and card-set logic lives in the
  frontend; the OSD names cards and emits `SelectBackupCard`/`CreateBackupCard`/
  `DeleteBackupCard` actions, and the core only ever sees "here are the bytes
  for the plugged cart" — consistent with the OSD's core-free contract.

## Consequences

- A player can keep several independent backup carts, create a clean one, switch
  between them, and delete stale ones — without touching the internal card or
  the per-game save states.
- The cartridge's saves now survive across runs (the original motivation), and
  the "insufficient backup RAM" class of game complaints (ADR-0023) is fully
  addressed: plug Backup RAM, pick a card, and it persists.
- Selecting/creating a card mid-session hot-swaps the live cart's contents
  without a machine reset when the Backup-RAM cart is already plugged (a game
  re-reads the card on its next access, like inserting a different memory card);
  plugging the cart for the first time still resets, matching the existing
  cartridge-swap behaviour.
- Cost: a fixed 8-slot ceiling (a deliberate simplification over a dynamic,
  named card list) and 8 potential 4 MiB files per BIOS. Card size is fixed at
  the OSD default (4 MiB); per-card size selection is not offered.
- The interim single `<bios>.crtbup` file is replaced by `<bios>.0.crtbup`.
  Because the single-file scheme was never released, no migration is provided.

## Alternatives considered

- **Keep the single `<bios>.crtbup` file.** Simplest, but a real memory-card
  workflow (multiple cards, create/remove) is exactly what a shared 4 MiB card
  cannot express — the whole reason the user asked for it.
- **Per-game backup carts** (key the card to the disc like save states). Rejected
  for the same reason ADR-0023 rejected per-game `.bup`: a backup cartridge is a
  physical card a player moves *between* games, so keying it per game would break
  the shared-card model a game's save browser expects.
- **A dynamic, named card list** (arbitrary count, user-chosen names). More
  faithful to a PC memory-card manager, but the menu-driven OSD has no text
  entry, and a fixed indexed set matches the existing save-state slot UI the
  player already knows. Can be revisited if the fixed ceiling proves limiting.
