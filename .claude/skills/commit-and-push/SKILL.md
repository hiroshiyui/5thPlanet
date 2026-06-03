---
name: commit-and-push
description: Stage, commit, and (when a remote exists) push changes with a well-formed Conventional Commits message.
---

When committing and pushing changes, always follow these steps:

1. **Verify tests pass** — run `cargo test --workspace` before committing. A red bar must not be committed. If a test is intentionally being marked `#[ignore]` or `#[should_panic]`, call it out in the commit body.

2. **Stage** all relevant changes with `git add <paths>`. Be deliberate — stage only files related to the current topic. Never blindly use `git add -A` if unrelated changes are present.

3. **Compose the message** following the [Conventional Commits](https://www.conventionalcommits.org/) standard. Use these scopes for this project:
   - `sh2` — anything inside `crates/sh2/`
   - `saturn` — anything inside `crates/saturn/`
   - `frontend` — `jupiter/` binary
   - `workspace` — root `Cargo.toml`, `.gitignore`, shared lints
   - `doc` — `doc/**`, `README.md`, `CLAUDE.md`
   - `ci` — `.github/`, hooks, scripts
   - Use `feat`, `fix`, `refactor`, `test`, `docs`, `chore` as the type.

   The message should explain *why* the change was made, not just *what* changed. When the change advances a task in `doc/roadmap.md`, reference the task number (e.g. "advances M1 task #4").

4. **Commit** with the composed message.

5. **Push** the committed changes to the current branch on the remote — **but only if a remote is configured**. Check with `git remote -v` first. If no remote exists, stop here and report the local commit hash; do not invent or add a remote without the user's explicit instruction.

6. **Verify** that the push succeeded and the remote is in sync with the local branch (`git status` should report "up to date").
