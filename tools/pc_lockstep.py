#!/usr/bin/env python3
"""Interrupt-aware 68k instruction-lockstep of two *raw* PC streams (one PC per
line, no loop-collapse): `tools/pc_lockstep.py ref.txt ours.txt`.

Both files are line-per-PC dumps from a known-identical start (the sound 68k's
reset entry 0x1000). On a mismatch one side took an interrupt the other hasn't
yet (a benign interleaving) — re-sync by advancing whichever side realigns
(confirmed by a short matching run), count it, and continue. The first mismatch
that cannot re-sync within the window is the real divergence (ADR-0012).

Used for the Mednafen oracle (colon-less stream); `mame_lockstep.py` is the
MAME variant that also understands MAME's `(loops for N)` markers.
"""
import sys

ref = [l.strip().upper().zfill(6) for l in open(sys.argv[1]) if l.strip()]
ours = [l.strip().upper().zfill(6) for l in open(sys.argv[2]) if l.strip()]

K, R = 400, 6  # K = re-sync search window; R = matches needed to confirm a re-sync


def confirm(ri, oi):
    m = 0
    while m < R and ri < len(ref) and oi < len(ours):
        if ref[ri] != ours[oi]:
            return False
        ri += 1
        oi += 1
        m += 1
    return True


ri = oi = resyncs = 0
samples = []
while ri < len(ref) and oi < len(ours):
    if ref[ri] == ours[oi]:
        ri += 1
        oi += 1
        continue
    # mismatch: case a = ours ran extra (advance ours); case b = ref ran extra
    a = next((x for x in range(1, K) if oi + x < len(ours)
              and ours[oi + x] == ref[ri] and confirm(ri, oi + x)), None)
    b = next((x for x in range(1, K) if ri + x < len(ref)
              and ref[ri + x] == ours[oi] and confirm(ri + x, oi)), None)
    if a is None and b is None:
        print(f"\n*** REAL DIVERGENCE at ours instr {oi} (after {resyncs} benign interrupt re-syncs) ***")
        print(f"  ref={ref[ri]}  ours={ours[oi]}  (ref instr {ri})")
        lo, hi = max(0, oi - 6), min(len(ours), oi + 6)
        for k in range(lo, hi):
            mk = ref[ri - (oi - k)] if 0 <= ri - (oi - k) < len(ref) else "------"
            print(f"    [{k}] ours={ours[k]} ref={mk}{'   <<<' if k == oi else ''}")
        sys.exit(0)
    if a is not None and (b is None or a <= b):
        if resyncs < 8:
            samples.append(f"resync@ours{oi}: ours ran {a} extra (ours={ours[oi]}, rejoin ref={ref[ri]})")
        oi += a
    else:
        if resyncs < 8:
            samples.append(f"resync@ours{oi}: ref ran {b} extra (ref={ref[ri]}, rejoin ours={ours[oi]})")
        ri += b
    resyncs += 1

print(f"\nEND: reached ref instr {ri} / ours instr {oi} after {resyncs} re-syncs (no real divergence found)")
for s in samples:
    print("  " + s)
