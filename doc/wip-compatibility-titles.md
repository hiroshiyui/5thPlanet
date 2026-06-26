# WIP compatibility — titles under investigation

A working tracker for commercial titles that **do not yet boot/run correctly**
in 5thPlanet, with the symptoms, findings, evidence, and ruled-out hypotheses
gathered so far. Each entry is a resume point, not a closed case.

For the fully-working titles see
[`compatible-game-titles.md`](compatible-game-titles.md): *Virtua Fighter 2* and
*Doukyuusei ~if~* are fully playable. *Sangokushi V* is **playable but not yet
fully** — it has one open intermittent blocker (the per-scenario opening movie
stall) tracked in its section below. The references this work is checked against
(Mednafen, MAME, Yabause) are the never-committed local oracles described in
[`adr/0017-reference-oracle-policy.md`](adr/0017-reference-oracle-policy.md).

Panzer Dragoon Zwei (below) **boots with no per-game hack in Mednafen** (checked
against `mednaref/src/ss/db.cpp`), so it's an **our-side fidelity gap**, not a
bad dump or a game that needs a quirk flag. It passes authentication and runs
real game code, then **stalls in the CD-driven Sega FILM / Cinepak movie player**
during its intro movie — the CD read pump freezes mid-`Play` with status stuck at
`PLAY`.

The Cinepak FILM path is **no longer unvalidated**: *Sangokushi V* (playable —
see [`compatible-game-titles.md`](compatible-game-titles.md)) was the
first title to drive it through to gameplay, its eighteen Cinepak FILM files
playing. PDZ's stall is therefore *likely* a **PDZ-specific FILM/timing issue**
rather than a wholesale gap in the player — but note SAN5 is **not yet fully
playable**: its per-scenario opening introduction movie also intermittently
stalls (a reset usually bypasses it), so the FILM path is not yet fully robust.
(VF2's opening movie is **Duck TrueMotion**, a different codec; all
of these decoders are the games' own SH-2 software run by LLE — no decoder to
implement either way. See the FMV note below.)

---

## Panzer Dragoon Zwei (PDZ) — PAUSED

- **Status:** paused by user choice (2026-06-11, low play priority). Root
  *mechanism* identified; one residual emulation bug remains.
- **Image:** `roms/pdzwei.cue/.bin` — dump verified bit-identical across two
  reads (audio byte-swapped at rip time; `.bak` kept). 4 tracks; a `PREGAP`
  directive on track 2 (parser caveat, not implicated).

### Symptom
Boots; the SEGA-PRESENTS and license screens render (so the 1st-read program
runs), then the game **silently falls back to the BIOS CD-player UI** at
~frame 870. Re-launching the application does not progress. **No CPU fault** on
either SH-2.

### Root mechanism (decoded over sessions 1–9)
The game's Sega CD library calls a **BIOS disc-validity service**
(via the system-table vector `[0x06000340]` → ROM `0x060007B0`) and spins at
`0x0604BF02` until that service writes an async status word. The service
requires a **stable PERIODIC CD report**: empirically (sdbg bp `0x3BAE`, the
`0x3BA6–0x3BBA` check) it is OK *iff* `status != 0xFF && (status & 0x20) != 0`
(i.e. a `PERIODIC`-flagged report; a paused drive yields identical `0x21`
reports). Verdict `1` → exit to CD player; `2` → continue.

Ours feeds the check a **COMMAND-response pair** (drive `PLAY`, status `0x03`,
no `0x20` bit) instead of a stable periodic report → verdict `1` → exit. The
"exit to CD player" is exactly the audio-CD/invalid-disc UX path.

### The residual our-side bug (next to fix)
At the moment of the check, our drive reports **`PLAY` with FAD frozen at 2041**
(intro movie `Play FAD 2035 × 7868`; only ~6 sectors delivered then freed). On
Mednafen the stream keeps buffering to full (~195 more sectors), transitions
`BUSY → PAUSE`, and the periodic then repeats identical `0x21` reports —
satisfying both the stability and the `0x20` requirement. **So: why does our
read pump freeze at FAD 2041 while status stays `PLAY`?** Suspects: `sec_prebuf_in`
stuck, `drive_counter` not re-armed after the `GetSectorData`/`EndDataXfer`/
`GetThenDelSector` dance, or a pause-with-status-`PLAY` path
(`crates/saturn/src/cd_block.rs`).

### Evidence
- CD delivery is **byte-perfect** through the first movie batch
  (`disc_read_content_check`: FAD 150 security header + FAD 2035 `FILM..P1.07`
  Sega FILM/Cinepak header match the `.bin` exactly).
- Give-up localized with `dump_giveup_state` (`CUE=pdzwei.cue FRAMES=1500
  CMD_LOG_TAIL=1024`), `SAT_CDSEEKLOG`, windowed master PC trace
  (`gen_vf2_pc_trace` pattern, `PCTRACE_LO=06000000`), and chained sdbg
  breakpoints into the BIOS service body.

### Landed
- **ResultsRead latch** (`3d8e8eb`, savestate v7) — the Mednafen-faithful
  `cdb.cpp ResultsRead` gate (CR report stays latched between host reads so two
  back-to-back reads match). Necessary (fixed torn CR reads that an earlier
  read-twice stability check tripped on) but **not sufficient** — PDZ still
  exits.

### Ruled out
CPU fault; dump quality (double-read verified); CD command protocol through
movie batch 1; sector content; commands `0x52`/`0x53`; the "PLAY-for-1-sector
read" Break-Point quirk (movie is a long play, PLAY is legitimate); filter
mode-bit mapping / reset defaults / connector chain.

### Reference notes
- `mednaref/src/ss/db.cpp`: **no hack for PDZ** (boots on generic CDB fidelity).
- Mednafen's PROBLEMATIC-GAMES list notes PD2 "relies on illegal/questionable
  VDP2 window settings" — an **in-game rendering quirk for later**, not the boot.

---

## Sangokushi V (三國志V) — playable, scenario-opening movie stall

- **Status:** **playable** (intro FMV → title → menus → in-game strategy map; see
  [`compatible-game-titles.md`](compatible-game-titles.md)) but **not yet fully
  playable** — one open, intermittent blocker. Active (not paused).
- **Image:** `roms/SANGOKUSHI_V.cue` (+ 8 tracks), KOEI, JP, serial **T-7623G**,
  BIOS v1.01. **No per-game hack in Mednafen** → our-side fidelity gap.

### Symptom
The **per-scenario opening introduction movie** sometimes fails to play and
**stalls the emulation**. It is **intermittent** — **resetting the emulator
usually bypasses it** and the game proceeds. The startup intro FMV, the title,
and the menus all run; this is the per-scenario opening movie specifically.

### Likely class
Another **CD-driven Sega FILM / Cinepak movie-player** stall — the same family as
the Panzer Dragoon Zwei blocker above. SAN5 already drives its eighteen Cinepak
FILM files to gameplay, so the player works for most movies; this is a
robustness / timing gap, not a missing feature. The intermittency points at a
timing or ordering race rather than a deterministic content bug.

### Next steps (resume point)
1. **Reproduce deterministically** — capture the stall as an input movie
   (`SAT_INPUT_REC`) and replay it headless (`sdbg replay --cart`); a fixed RTC
   seed + pad stream is essential given the intermittency.
2. **Trace to the divergence** — at the freeze, dump the CD read-pump / FILM
   player state (drive status, FAD, partition/FIFO occupancy, `cmd_log`) and diff
   against the Mednafen oracle at the same point (the LLE trace-to-divergence
   workflow). Compare with the PDZ findings above — status-stuck-`PLAY` + a
   read-pump freeze — which may share a root.

### History
Reached playable 2026-06-24 via two SH-2 cache-purge fixes (the FMV→menu deadlock
`35ce7e8` and the word-`CCR` menu-button purge `6215aab`) — cache coherency is
SAN5's signature failure class. Full chain in the commit messages and the
[`debugging-playbook.md`](debugging-playbook.md) SAN5 case study.
