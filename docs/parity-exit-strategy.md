# Parity Exit Strategy

The legacy Python implementation under `src/agentdictate/`, its unittest suite
under `tests/`, and the Python leg of `run-tests.sh` exist solely to pin
behavioral parity during the migration to Rust. Python is not shipped:
installed and packaged binaries are built from the Rust workspace only.

## Status

- The Rust binaries `agentdictate` and `agentdictated` are the shipped product.
- The Python suite is a test fixture for migration, not a maintained
  implementation. It must never gain features.
- The final gate (`./run-tests.sh`) runs the full locked Rust workspace first,
  then the Python parity suite.

## Accepted Divergences

These divergences from legacy Python behavior are deliberate and are the
intended forward behavior of the Rust implementation. Do not change code to
restore parity in these areas.

### Replacement application

Legacy Python used regex lookarounds with `re.subn`, which would have expanded
`$1`-style backreferences inside replacement phrases. The Rust engine splices
replacement text literally and checks word neighbors manually to enforce
whole-word matching. A `$1` in a replacement phrase is now inserted literally.

### Applied-rule reporting key

When reporting which replacement rules were applied, legacy Python emitted a
key named `id`; Rust serializes the same field as `rule_id`.

### Cost math

Both implementations share cost arithmetic, including round-ties-even
(banker's) rounding. This shared behavior is pinned by golden-value tests in
`crates/agentdictate-core/tests/core/` (for example
`session_costs_match_the_legacy_python_golden_values`) and does not depend on
the Python suite surviving.

One deliberate divergence inside token estimation: legacy Python measures
text length in characters (`len(text)`), while Rust measures bytes
(`str::len`). The two agree for ASCII transcripts; non-ASCII transcripts can
produce slightly different token estimates. Estimates only ever feed the
usage/cost display, never billing, so byte counting is the intended forward
behavior. The golden fixture uses ASCII inputs to pin the shared arithmetic
itself.

## Sunset Criteria

The Python source and unittest suite may be deleted once all three hold:

1. No installed or packaged artifact references `src/agentdictate`.
2. The Rust gate covers schema and wire compatibility previously pinned only
   by parity tests (settings JSON tolerance, SQLite schema shape, protocol v2
   wire format).
3. Two consecutive release cycles ship without a bug report that was caught by
   the parity suite alone.

Deletion is one commit that removes `src/`, `tests/`, `pyproject.toml`, and
the Python leg of `run-tests.sh`.

## Interim Rule

While the suite exists, behavior fixes land in Rust first. The Python tree is
updated only when a parity pin itself needs correcting, never to add features.
