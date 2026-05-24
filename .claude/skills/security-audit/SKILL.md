---
name: security-audit
description: Perform a project-wide security and safety audit of the 5thPlanet workspace.
---

When performing a security audit, always follow these steps:

1. **Audit dependencies** — run `cargo audit` (install with `cargo install cargo-audit` if missing) against `Cargo.lock` to check for known RustSec advisories. Then run `cargo tree --workspace --duplicates` to flag duplicate transitive dependencies that could mask CVE fixes. Treat any *unmaintained* or *yanked* crate as a Medium finding even without a known vuln.

2. **Verify the unsafe-code lint is intact** — the workspace `Cargo.toml` must keep `[workspace.lints.rust] unsafe_code = "forbid"`. Grep for any per-crate `#![allow(unsafe_code)]` overrides; any that exist must carry a justification comment and a soundness argument. New `unsafe` blocks land as Critical findings until justified.

3. **Static review of trust boundaries** — the only trust boundary in M1 is the `Bus` trait. Audit each `Bus` impl (notably `sh2::harness::MemBus` and any future Saturn bus) for:
   - Address arithmetic that could panic on hostile or out-of-range inputs (use `wrapping_*` and explicit bounds, never raw `Vec` indexing in production paths).
   - Integer underflow on pre-decrement / post-increment addressing modes.
   - Mutable state shared across CPU instances without documented synchronization (relevant once the Saturn bus and the dual SH-2 land).

4. **Static review of host-facing future code** — for any code that will eventually touch the host (file I/O for CD images, save states, BIOS loading, SDL2 frontend), confirm:
   - File paths are validated and canonicalized before opening.
   - Image loaders bound their allocations (no `Vec::with_capacity(untrusted_u32 as usize)`).
   - Save-state deserialization uses a versioned format and rejects unknown versions.

   (Most of these don't exist yet — note the gap as a roadmap reminder rather than a finding.)

5. **Build-time / supply-chain check** — confirm no `build.rs` in any workspace crate executes network requests or shells out to untrusted binaries. Confirm `Cargo.lock` is committed and matches the manifest.

6. **Report findings** — document all identified risks grouped by category (Dependencies, Unsafe Code, Trust Boundaries, Host Boundary, Build/Supply Chain). Classify each by severity (**Critical / High / Medium / Low**) and provide concrete remediation steps. For each finding, cite the file path, line number, and the relevant audit advisory ID (when applicable).
