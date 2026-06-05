# 0012. SCSP sound-driver HLE — design study

- **Status:** **Accepted — implementing** (2026-06-05). The accuracy-preserving
  alternative below — a **full 68k instruction-lockstep** vs the Mednafen LLE
  oracle — *was tried first* and banked a real **16× accuracy win** (per-instruction
  m68k cycle penalties, `5bd7131`: the lockstep's first divergence moved from
  instruction 14,509 to 230,967). But it did **not** fix the BGM in reasonable
  effort: the residual is a deep sub-instruction-timer-granularity rework, and the
  divergence sits ~frame 110, far before the BGM key-on at ~frame 592. That is
  exactly the trigger condition this ADR set for the **HLE fallback**, so we now
  implement it. Two go/no-go gates passed first: the **synthesis is LLE-correct**
  (the sine test ROM matches Mednafen `SS_SINETEST` to 0.2 % / 0.99972 correlation
  → synthesis *stays LLE*, the oracle), and the **68k-driven sine ROM proves our
  hosted 68k can drive the SCSP** → the HLE boundary ("produce the slot-register
  writes the LLE synthesis consumes") is viable. **M1 (opt-in scaffold + boundary
  proof) landed `b8d9870`** — the native driver keys a voice from the sequence's
  first note-on on the real audio-CD panel (avg amplitude 5792 where the LLE driver
  was 0). The implementation plan + remaining milestones (M2 sequence player, M3
  voice allocator, M4 CC/pitch, M5 instrument-bank RE vs `SS_KYONEX`) are the
  approved plan; the scope (B vs C) gate is resolved at M5.
- **Date:** 2026-06-05

## Context

The M12 BGM goal (the BIOS audio-CD CD-player panel should play its background
music) is stuck on the **LLE sound 68k**. This session landed two real,
oracle-validated timing fixes (the SCSP sound-RAM access wait-state, `729bfc3`;
the SCSP↔68k interleave budget-carry, `d755708`) and built a cross-emulator
signal "oscilloscope" (`c7efb89`+), which narrowed the divergence to the 68k
sound driver's own sequence/command state — but the root **recedes one driver
layer at every step** (activation timing → fade target → command byte → command
source …), each a real divergence, none yet a single fixable cause, and the
causal link to the actual silence (0 BGM voices keyed) still unconfirmed. The
cheap, high-confidence axes (cycle timing) are exhausted.

The project is **accuracy-first** (ADR-0002). The user has chosen to consider
deviating for this *optional* audio goal (`doc/bios-bgm-diagnosis.md` "Project
note"), via this design study.

**Directly relevant precedent — HLE was tried here and removed.** ADR-0010
(HLE direct boot) and ADR-0011 (HLE BIOS SYS-call library) were **superseded
(2026-05-30)** once the real **LLE** BIOS boot was fixed — VF2 and Doukyuusei ~if~
now boot on the LLE path. The lesson recorded there: the oracle (Mednafen) is
LLE, the project's superpower is the **LLE↔LLE PC-trace-diff**, and HLE removes a
subsystem from that validation. That argues for caution, not prohibition.

## The SCSP, decomposed — what can and cannot be HLE'd

The SCSP is two layers with very different character:

1. **Synthesis** — 32 slots (PCM/FM), the SCSP-DSP, the LFO, the mixer. This is
   *inherently low-level* DSP arithmetic: it reads slot registers + PCM/work
   data from sound RAM and produces 44.1 kHz samples. There is **no higher-level
   abstraction to "HLE"** — the synthesis *is* the low level. It **stays LLE**,
   and it already works (the `audio_pipeline` sine ROMs cross-check it to
   0.4 %/1.3 % vs Mednafen; the silence is upstream, not in synthesis).

2. **The 68k sound driver** — a program loaded into sound RAM by the BIOS/game.
   It reads master commands (sound RAM `0x500`/`0x700`), plays a **MIDI-like
   sequence** (the BGM data at `0x18200` is SMF-style: control-change `0xBn`,
   note events, delta-times, running status — see the dump in the diagnosis
   doc), runs per-channel/per-voice envelopes + a volume-fade engine, and
   **writes the SCSP slot registers** (key-on via `KYONB`/`KYONEX`, pitch,
   level). **This is the only HLE-able layer.**

So **"HLE the SCSP" means "HLE the 68k sound driver"** — nothing else.

## The HLE boundary

The boundary is forced by (1): the HLE must produce the **SCSP slot-register
writes** the LLE synthesis consumes. So an HLE replaces the 68k driver with a
native implementation of:

- the master→68k **command processor** (parse the `0x500`/`0x700` mailbox),
- a **MIDI-like sequence player** (parse `0x18200`-style data, tick it on the
  Timer-B cadence, handle delta-times / running status / loops / tempo),
- a **voice allocator + envelope/fade** driver writing the slot registers.

That is **most of the driver's logic — a full sequencer, not a thin shim.** HLE
genuinely *avoids the 68k execution/timing divergence* (the wall), but it does
**not** avoid the reverse-engineering: you must RE *what the driver does* to
reimplement it faithfully — the same RE, feeding a native player instead of a
68k fix.

## The critical complication: sound drivers are game-specific

The CD-block HLE works because the SH-1 presents **one fixed, documented host
interface** (CR1–4/HIRQ), identical for every disc. **The sound driver has no
such single interface** — it is loaded per title:

- the **BIOS** has its own built-in driver (what the CD-player panel uses);
- games ship **SEGA's standard sound driver** (the SBL/SGL "sound driver") *or*
  a **custom** one, each with its own master→68k command protocol and possibly
  its own sequence dialect.

So in practice this is **"HLE driver X," repeated per driver** — not "HLE the
SCSP" once. The leverage of an HLE depends entirely on **how widely a single
driver/format is shared.** The SMF-like sequence format is an encouraging signal
of a *SEGA standard* lineage, but **that is unverified** — and it is the hinge of
the whole decision.

## Options

| | Option | Pros | Cons |
|---|---|---|---|
| **A** | **Keep LLE** (status quo) | Accuracy-first intact; full trace-diff; the precedent (0010/0011) shows LLE wins when fixable; we are *close* (real bugs found, narrow divergence) | BGM may stay stuck behind slow, layer-by-layer 68k RE |
| **B** | **HLE the SEGA *standard* driver**, opt-in, synthesis stays LLE | One native reimplementation could cover the BIOS panel **and** many games; the HLE boundary lands exactly where trace-diff is hardest (68k driver execution); gated like the CD-block | Large (a full MIDI sequencer + voice/envelope model); per-driver RE; **custom-driver games get nothing**; departs from accuracy-first; repeats the removed-HLE pattern |
| **C** | **HLE the BIOS driver only** (narrowest) | Bounded; gets the current BGM target | **Zero leverage beyond the BIOS panel**; still a full sequencer reimplementation for a single driver — poor ROI |

## Recommendation

1. **Whatever is built must be strictly opt-in, with LLE the default and the
   oracle** — exactly how the CD-block is HLE while everything else is LLE, and
   how the removed HLE-boot was gated. The synthesis stays LLE regardless, so
   the synthesis trace-diff (the part we *can* validate) is preserved; only the
   driver layer — which is the part trace-diff *cannot* easily reach — goes HLE.

2. **Gate the scope decision on one cheap feasibility check, before any HLE
   code:** is the BIOS panel driver the **same** driver (code + command protocol
   + sequence format) that our target games use? Concretely — do VF2 and
   Doukyuusei ~if~ run the *same* sound-driver code (the `0x40F2` seq-tick handler,
   the `0x4570` channel processor, the SMF sequence format) as the BIOS? This is
   answerable by comparing their sound-RAM driver images.
   - **If shared** → Option **B** has real leverage; an opt-in standard-driver
     HLE is worth its accuracy cost.
   - **If custom per game** → Option **B/C** have poor ROI; **Option A (keep
     LLE)** is the wiser path, and the BGM either gets the continued LLE narrowing
     or is parked (it is optional).

3. **Honest leaning:** the precedent + the *closeness* of the LLE path make
   Option A defensible, but the user has asked to deviate, and the HLE boundary
   (driver vs synthesis) is unusually clean *for the SCSP specifically* — the
   silence is in the driver, not the synthesis, so HLE-ing the driver targets the
   exact stuck layer. **If we deviate, Option B (standard driver, opt-in) is the
   only one with leverage; Option C is not worth it.** Run the feasibility check
   first — it is cheap and it picks B vs A unambiguously.

## Decision

**Proceed with a practical HLE of the SCSP 68k sound driver.** Rationale (the
user's call): the project stays **accuracy-first** as its identity, but for a
*game-console* emulator **sound is essential** — it cannot be a permanent
blocker, and the LLE sound-driver path has become an open-ended,
recedes-every-layer debugging wall on an *optional-by-roadmap* but
*essential-by-product* feature. So the **68k sound driver** joins the **CD-block**
as a deliberate, documented HLE exception. The **synthesis stays LLE** (it works
and is trace-diff-validated); only the driver layer — exactly the layer
trace-diff cannot reach — is high-level-emulated.

Open sub-decisions, resolved before/while implementing:

1. **Scope (B vs C)** — confirmed by the *feasibility check*: do the target
   games (VF2, Doukyuusei ~if~, …) drive the SCSP via the **SEGA-standard** driver
   + SMF sequence dialect (→ Option B, one reusable HLE with real leverage), or
   custom per-game drivers (→ start with the BIOS panel, Option C, and grow)?
   The SMF format dump (`0x18200`) is the standard dialect, which is encouraging.
2. **Default vs opt-in** — start **opt-in** (LLE 68k remains the default + the
   reference, so the synthesis/driver trace-diff is preserved for validation),
   and flip the sound-driver HLE to **default-on** once it is mature enough that
   audio "just works" — since a silent default is exactly the block we are
   removing. The LLE 68k is *retained*, not deleted (unlike the CD firmware it is
   dumped), so it stays the oracle.
3. **Architecture** — a native module that (a) reads the master→68k command
   mailbox, (b) runs a **SMF sequence player** on the Timer-B cadence, (c) drives
   a voice allocator + envelope/fade that writes the **SCSP slot registers** the
   LLE synthesis consumes. Reverse-engineered from the LLE driver + cross-checked
   against Mednafen's *audible output* (the synthesis path is unchanged, so
   correct slot writes ⇒ correct sound).

When the scope is fixed this ADR moves to plain **Accepted** with the chosen
option recorded.

## Alternatives considered

- **HLE the synthesis too** — rejected: there is no higher-level model of PCM/FM/
  DSP synthesis to high-level-emulate; it *is* the low level, and it already
  works.
- **A full 68k instruction-lockstep** (LLE) — **CHOSEN to try first** (see
  Status). The accuracy-pure alternative to HLE: trace `(pc, full registers)` per
  68k instruction from a known-identical start point (SNDON / driver entry) on
  ours **and on two oracles, Mednafen *and* MAME**, then find the **first
  instruction where they diverge** — a different branch (PC mismatch ⇒ ours read
  different data or has a flag bug) or a different result on a matching PC
  (register mismatch ⇒ our m68k core miscomputes). That single instruction is the
  root; everything downstream (the value recession) follows from it. Dual oracles:
  if Mednafen and MAME agree and ours differs, the bug is unambiguously ours; if
  the two oracles differ, that itself is informative. Real tooling work (extend
  our itrace to full registers; extend Mednafen's `SS_ITRACE`; solve MAME headless
  `audiocpu` tracing — the GPU-debugger blocker noted in the diagnosis doc), but
  faithful, reusable, and it ends the recession.
