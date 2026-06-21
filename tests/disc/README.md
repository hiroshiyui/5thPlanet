# 5thPlanet homebrew test disc

A **royalty-free** Sega Saturn test disc for evaluating emulator accuracy,
built with [libyaul](https://github.com/yaul-org/libyaul) (MIT). It boots on
the normal real-BIOS path — a libyaul `IP.BIN` carries the `SEGA SEGASATURN`
header 5thPlanet's authentication checks, so no special-casing is needed — runs
a set of feature checks, and posts a machine-readable result that the headless
harness asserts.

This is the full-disc counterpart to the in-process `audio_pipeline.rs`
"program" test: it exercises the *real* boot path (auth → `IP.BIN` → 1st-read
program), the ISO9660 filesystem, and multi-chip interplay.

Because everything here is our own MIT code (no Sega SGL/SBL), the **built disc
can be committed** and run in CI — unlike the gitignored commercial `roms/` and
`bios/`.

## Result protocol (the contract with `crates/saturn/tests/homebrew_disc.rs`)

The program writes these to **High Work RAM** as its *last* action, in order
(signature last, so the harness never reads a half-written status):

| address       | meaning                                                  |
|---------------|----------------------------------------------------------|
| `0x0603_FF04` | status: `0` = all pass, non-zero = id of the failing test |
| `0x0603_FF08` | detail / last-checked code (informational)               |
| `0x0603_FF00` | signature `0x54535431` (`"TST1"`), written **last**       |

The harness boots, polls `0x0603_FF00` for `"TST1"`, then asserts
`0x0603_FF04 == 0`. No signature ⇒ *inconclusive* (not a failure), so the
harness can be smoke-tested against any bootable disc.

> ⚠️ **Reserve the result block.** `0x0603_FF00..0x0603_FF10` must sit outside
> the program image, stack, and heap. Confirm against the linker map
> (`build/saturn-tests.map`) — adjust the addresses here *and* in
> `homebrew_disc.rs` together if the layout forces a different reserved region.

## Build

Needs the libyaul toolchain (SH-2 GCC) — not bundled. One-time install:

- **Docker (easiest):** the [`yaul-org/build-scripts`](https://github.com/yaul-org/build-scripts)
  image; mount this dir and `make`.
- **Arch:** the `yaul-tool-chain-git` AUR package, then libyaul.
- **Debian/macOS:** build the toolchain from source per libyaul's README.

Then point `$YAUL_INSTALL_ROOT` at the install and:

```sh
make                      # → build/saturn-tests.{iso,cue,bin}
make clean
```

The default output `build/saturn-tests.cue` is what the harness loads (override
with `HOMEBREW_DISC=<path>`). After a build, run the check:

```sh
# from the repo root, with a BIOS present in bios/
cargo test -p saturn --features chd --test homebrew_disc -- --nocapture
```

> The `Makefile` targets the current libyaul layout (`build.pre.mk` /
> `build.post.mk`); if your installed yaul version differs, mirror the Makefile
> from a libyaul `example/` or from StrikerX3/saturn-tests. The C in `src/`
> doesn't depend on yaul beyond startup — the result-protocol writes are plain
> volatile stores — so the program body is portable across SDK versions.

## Adding a test

Each feature check lives in `src/` and sets `status` to its own non-zero id on
failure (and leaves `0` on pass). Drive the checklist from
[`doc/emulation-capabilities-evaluation.md`](../../doc/emulation-capabilities-evaluation.md).
You may also adapt tests from [StrikerX3/saturn-tests](https://github.com/StrikerX3/saturn-tests)
(MIT) with attribution. Keep every check **deterministic** — no dependence on
the RTC or uninitialised RAM — so the result (and any framebuffer golden) is
stable across runs and across the Mednafen oracle.
