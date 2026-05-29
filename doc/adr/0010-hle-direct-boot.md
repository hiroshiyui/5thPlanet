# 0010. Optional HLE direct boot

- **Status:** Accepted
- **Date:** 2026-05-29

## Context

M11's goal is to boot a commercial game to gameplay. Running the real BIOS
ROM (LLE) gets a disc *recognised* — for *Virtua Fighter 2* the BIOS
authenticates the disc, passes the region check, reads IP.BIN, and shows the
SEGA license screen — but then **fails to load the game's 1st-read program and
drops to the BIOS CD player**. The give-up decision lives in a ~200k-instruction
boot-loader blob the BIOS copies to work RAM; we localised it with a full-speed
PC tracer but pinpointing the one wrong CD-derived branch blind is impractical,
and the MAME PC-diff route is blocked on MAME's nvram/clock and multi-GB traces.

Meanwhile every piece a boot needs already works: the CD-block reads sectors,
the ISO9660 filesystem locates the 1st-read file (`AAAVF2.BIN`), and
region/authentication pass. The project is accuracy-first (ADR-0002), but the
**CD-block is already HLE** by necessity (the SH-1 firmware is undumped — see
`CLAUDE.md` / roadmap M7); the boot handoff sits in the same territory. The user
chose to prioritise playability over a fully-LLE boot.

## Decision

We will add an **optional** HLE direct-boot path, off by default, that loads the
disc's 1st-read program itself and jumps the master SH-2 to it — bypassing the
BIOS's CD boot loader. The **LLE BIOS boot remains the default and the
reference**; HLE boot is opt-in via the `--hle-boot` flag / `SAT_HLE_BOOT` env.

v1 is the **hybrid** model: let the real BIOS run (so it initialises the
hardware, the `SYS_*` call table, and the exception vectors), and when it is
about to drop to the CD player, perform the handoff — read IP.BIN's 1st-read
load address (`+0xF0`), load the 1st-read file into work RAM, and
`Cpu::hle_jump` the master to it (the game releases the slave itself). The core
lives in `Saturn::hle_boot` (`crates/saturn/src/system.rs`) and
`CdBlock::first_read_file` (`crates/saturn/src/cd_block.rs`); the frontend owns
the trigger.

## Consequences

- **Easier:** a game's own code now executes (VF2's 1st-read runs ~40k+
  instructions of its own program) — far past the CD player — reusing all the
  working CD/filesystem/BIOS-init code and adding only the load + handoff. The
  LLE path is untouched (HLE off by default; the `bios_boot` golden is
  unchanged).
- **Costs accepted:** v1 is BIOS-version-tuned (the give-up trigger keys on the
  JP v1.01 shell address range). The hybrid reuses the BIOS's *give-up* state,
  which is not a true game-launch state — so a game whose early init calls a
  `SYS_*` BIOS routine that depends on launch-specific BIOS state can stall
  (VF2 currently stalls in a BIOS SMPC-command poll during its loader). Closing
  that gap is follow-up work: set up the proper game-launch state (toward a
  cold/no-BIOS HLE boot) or fix the specific stuck SYS path.

## Alternatives considered

- **Cold / no-BIOS HLE boot** (deferred): set up the `SYS_*` table + hardware
  ourselves and never run the BIOS. More general and instant, but it must
  replicate BIOS state, is equally BIOS-version-dependent (the SYS routine
  addresses), and carries more game-compatibility risk; the hybrid reuses the
  real BIOS init for far less code as a first step.
- **Keep reverse-engineering the LLE give-up** — impractical: a
  200k-instruction RAM blob with no tractable diff oracle.
- **MAME master-PC diff** — blocked on MAME's nvram/clock setup and
  multi-GB traces; deferred.
