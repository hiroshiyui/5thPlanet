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

8. **Create a GitHub release** — if a GitHub remote is configured, use `gh release create vX.Y.Z --title "vX.Y.Z" --notes "..."` with the corresponding `CHANGELOG.md` section as the release notes. Note: use `--notes` (not `--body`) for the release description.
