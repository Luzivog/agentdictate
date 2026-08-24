# Parity Exit Strategy

## Executed

The legacy Python implementation under `src/agentdictate/`, its root unittest
suite under `tests/`, and the Python leg of `run-tests.sh` were removed on
2026-08-24.

Sunset criteria 1 and 2 were met:

1. No installed or packaged artifact referenced `src/agentdictate/`.
2. The Rust gate covers settings JSON tolerance, SQLite schema shape, and the
   protocol version 3 wire format through
   `crates/agentdictate-core/tests/core/settings.rs`,
   `crates/agentdictate-core/tests/core/costs_golden.rs`,
   `crates/agentdictate-core/tests/core/protocol.rs`, and
   `crates/agentdictate-runtime/tests/runtime/migration_parity.rs`.

The owner waived criterion 3, two clean release cycles, because the suite was
already violating its own freeze policy.

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
