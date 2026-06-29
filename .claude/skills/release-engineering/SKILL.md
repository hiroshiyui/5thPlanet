---
name: release-engineering
description: Manage the full software release process for the 5thPlanet workspace — version bumps, changelogs, Git tags, and (when applicable) GitHub releases.
---

When performing release engineering, always follow these steps:

1. **Verify the build is clean from scratch** — run `cargo clean && cargo test --workspace --all-targets` to confirm a from-scratch build passes every test. This catches build-plumbing bugs that an incremental `cargo build` would hide due to caching, and mirrors what CI sees.

2. **Verify formatting and lints** — `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must both be clean. Don't release with a yellow bar.

3. **Determine the release type** — review all unreleased commits since the last tag (`git log --oneline $(git describe --tags --abbrev=0 2>/dev/null)..HEAD` if a previous tag exists, otherwise `git log --oneline`) and classify the release as `major`, `minor`, or `patch` following [Semantic Versioning](https://semver.org/). Until milestone M1 (cycle-accurate SH-2) ships, the project stays on `0.y.z` and any user-visible behavioral change is a minor bump. Present the recommendation to the user and confirm before proceeding.

4. **Update the version** — bump `workspace.package.version` in the root `Cargo.toml`. All member crates inherit it via `version.workspace = true`, so no per-crate edits are needed. Run `cargo update --workspace` to refresh `Cargo.lock`.

5. **Update `CHANGELOG.md`** — add a new version entry at the top following the [Keep a Changelog](https://keepachangelog.com/) format. Group changes under `Added`, `Changed`, `Fixed`, `Removed`, or `Security` as appropriate. For SH-2 work, also note which roadmap tasks (from `doc/roadmap.md`) flipped to ✅ done. Create `CHANGELOG.md` if it doesn't yet exist.

6. **Commit the release** — stage `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, and any roadmap updates together and commit with the message `chore: release vX.Y.Z`.

7. **Tag the release** — create an annotated Git tag (`git tag -a vX.Y.Z -m "vX.Y.Z"`) and push both the commit and the tag (`git push && git push --tags`). Skip this step if no remote is configured; report the local tag instead.

8. **Build the distributable binary with PGO** — *only when producing a downloadable `jupiter` binary for the release* (the project doesn't attach binaries yet — "precompiled binary packages" is a later-milestone goal — so skip this for a source-only/tag-only release). Build the shipping binary with `tools/pgo/build_release.sh`, **not** a plain `cargo build`: Profile-Guided Optimization is the project's biggest single-core lever (measured +31% VF2 fight / +39–56% Doukyuusei, generalises across games — see `tools/pgo/README.md`). It is **build-time only and bit-identical** (the script runs the `bios_boot` golden + `savestate` gates under `profile-use` and aborts if either moves), so it never alters emulation accuracy. It needs a training BIOS + `roms/*.cue` and the rustup `llvm-tools` component; if any are absent it **falls back to a plain release build** with a warning, so the release never breaks. PGO is deliberately a packaging step, never a checked-in `RUSTFLAGS` every `cargo build` would pay. Attach the resulting `target/release/jupiter` (per-platform) to the release in the next step.

9. **Create a GitHub release** — if a GitHub remote is configured, use `gh release create vX.Y.Z --title "vX.Y.Z" --notes "..."` with the corresponding `CHANGELOG.md` section as the release notes. Note: use `--notes` (not `--body`) for the release description. If step 8 produced PGO binaries, attach them (`gh release create … <binary>…` or `gh release upload vX.Y.Z <binary>`).
