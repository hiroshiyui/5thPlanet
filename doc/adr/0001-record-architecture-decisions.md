# 0001. Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-05-25

## Context

5thPlanet emulates eight tightly-coupled Saturn processors and is built
up one chip at a time across milestones. Many of its design choices are
load-bearing in ways that aren't obvious from the code alone — the `Bus`
returning a stall count, the queue-and-drain pattern, the determinism
contract in the scheduler. `CLAUDE.md` already documents *what* the
architecture is, but the *why* behind individual decisions lives only in
commit messages and contributors' heads, where it's easy to lose or to
unknowingly contradict.

As the emulator grows (VDP1, SCSP, CD-block, save states still to come),
we want a durable, greppable record of significant decisions so a future
contributor doesn't relitigate a settled trade-off or quietly undo one.

## Decision

We will keep Architecture Decision Records in `doc/adr/`, one Markdown
file per decision, using the lightweight Nygard format
([`template.md`](template.md)). ADRs are numbered, append-only, and
superseded rather than rewritten. A new ADR lands in its own
`docs(adr): …` commit. The process and conventions live in
[`README.md`](README.md).

We record a decision as an ADR when it is architecturally significant —
it constrains future code, is expensive to reverse, or is the kind of
thing a reviewer should be able to cite ("that violates ADR-NNNN").
Routine implementation choices do not get an ADR.

## Consequences

- Significant decisions gain a stable home and a clear rationale; reviews
  can reference them.
- A small recurring cost: writing an ADR when a real decision is made,
  and keeping the index current.
- Some already-made decisions are worth backfilling (see the README
  backlog); we'll do so opportunistically, not all at once.

## Alternatives considered

- **Keep rationale in `CLAUDE.md` / commit messages only.** That's the
  status quo; it works for *what* but scatters *why* and offers no
  superseded-by trail when a decision changes.
- **A heavier ADR format (e.g. MADR).** More structure than this
  project needs right now; the Nygard format keeps the barrier to
  writing one low.
