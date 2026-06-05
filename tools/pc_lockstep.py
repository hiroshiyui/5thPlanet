#!/usr/bin/env python3
"""Interrupt-aware 68k instruction-lockstep of two PC streams (one instruction
per line, no loop-collapse): `tools/pc_lockstep.py ref.txt ours.txt`.

Each line is `PC` or `PC cycle` (the optional second column = the pre-instruction
68k accumulated cycle, captured uniformly so consecutive-cycle *deltas* are the
cost of each instruction). Both files start from a known-identical point (the
sound 68k's reset entry 0x1000).

On a PC mismatch one side took an interrupt the other hasn't yet (a benign
interleaving) — re-sync by advancing whichever side realigns (confirmed by a
short matching run), count it, continue. The first mismatch that cannot re-sync
within the window is the real divergence (ADR-0012).

When both files carry the cycle column, it also reports **cost-per-instruction**
mismatches over the aligned prefix: for each matched instruction the cost (cycle
delta) is compared, and a per-PC histogram of `(ours_cost vs ref_cost)`
mismatches is printed — directly naming the instructions ours mis-times (the
SCSP-timer-phase root). `mame_lockstep.py` is the MAME variant (loop markers).
"""
import sys
from collections import defaultdict


def load(path):
    pcs, cycs = [], []
    for l in open(path):
        l = l.split()
        if not l:
            continue
        pcs.append(l[0].upper().zfill(6))
        cycs.append(int(l[1]) if len(l) > 1 else None)
    return pcs, cycs


ref, refc = load(sys.argv[1])
ours, oursc = load(sys.argv[2])
have_cost = any(c is not None for c in refc) and any(c is not None for c in oursc)

K, R = 400, 6  # re-sync search window; matches needed to confirm a re-sync


def confirm(ri, oi):
    m = 0
    while m < R and ri < len(ref) and oi < len(ours):
        if ref[ri] != ours[oi]:
            return False
        ri += 1
        oi += 1
        m += 1
    return True


# cost (= cycle delta from the previous instruction) at index i, per stream.
# Deltas outside a sane single-instruction range are not instruction costs —
# they're the reference's periodic timestamp reset (Mednafen `ResetTS_68K`) or an
# exception-frame boundary; skip them (return None) so they don't swamp the diff.
def cost(cyc, i):
    if i <= 0 or cyc[i] is None or cyc[i - 1] is None:
        return None
    d = cyc[i] - cyc[i - 1]
    return d if 0 <= d <= 250 else None


ri = oi = resyncs = 0
samples = []
# per-PC cost mismatch tally: pc -> {(ours_cost, ref_cost): count}
cost_mismatch = defaultdict(lambda: defaultdict(int))
matched = 0
cost_checked = 0
drift = 0  # cumulative (ours_cost - ref_cost) over matched instructions

while ri < len(ref) and oi < len(ours):
    if ref[ri] == ours[oi]:
        if have_cost and matched > 0:
            oc, rc = cost(oursc, oi), cost(refc, ri)
            if oc is not None and rc is not None:
                cost_checked += 1
                if oc != rc:
                    cost_mismatch[ours[oi - 1]][(oc, rc)] += 1
                    drift += oc - rc
        matched += 1
        ri += 1
        oi += 1
        continue
    a = next((x for x in range(1, K) if oi + x < len(ours)
              and ours[oi + x] == ref[ri] and confirm(ri, oi + x)), None)
    b = next((x for x in range(1, K) if ri + x < len(ref)
              and ref[ri + x] == ours[oi] and confirm(ri + x, oi)), None)
    if a is None and b is None:
        print(f"\n*** REAL DIVERGENCE at ours instr {oi} (after {resyncs} benign interrupt re-syncs) ***")
        print(f"  ref={ref[ri]}  ours={ours[oi]}  (ref instr {ri})")
        break
    if a is not None and (b is None or a <= b):
        if resyncs < 8:
            samples.append(f"resync@ours{oi}: ours ran {a} extra (ours={ours[oi]}, rejoin ref={ref[ri]})")
        oi += a
    else:
        if resyncs < 8:
            samples.append(f"resync@ours{oi}: ref ran {b} extra (ref={ref[ri]}, rejoin ours={ours[oi]})")
        ri += b
    resyncs += 1
else:
    print(f"\nEND: ref {ri} / ours {oi} after {resyncs} re-syncs (no real divergence found)")

for s in samples:
    print("  " + s)

if have_cost:
    print(f"\n=== cost-per-instruction (cycle-delta) over {cost_checked} aligned instrs ===")
    print(f"cumulative drift (ours - ref): {drift:+d} cycles   "
          f"({len(cost_mismatch)} distinct PCs mis-timed)")
    # rank PCs by total cycle-drift contribution
    ranked = []
    for pc, d in cost_mismatch.items():
        n = sum(d.values())
        dd = sum((oc - rc) * c for (oc, rc), c in d.items())
        ranked.append((abs(dd), dd, n, pc, dict(d)))
    ranked.sort(reverse=True)
    print(f"\n  top mis-timed PCs (by |total drift|):")
    print(f"  {'PC':>6}  {'hits':>6}  {'Δcyc':>7}   (ours_cost -> ref_cost): count ...")
    for _, dd, n, pc, d in ranked[:25]:
        variants = "  ".join(f"{oc}->{rc}:{c}" for (oc, rc), c in sorted(d.items()))
        print(f"  {pc:>6}  {n:>6}  {dd:>+7}   {variants}")
