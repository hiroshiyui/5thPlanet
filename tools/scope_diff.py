#!/usr/bin/env python3
"""Cross-emulator signal-scope overlay/diff.

Reads two scope CSVs (produced by 5thPlanet's `SCOPE_*` probe and the matching
mednaref `scope_tick`), aligns them row-for-row on the shared timebase (one row
per trigger-PC hit), and for each channel reports the first row where ours and
Mednafen diverge, plus a downsampled sparkline of both traces.

    tools/scope_diff.py ours_scope.csv mfn_scope.csv

The two captures must use the same SCOPE_PC / SCOPE_CH so the channels and rows
line up.
"""
import sys


def load(path):
    names, rows = [], []
    for line in open(path):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        p = line.split()
        if p[0] == "row":
            names = p[1:]
            continue
        rows.append([int(x, 16) for x in p[1:]])
    return names, rows


def spark(vals):
    blocks = "▁▂▃▄▅▆▇█"
    lo, hi = min(vals), max(vals)
    rng = (hi - lo) or 1
    return "".join(blocks[min(7, (v - lo) * 8 // rng)] for v in vals)


def downsample(col, width=72):
    if len(col) <= width:
        return col
    step = len(col) / width
    return [col[int(i * step)] for i in range(width)]


def main():
    if len(sys.argv) < 3:
        print("usage: scope_diff.py ours.csv mfn.csv")
        return
    na, ra = load(sys.argv[1])
    nb, rb = load(sys.argv[2])
    print(f"ours = {sys.argv[1]}: {len(ra)} rows, channels {na}")
    print(f"mfn  = {sys.argv[2]}: {len(rb)} rows, channels {nb}")
    if na != nb:
        print(f"!! channel mismatch: {na} vs {nb}")
    n = min(len(ra), len(rb))
    print(f"\n=== first divergence per channel (over {n} aligned rows) ===")
    for ci, name in enumerate(na):
        first = next((i for i in range(n) if ra[i][ci] != rb[i][ci]), None)
        if first is None:
            print(f"  {name:12s}: IDENTICAL over all {n} rows")
        else:
            print(
                f"  {name:12s}: FIRST DIVERGE @row {first}: "
                f"ours={ra[first][ci]:X} mfn={rb[first][ci]:X} "
                f"(Δ={ra[first][ci] - rb[first][ci]:+d})"
            )
    print(f"\n=== sparkline overlay (downsampled to ~72 cols, full row range) ===")
    for ci, name in enumerate(na):
        oc = downsample([ra[i][ci] for i in range(n)])
        mc = downsample([rb[i][ci] for i in range(n)])
        print(f"  {name:12s} ours {spark(oc)}")
        print(f"  {'':12s} mfn  {spark(mc)}")


if __name__ == "__main__":
    main()
