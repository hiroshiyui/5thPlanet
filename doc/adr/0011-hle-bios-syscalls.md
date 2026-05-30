# 0011. HLE the BIOS system-call library for cold direct boot

- **Status:** Superseded (2026-05-30) — removed together with ADR-0010's
  HLE-boot path. `crates/saturn/src/bios_hle.rs` and `cold_hle_boot` no longer
  exist; the project boots only the real BIOS (LLE). Kept for the record.
- **Date:** 2026-05-29

## Context

HLE direct boot (ADR-0010) loads a game's 1st-read program and jumps to it; VF2's
own code then runs ~40k instructions and calls a BIOS **system call** —
`JSR @[0x06000320]`, the `ChangeSystemClock` slot of the Saturn BIOS SYS call
table — which, in our emulation, leads to the BIOS fatal handler and halts.

The Saturn BIOS exposes a library of ~20 SYS functions to games via a pointer
table in low work RAM (`0x06000200..0x06000360`): system-clock change, SCU/SH-2
interrupt set/get/mask, semaphores, CD init, and the backup-RAM (BUP) calls.
Games call them by `JSR @[table slot]`. We currently rely on the *real BIOS*
(run as LLE) to populate that table and implement those routines — but the BIOS
never reaches its game-launch state for us (it fails to boot the disc and gives
up), so the table ends up pointing at the fatal/unimplemented path. M11 Stage 1
showed this is **inject-independent**: there is no handoff point at which the
real BIOS has that table in a working game-launch state.

Every HLE Saturn emulator solves this the same way — it provides the SYS library
itself rather than depending on the BIOS. Yabause's `src/bios.c` is a complete,
readable reference: it builds the table in work RAM and dispatches each call to a
host implementation. This is the same kind of decision as ADR-0010 (HLE the boot
handoff) and the CD-block being HLE (CLAUDE.md): the one area where "never
approximate" is met by HLE rather than LLE.

## Decision

We will **high-level-emulate the BIOS SYS-call library** as part of the optional
cold HLE direct boot, in a new `crates/saturn/src/bios_hle.rs`:

- A **per-core dispatch hook** in the SH-2 step (`scheduler.rs`): when an
  `hle_sys_active` flag is set and the core's PC reaches a SYS entry address,
  run the host implementation (read args R4–R7, mutate the bus, set R0, return
  via `pc = pr`) instead of executing BIOS code. Off by default — the LLE boot
  path is untouched. (Enabled on both the master and the slave, since the SYS
  table is shared work RAM and the slave `JSR`s it too.)
- `Saturn::cold_hle_boot` writes the SYS call table (`0x06000200..0x06000360`)
  with the SYS entry addresses, enables the hook, loads the 1st-read, and jumps —
  reusing the real BIOS's hardware + interrupt-dispatch init, replacing only the
  broken SYS table.
- SYS functions are implemented **as a discovery effort** (M4-style): the ones a
  test game actually calls, starting with `ChangeSystemClock`, each cross-checked
  against `yabref/yabause/src/bios.c` — we do not invent behaviour.

## Consequences

- **Easier:** an HLE-booted game gets a working BIOS SYS environment independent
  of our (failing) BIOS boot, so it can progress past system calls toward
  gameplay. The LLE boot, the CD-block, and existing tests are unaffected (the
  hook is gated off by default; the `bios_boot` golden is unchanged).
- **Costs accepted:** a growing surface of HLE SYS functions to implement and
  keep faithful to `yabref bios.c`; coverage is per-game/iterative (unknown count
  up front, like M4); some SYS functions may touch state beyond the bus. MPEG SYS
  calls and a full no-BIOS cold boot stay out of scope (we still reuse the BIOS's
  hardware init).

## Result (updated 2026-05-30)

The SYS library now covers what VF2's launch exercises: `ChangeSystemClock`,
`Get/ClearSemaphore`, `Set/GetScuInterrupt`, `Set/GetSh2Interrupt`, and
`Set/ChangeScuInterruptMask` (the mask calls **master-only**, per Yabause
`!isslave`). Two further pieces, also RE'd from Yabause, were needed to get
*both* CPUs alive:

- **Slave start on `SSHON`** (`Saturn::release_slave`, Yabause
  `YabauseStartSlave`): the slave jumps to the game-written entry at
  `[0x06000250]` with `VBR = 0x06000400` and the slave stack, rather than
  resuming its stale (mid-BIOS-init) PC — which had re-run the cold work-RAM
  clear and wiped the loaded program.
- **Inter-CPU FRT input-capture (FTI)**: a 16-bit write to
  `0x0100_0000..0x017F_FFFF` (slave) / `0x0180_0000..` (master) pulses the
  target core's FRT input capture (`FTCSR.ICF`) — the wake signal a game uses to
  dispatch its slave.

With these, VF2's program survives, the slave runs the game's slave routine and
drains the master's work queue, and VBlank interrupts are delivered. It does not
yet render — the master then waits on a frame-sync flag (`[GBR+19]`) that is the
next handshake to resolve.

## Alternatives considered

- **Keep running the real BIOS for SYS calls** (the current hybrid) — rejected:
  the BIOS never reaches a game-launch state for us, so the SYS table is never
  valid (M11 Stage 1, inject-independent).
- **Full no-BIOS cold boot** (replicate the hardware + interrupt init too) —
  deferred: more code and risk for no extra benefit right now; reusing the BIOS's
  init and overriding only the SYS table is the smaller, targeted fix.
- **Fix the underlying BIOS-execution bug** (the LLE give-up root cause) — a
  needle-in-a-haystack with no tractable diff oracle; not a reliable path.
