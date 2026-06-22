# WIP compatibility — titles under investigation

A working tracker for commercial titles that **do not yet boot/run correctly**
in 5thPlanet, with the symptoms, findings, evidence, and ruled-out hypotheses
gathered so far. Each entry is a resume point, not a closed case.

For the titles that **do** work, see the milestone notes in
[`roadmap.md`](roadmap.md): *Virtua Fighter 2* and *Doukyuusei ~if~* are both
fully playable (M11). The references this work is checked against (Mednafen,
MAME, Yabause) are the never-committed local oracles described in
[`adr/0017-reference-oracle-policy.md`](adr/0017-reference-oracle-policy.md).

Both titles below share a profile: **Mednafen boots them with no per-game hack**
(checked against `mednaref/src/ss/db.cpp`), so each is an **our-side fidelity
gap**, not a bad dump or a game that needs a quirk flag. Both pass authentication
and run real game code, then **stall in the CD-driven Sega FILM/Cinepak movie
player** (PDZ's intro movie; SAN5's KOEI-logo FMV) — a shared fragile subsystem,
though by different proximate mechanisms (PDZ = the CD read pump freezes mid-Play
with status stuck at `PLAY`; SAN5 = the master stalls between movie chunks on a
timing-dependent gate). The FILM-player ↔ CD/timing interaction is the common
suspect.

**Crucially, no fully-working title exercises the Cinepak FILM player, so it is
effectively unvalidated.** A disc scan for the Sega FILM / Cinepak markers
(`FDSC` description chunk, `cvid` Cinepak fourCC, `STAB` sample table) — and for
other movie codecs — shows:
- *VF2* = 0/0/0 Cinepak, but its opening **does** include a movie (scene 1):
  it's **Duck TrueMotion** (three `DUCK`-headed files; scenes 2–3 are real-time
  3D). A *different* codec and player from Cinepak FILM.
- *Doukyuusei ~if~* = 1/0/3 — at most one non-Cinepak FILM.
- *SAN5* = 18/18/21 — eighteen genuine Cinepak FILM files; *PDZ* uses Cinepak too.

So SAN5 and PDZ are the **first real users of the Cinepak FILM path**, which is
why both fail there while VF2/Doukyuusei don't. And VF2's TrueMotion movie
playing flawlessly is the *opposite* of a counterexample — it proves our
**CD-streaming + a software movie player work end-to-end**, so the defect is
specifically in the **Cinepak FILM player's timing-sensitive read/pacing logic**,
not in the CD block or movie streaming in general. (Both the Cinepak and
TrueMotion decoders are the games' own SH-2 software, run by LLE — no decoder to
implement either way; see the FMV note below.)

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

## Sangokushi V (SAN5 / 三國志V, KOEI, serial T-7623G) — OPEN

- **Status:** open (investigated 2026-06-22). Localized to a **timing-dependent
  control-flow divergence** in the master's main loop; exact root not yet found.
- **Image:** `roms/SANGOKUSHI_V.cue` (redumper multi-`FILE` CUE-BIN; the disc is
  fine — the cdrdao→redumper re-dump changed nothing). **JP BIOS v1.01.**

### Symptom
Boots the BIOS, then soft-hangs before the KOEI logo: **both SH-2s spin in
`0x060E_xxxx`, the framebuffer stays 100% black** (display + NBG0 are enabled).
Fully deterministic (same PC/cycle each run).

### Findings
- **Inter-CPU producer-consumer is correct for round 1.** The master wakes the
  slave (writes `0xFFFF` → `0x01000000` at master PC `060E4E40`) and posts a
  command in a shared mailbox (`060ED6xx`). The slave (released ~frame 850 via
  SMPC `SSHON`) reads a **non-zero** command (`R0=0x060DFC46` at slave PC
  `060E4E16`), takes the work path, processes it, acks (`0x01800000` at
  `060E4E2C`), and parks polling `FTCSR.ICF`. The init script (mailbox commands
  4→5) completes normally.
- **The master then stalls in its main game loop** (`0601_99xx` — a sequence of
  per-frame update-fn `JSR`s: `060E179C / 060E46E8 / 060E4672 / 060E4A2C`,
  pointers in the literal pool at `06019A30`). The loop runs every frame but the
  **scene state machine never advances** to the logo. The stall is inside the
  call graph rooted at **`060E4672`**.
- **Master *speed* changes the PATH** (fast → no-logo → deadlock; slow → logo →
  deadlock), so a **timing-dependent branch** several call-levels deep drives the
  divergence.
- **★ The blocker is the FILM/Cinepak movie player — the KOEI logo is an FMV
  (same subsystem PDZ dies in).** At the main-loop scene gate `060199B2` the
  registers show **R2 = `0x46494C4D` = "FILM"**, and the gate fn (`060E4A2C`,
  called at `060199AE`) returns **R0 ≠ 1** ("scene not done") *every iteration*
  → the scene-advance block at `060199B6` is always skipped. (`060E4A2C` loops
  over up to 32 display objects and reports "done" only when every object's
  state field `== 1`; one object — the FILM/movie — never reaches `1` because
  playback stalled.) The main loop keeps
  running (PC sequence at the hang cycles `060199AC → …4A2C… → …49D6… → 06019A5E
  → 060EA3xx`, confirmed by a raw 180-PC `SAT_INLOOP` capture — an earlier
  "`060199AC` hit once" reading was the `fc`-then-`c` debugger-stall artifact,
  not reality), but the FILM player never advances the movie.
- **The movie read stalls.** The CD command ring (`@0601411A`) shows the master
  reading movie sectors via the buffer/partition dance (`CalcActualSize →
  GetSectorData → EndDataXfer → DeleteSectorData → GetBufStat`); `SAT_CDSEEKLOG`
  shows the movie read in **count-limited chunks** (…, `fad=5092 count=141`,
  `fad=5234 count=177 → end_fad=5411`), then **no further chunk reads** (drive
  PAUSEd at FAD 5411, partitions drained, `GetBufStat → 0`). So the FILM player's
  per-chunk advance is gated off by a **timing-dependent condition** our fast
  master never satisfies; `SAT_SLOW_FETCH` lets it pass (logo renders briefly)
  then fails a later chunk. The exact gate (likely a movie-frame pacing /
  decode-done / per-chunk slave-dispatch condition — note the slave was
  dispatched only once, frame 877) is in the `060E46xx–060E4Axx` FILM-player
  code and `060E4A2C` — not yet pinned.
- **★ It is an upstream divergence, NOT a CD Play-count parse bug (Mednafen
  cross-check).** From the *same* movie start FAD 5234 (`0x1472`), Mednafen FULL
  (`SS_CDTRACE`) issues `Play count=0x323`(803) then `0x3deb`(15851) — it reads
  the **whole movie**; ours (`SAT_CDSEEKLOG`) issues a read ending at FAD 5411
  (**count 177**) then stops. The commands genuinely differ (ours' end-CR
  `0x1523` vs Mednafen's `0x0323`), so the CD-block's count/Play handling is
  *correct* — the **game itself computes a different, truncated movie read** in
  our run, i.e. the FILM player's read-size (which depends on diverged
  master/frame-timing state) is wrong because of the upstream timing
  divergence. So the root is still the timing-dependent control flow, now seen
  to corrupt the FILM player's read-length calculation.

### Ruled out (with evidence)
| Hypothesis | Verdict / evidence |
|---|---|
| SH-2 cache coherency | **No** — the `sdbg stale` detector found **0 stale of 486M (master) + 249M (slave) cache reads**; the cache is always coherent. |
| SH7604 cache LRU | Was a true LRU vs the hardware 6-bit pseudo-LRU; **ported (a real accuracy fix) but it does NOT fix SAN5.** |
| Timing magnitude ("master too fast") | **No** — `SAT_SLOW_FETCH` slowdown renders the logo *transiently* (N=7, frame 1800) but the SAME deadlock returns by frame 3000. Slowing only **delays** it. |
| Watchdog timer | **No** — WTCSR=00 (TME=0) on both cores; SAN5 leaves the WDT disabled. |
| FRT interrupts | **No** — TIER ICIE=0 on both cores. |
| VDP1 draw-end | **No** — VDP1 `drawing=false` at the hang. |
| Broken FTI handshake | **No** — round 1 is fully correct (above). |
| Wrong scene variable | **No** — `SS_MEMDUMP` of the command mailbox (`060ED5F0..060ED650`) is **byte-identical** between ours (stuck) and Mednafen FULL (progressed to the logo). The init state matches; only the *execution path* diverges. |

Mednafen plays SAN5 in **both** its cache modes (`-ss.dbg_cem` default *and*
`full`), so it isn't cache-mode-fragile there — our timing model has the gap.

### Landed (accuracy, not the SAN5 fix)
- **SH7604 6-bit pseudo-LRU** (`13454b6`, savestate v10→v11, golden-invariant;
  VF2 + Doukyuusei still render).

### Debug instruments built (reusable, golden-safe)
- sdbg `cache` (CCR + hit/miss + purge counts), `frt` (FTI/FTCSR + WDT state),
  `caudit` (cache-vs-memory line audit), `stale` (per-access stale-read
  detector via `Bus::peek16`) — commits `bd2f78b`, `0ee6553`, `f1fc8c7`.
- CPU-tagged `SAT_FTILOG` (`SaturnBus::cur_is_slave`) — names the core issuing
  each inter-CPU FTI pulse — commit `f1fc8c7`.
- `SAT_SLOW_FETCH=N` headless timing-probe knob — commit `d257a22`.

### Next phase (sustained, separate effort)
Find the control-flow divergence: (a) make the **PC-stream tdiff vs Mednafen
`SS_PCTRACE` work** — the blocker is the loop-collapse + delay-slot/`+4`
alignment of `run_for_traced`'s trace, not the bug; a clean diff yields the first
divergent master PC directly. Or (b) keep tracing `060E4672` → … with `pctrace`
register capture to the conditional branch and what it reads. Or (c) extend the
cross-emulator **signal scope** (`Scsp::enable_scope`, today SCSP-only) to sample
the master's branch input on both emulators and overlay. The fix (matching the
underlying master/peripheral rate the branch depends on) is itself a hard
cycle-accuracy problem — see [`roadmap.md`](roadmap.md) M12/M13.
