# Compatibility — boot-blocker investigations

The working tracker for commercial titles whose boot/run had to be investigated:
symptoms, findings, evidence, and ruled-out hypotheses. It holds **both** active
blockers and **resolved** cases retained as resume points / methodology
references; new blockers get added here as they surface. **Every entry below is
currently a ✅ resolved case — there are no titles actively under investigation.**

For the fully-working titles see
[`compatible-game-titles.md`](compatible-game-titles.md): *Virtua Fighter 2*,
*Doukyuusei ~if~*, *Sangokushi V*, and *Panzer Dragoon Zwei* are all fully
playable. SAN5's former intermittent per-scenario opening movie stall and PDZ's
former post-FMV exit + dead input are tracked below as **closed cases** kept for
their methodology. The references this work is checked against (Mednafen, MAME,
Yabause) are the never-committed local oracles described in
[`adr/0017-reference-oracle-policy.md`](adr/0017-reference-oracle-policy.md).

The Cinepak FILM path is now well-exercised: *Sangokushi V* (eighteen Cinepak
FILM files) was the first title to drive it through to gameplay, and *Panzer
Dragoon Zwei* is the second. (VF2's opening movie is **Duck TrueMotion**, a
different codec; all of these decoders are the games' own SH-2 software run by
LLE — no decoder to implement either way.)

---

## Panzer Dragoon Zwei (PDZ) — ✅ RESOLVED (fully playable)

- **Status:** **fully playable** (2026-06-27, user-confirmed): opening Cinepak
  FMV → title → main menu (NEW GAME / OPTIONS) → game, controller input working.
  See [`compatible-game-titles.md`](compatible-game-titles.md). Boots with no
  per-game hack in Mednafen (`mednaref/src/ss/db.cpp`), so both blockers were
  our-side fidelity gaps. Kept here as a closed case for the methodology.

Two distinct fixes, both found by diffing our behaviour against the Mednafen
oracle at the divergence:

### Fix 1 — post-FMV exit to the BIOS CD player (CD Seek 0x11 decode)
The opening FMV (which only began playing after the SAN5 DMA-halfword-skip +
SCU-delay-slot interrupt fixes) ran to completion, then the game tore it down and
**bailed to the BIOS CD player** via the Saturn disc-validity convention (BIOS
service `0x060007B0`; ROM `0x3BA6` verdict OK *iff* `status != 0xFF &&
(status & 0x20)`). Root: our CD **Seek (0x11)** handler was a port of MAME
`cmd_seek_disc` (FAD-vs-track keyed on `CR1 & 0x80`, track from `CR2 >> 8`) — a
*different model* from Mednafen `COMMAND_SEEK` (cdb.cpp:2851), where the seek
parameter is a single value `((CR1 & 0xFF) << 16) | CR2` (`0` = Stop, `0xFFFFFF`
= Pause, else a seek whose FAD-vs-track addressing is the `0x800000` marker bit,
resolved in `SeekStart1`). PDZ's post-FMV `Seek 1100,0200` (param `0x000200` =
track 2) hit the bogus track arm, which set `track` but **left `cd_curfad` at the
stale FMV head FAD** and **completed instantly** (no timed BUSY→SEEK→PAUSE), so
the disc-validity check sampled an unsettled drive → verdict `1` → CD player.
**Fix:** route the real-seek case through `start_seek(cmd_sp, 0x800000, 0, 0)` so
the phase machine runs and `cd_curfad` settles at the target (a bare seek's
`cur_play_end = 0x800000` makes `check_end_met` true on the first sector →
PAUSE). It surfaced only now because no prior game issued a plain *track-form*
Seek (BIOS/VF2 set the `0x800000` marker, which the buggy `CR1 & 0x80` test
happened to satisfy). Regressions: the rewritten Seek tests in
`crates/saturn/src/cd_block.rs`.

### Fix 2 — dead controller input (SMPC peripheral-only INTBACK)
At the title PDZ accepted **no** pad input while **VF2 read input fine**. Both
poll with the same `IREG0=00, IREG1=08, COMREG=10` INTBACK, but VF2 drives the
status+CONTINUE handshake whereas PDZ reads only OREG0 and re-issues. Our handler
**always** ran the INTBACK status phase (OREG0 = `0x80`) and armed the staged
CONTINUE, but INTBACK gates the two fetches independently: the status phase runs
only `if(IREG0 & 0xF)` and `SR_NPE` ("await CONTINUE") is set only inside it
(Mednafen `smpc.cpp:1217/1250`). A **peripheral-only INTBACK** (`IREG0 & 0xF ==
0`, `IREG1 & 0x8`) must return the pad report **directly in OREG0.. with no
CONTINUE** — ours put `0x80` where PDZ expects the `0xF1` port byte, so PDZ saw
"no controller". **Fix:** honour the `IREG0 & 0xF` gate
(`crates/saturn/src/system.rs` `drain_smpc`). Found with the new `SAT_SMPCLOG`
register-access logger (observer-only). Regression:
`intback_peripheral_only_returns_the_pad_directly_without_a_continue`
(`crates/saturn/tests/smpc.rs`) + the manual `pad_input_reacts` harness.

### Reference notes
- `mednaref/src/ss/db.cpp`: **no hack for PDZ** (boots on generic CDB fidelity).
- Mednafen's PROBLEMATIC-GAMES list notes PD2 "relies on illegal/questionable
  VDP2 window settings" — an **in-game rendering quirk to watch for**, not a boot
  blocker.

---

## Sangokushi V (三國志V) — playable, scenario-opening movie stall fixed

- **Status:** **playable** (intro FMV → title → menus → in-game strategy map; see
  [`compatible-game-titles.md`](compatible-game-titles.md)). The former
  intermittent per-scenario opening movie stall is fixed in the current tree
  by the SCU/SH-2 interrupt timing correction below; user-side interactive
  confirmation is still useful because the original symptom was timing- and
  input-path sensitive.
- **Image:** `roms/SANGOKUSHI_V.cue` (+ 8 tracks), KOEI, JP, serial **T-7623G**,
  BIOS v1.01. **No per-game hack in Mednafen** → our-side fidelity gap.

### Symptom
The **per-scenario opening introduction movie** sometimes fails to play and
**stalls the emulation**. It is **intermittent** — **resetting the emulator
usually bypasses it** and the game proceeds. The startup intro FMV, the title,
and the menus all run; this is the per-scenario opening movie specifically.

### Confirmed mechanism (investigated 2026-06-26)
**It is a core CD buffer-transfer deadlock, NOT a frontend pacing stall**, but
the CD block was only the downstream victim. The root cause was that the Saturn
aggregate sampled SCU interrupts before every master instruction, including
branch delay slots. A `Level0DmaEnd` interrupt could therefore be forwarded while
the SH-2 was executing the `nop` delay slot after an `rte` in the RAM interrupt
dispatcher (`0600094A: rte`, `0600094C: nop`). Hardware does not accept
interrupts inside delay slots.

In the deterministic bad run, the DMA transfer for sector FAD 51805 completed,
but the DMA-end interrupt was delivered at `PC=0600094C` instead of the next
post-slot boundary. The game's CD DMA completion path then failed to issue
`EndDataXfer`; the movie player kept reading until the 200-block CD buffer filled
(`free=0`, `parts=[0:200]`, observed freeze at FAD 52005). Both SH-2s kept
running, the FRT/FTI handshake stayed alive, and cache audit showed no stale
lines, which is why the failure initially looked like a missing CD protocol
transition.

The fix is to leave the SCU edge pending while `next_is_delay_slot()` is true and
forward it only at the first non-delay-slot instruction boundary. SCU `IST` also
remains software-visible until the guest clears it via write-0-clear; accepting a
vector consumes only the emulator's fresh edge.

### Deterministic repro
The core is deterministic given (RTC seed + pad stream), both captured by the
jupiter `SAT_INPUT_REC` movie — so a recording of a *stalling* session reproduces
the stall on every headless replay. The interactive "intermittency" is **not**
non-determinism in the core: it is purely **between-session** variation in the RTC
seed (host wall-clock at boot) and human pad timing — the only entropy the core
sees (single-threaded; no `rand`; the SMPC RTC is seeded then cycle-driven). A
savestate from a *good* timeline always plays; one from a *stalling* timeline
always stalls (it freezes one of the unlucky seed/timing combinations).
- **`sdbg replay <stall.rec>`** parks at `master 002D6B04, CD status=01 fad=52005
  free_blocks=0 parts=[0:200]` — bit-identical to the interactive freeze.
- **Fast repro:** a savestate taken ~frame 4300 (just before the read) + ~140
  frames forward with **no input** develops the stall.
- **Verified 100% reproducible:** loading the pre-stall savestate and running 400
  frames forward **4/4 times** produced bit-identical results — same master PC
  (`002DF042`), slave PC (`002D8E3E`), and CD state (`status=01 fad=52005 free=0
  hirq=0FCD`).
- **Post-fix validation:** loading the same pre-stall savestate now crosses the
  old freeze point. A 420-frame headless probe reached `fad=52604` with the CD
  buffer still draining (`free=175`, `parts=[0:25]`) and completed normally.

### Ruled out (each with evidence)
| Hypothesis | Verdict | Evidence |
| --- | --- | --- |
| Frontend pacing stall | ❌ | Audio watchdogs (`8ac18cb`) self-heal ≤1.5 s analytically; `SDLMOVIE` frames keep advancing *during* the stall |
| CD read-pump deadlock | ❌ | Buffer-full pause re-arms `sec_prebuf_in` at `cd_block.rs:1648`; resumes once the game frees a block |
| CD protocol gap after `CalcActualSize` | ❌ | The game stopped issuing `EndDataXfer` only after the DMA-end interrupt landed in the `rte` delay slot; delaying SCU interrupt delivery past the slot fixes the same repro |
| BFUL HIRQ latch (`a4df618`) | ❌ | `SAT_BFUL_READ_CLEAR=1` A/B — identical stall |
| **Cache coherency** (SAN5's usual signature) | ❌ | `sdbg caudit`: **0 stale lines on both CPUs**; the game's 182,942 associative purges are all honored |
| SCU DMA-halt removal (`64237d7`) | ❌ | Clean savestate bisection — the DMA-halt-restored build stalls identically |

So the stall was **not** a CD HLE command bug, a frontend queue problem, or a
cache-coherency regression; it was a pre-existing SCU interrupt-delivery fidelity
bug exposed by the CD DMA completion path.

### Discriminator (now run) — does `SDLMOVIE f=…` keep printing when it stalls?
- **continues, but `fad`/`parts`/PC stuck** → core CD/game wedge ✅ (this is what happens)
- `SDLMOVIE` also stops → frontend pacing / thread stall (ruled out)
- input dies only after a load → SMPC port-restore (`2a33f47`, addressed)

### Validation
- `SAT_LOADSTATE=tmp/san5_pre.state SAT_FRAMES=420 SAT_MOVIE_PROBE=60 cargo run
  --release -p jupiter --no-default-features -- ... SANGOKUSHI_V.cue` crosses
  the old `fad=52005 parts=[0:200]` freeze and reaches `fad=52604`.
- `cargo test -p saturn` passes. The SH-2 core already has
  `interrupt_not_accepted_inside_delay_slot`; this fix extends that invariant to
  the SCU forwarding layer.
- `cargo test -p jupiter --no-default-features` passes.

### History
Reached playable 2026-06-24 via two SH-2 cache-purge fixes (the FMV→menu deadlock
`35ce7e8` and the word-`CCR` menu-button purge `6215aab`) — cache coherency is
SAN5's signature failure class. Full chain in the commit messages and the
[`debugging-playbook.md`](debugging-playbook.md) SAN5 case study.
