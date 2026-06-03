# 0009. Live physical-disc reads via a feature-gated libcdio FFI crate

- **Status:** Accepted
- **Date:** 2026-05-28

## Context

M10 adds the ability to play an *original* Saturn disc from a host optical
drive, not just a ripped image. Reading raw 2352-byte sectors, the TOC, and
CD-DA (digital audio extraction) from a drive is **OS-specific** (Linux SG_IO,
Windows SPTI, macOS IOKit) and not available in pure safe Rust.

Two project invariants are in tension with this:

- **ADR-0002 / minimal dependencies** — the workspace deliberately carries few
  dependencies.
- **ADR-0007 — `unsafe_code = "forbid"` workspace-wide.** Any FFI is `unsafe`,
  and hand-rolling three platforms' native ioctls would be a large amount of
  `unsafe`, per-OS code to maintain and get right per drive.

The cross-platform C library **libcdio** (Linux/macOS/Windows/BSD) already
abstracts TOC + raw-sector + CD-DA reads behind one stable API. Binding to it
is far less code than three native backends — but it is still FFI, hence
`unsafe`, and it pulls in a system library most builds (and CI) won't have.

## Decision

We will read live discs through **libcdio**, isolated in a **new, feature-gated
crate `crates/physdisc`** that is the single, documented exception to ADR-0007:

- `physdisc` **does not set `[lints] workspace = true`**, so it does not inherit
  `unsafe_code = "forbid"`; its libcdio FFI may use `unsafe`. Every other crate
  (the cores, `saturn`) keeps the forbid. The `unsafe` is confined to one
  module behind a safe `Cdio` handle (RAII `Drop`), and `physdisc` is the only
  place in the tree that contains `unsafe`.
- The libcdio linkage lives behind the crate's **`libcdio` cargo feature**,
  **off by default**. Without it, `physdisc` compiles to a stub whose
  `PhysicalDisc::open` returns an error and links nothing — so
  `cargo build --workspace` and CI need no libcdio. The frontend's
  `physical-disc` feature turns it on (`cdrom:<device>` disc specs).
- `PhysicalDisc` implements `saturn::disc::SectorSource` (ADR-introduced in M10
  Phase 1), so the CD-block is unaware whether it's reading an image or a drive.
- **Data sectors are read through the kernel's cooked block device**
  (`read_at`/`seek_read`), not libcdio. libcdio's sector reads issue SG_IO
  `READ CD`, which needs `CAP_SYS_RAWIO`; the cooked block read returns the
  2048-byte payload to an ordinary `cdrom`-group user. libcdio is used for what
  the block device can't give: the TOC (track types/LSNs) and CD-DA extraction.

## Consequences

- **Easier:** one cross-platform backend instead of three native ones; the
  whole emulator core stays `forbid(unsafe_code)`; default/CI builds are
  unaffected (no libcdio); a real owned disc plays with our HLE header-only
  authentication (the unreadable security ring is irrelevant).
- **Harder / costs accepted:** a system dependency (libcdio + dev headers) for
  the opt-in feature; the FFI signatures/enum values are hand-bound to the
  documented libcdio API and must track it; CD-DA extraction quality varies by
  drive; Mode-2 subheader filtering isn't reconstructed from a drive's cooked
  reads (image discs still expose it). Verified end to end on a **real drive**
  (Virtua Fighter 2, `/dev/sr0`): TOC (34 tracks), data via the cooked block
  read, and CD-DA via libcdio — all as a normal `cdrom`-group user, and the
  emulator boots from the disc. Also covered by a `#[ignore]`d CUE/BIN image
  test. CI stays libcdio-free (the libcdio paths are feature-gated + ignored).

## Alternatives considered

- **Per-OS native FFI** (SG_IO / SPTI / IOKit) — no external library, but ~3×
  the platform code and far more `unsafe` to maintain; worse cost/benefit than
  one libcdio binding.
- **Keep `forbid(unsafe_code)` everywhere and skip physical discs** — would
  drop a feature the project wants; ripping to an image is the only path. The
  feature-gated, isolated exception is a better trade than forgoing it.
- **Embed the FFI in `jupiter` behind a feature** — workable, but mixes the
  `unsafe` boundary into the frontend binary; a dedicated crate confines the
  ADR-0007 exception to one place and keeps the frontend safe.
