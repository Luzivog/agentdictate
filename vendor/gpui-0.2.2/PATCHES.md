# Local patches

This directory is the crates.io `gpui` 0.2.2 package published from
`69e2130295c2649963eb639fc70b4f2ee8ea1624`.

AgentDictate carries the X11 popup `override_redirect` change from upstream
commit `42d5f7e73e8597b26f4457399d4c5afa8fed24b0`. Remove this vendored package and
the root `[patch.crates-io]` entry when a published GPUI release includes that
commit.
