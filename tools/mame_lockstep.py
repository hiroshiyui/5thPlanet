#!/usr/bin/env python3
"""Interrupt-aware 68k instruction-lockstep: ours' raw PC stream vs MAME's
loop-collapsed audiocpu .tr.  tools/mame_lockstep.py mame.tr ours_pc.txt

- "(loops for N)" markers: the loop matched its 2 shown iterations, so advance
  ours by N (deterministic).
- On a mismatch, one side took an interrupt the other hasn't yet (benign
  interleaving): re-sync by advancing whichever side realigns (confirmed by a
  short matching run), count it, and continue. The FIRST mismatch that cannot
  re-sync within the window is the real divergence.
"""
import re, sys

loopre = re.compile(r'\(loops for (\d+) instructions\)')
hexre = re.compile(r'^([0-9A-Fa-f]+):')
ours = [l.strip().upper().zfill(6) for l in open(sys.argv[2]) if l.strip()]
mtoks = []
for line in open(sys.argv[1]):
    m = loopre.search(line)
    if m: mtoks.append(('s', int(m.group(1)))); continue
    hm = hexre.match(line)
    if hm: mtoks.append(('p', hm.group(1).upper().zfill(6)))

K, R = 400, 6
def confirm(mi, oi):
    matched = 0
    while matched < R and mi < len(mtoks) and oi < len(ours):
        t = mtoks[mi]
        if t[0] == 's': oi += t[1]; mi += 1; continue
        if t[1] != ours[oi]: return False
        mi += 1; oi += 1; matched += 1
    return True

mi = oi = resyncs = 0
samples = []
while mi < len(mtoks) and oi < len(ours):
    t = mtoks[mi]
    if t[0] == 's': oi += t[1]; mi += 1; continue
    mpc = t[1]
    if mpc == ours[oi]: mi += 1; oi += 1; continue
    # mismatch: case 1 = ours ran extra; case 2 = mame ran extra
    a = next((x for x in range(1, K) if oi+x < len(ours)
              and ours[oi+x] == mpc and confirm(mi, oi+x)), None)
    b, j, cnt = None, mi, 0
    while cnt < K and j < len(mtoks):
        t2 = mtoks[j]
        if t2[0] == 's': j += 1; continue
        if t2[1] == ours[oi] and confirm(j, oi): b = (cnt, j); break
        cnt += 1; j += 1
    if a is None and b is None:
        print(f"\n*** REAL DIVERGENCE at ours instr {oi} (after {resyncs} benign interrupt re-syncs) ***")
        print(f"  mame={mpc}  ours={ours[oi]}")
        for k in range(max(0, oi-6), min(len(ours), oi+5)):
            print(f"    ours[{k}]={ours[k]}{'   <<<' if k == oi else ''}")
        sys.exit(0)
    if a is not None and (b is None or a <= b[0]):
        if resyncs < 6: samples.append(f"resync@ours{oi}: ours ran {a} extra (ours={ours[oi]}, rejoin mame={mpc})")
        oi += a
    else:
        if resyncs < 6: samples.append(f"resync@ours{oi}: mame ran {b[0]} extra (mame={mpc}, rejoin ours={ours[oi]})")
        mi = b[1]
    resyncs += 1
print(f"\nEND: ours exhausted at instr {oi} after {resyncs} re-syncs (no real divergence found)")
for s in samples: print("  "+s)
