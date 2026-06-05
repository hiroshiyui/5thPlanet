#!/usr/bin/env python3
"""Lockstep ours' raw 68k PC stream against MAME's loop-collapsed audiocpu .tr.
    tools/mame_lockstep.py mame.tr ours_pc.txt
MAME collapses repeats as "<2 iters shown>\\n   (loops for N instructions)".
We don't expand — we match PC lines line-for-line and, at a marker, advance
ours' pointer by N (a deterministic loop that matched its 2 shown iterations
matches the rest by construction). First mismatch = the real divergence.
"""
import re, sys

loopre = re.compile(r'\(loops for (\d+) instructions\)')
hexre = re.compile(r'^([0-9A-Fa-f]+):')

ours = [l.strip().upper().zfill(6) for l in open(sys.argv[2]) if l.strip()]
oi = 0          # index into ours
mame_n = 0      # MAME instruction count (incl. collapsed)
for line in open(sys.argv[1]):
    m = loopre.search(line)
    if m:
        n = int(m.group(1)); oi += n; mame_n += n; continue
    hm = hexre.match(line)
    if not hm:
        continue
    mpc = hm.group(1).upper().zfill(6)
    if oi >= len(ours):
        print(f"ours stream ended (oi={oi}, mame still going at instr {mame_n})"); break
    if mpc != ours[oi]:
        print(f"FIRST DIVERGENCE at ours instr {oi} (mame instr ~{mame_n}):")
        print(f"  mame={mpc}  ours={ours[oi]}")
        print("  ours context:")
        for j in range(max(0, oi-6), min(len(ours), oi+5)):
            print(f"    ours[{j}]={ours[j]}{'   <<< DIVERGE' if j == oi else ''}")
        break
    oi += 1; mame_n += 1
else:
    print(f"IDENTICAL: ours matched all of MAME up to ours instr {oi} (mame {mame_n})")
