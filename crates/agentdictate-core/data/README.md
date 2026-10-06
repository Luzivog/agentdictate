# Bundled data

## common-english-words.txt

Common English words, one per line, lowercase ASCII letters only, sorted
byte-wise and deduplicated. Dictate mode uses it to decide whether a one-word
spelling from Words may also be an ordinary word: `Rust` and `Go` are in the list,
so "rust" and "go" keep their case, while `Codex` and `Leadlord` are not, so
"codex" and "leadlord" become the spelling.

- **Source:** SCOWL (Spell Checker Oriented Word Lists) by Kevin Atkinson,
  release 2020.12.07, `scowl-2020.12.07.tar.gz` from
  <http://wordlist.aspell.net/> (SHA-256
  `5587667caa20c4891390c2d42dbb4d5c4c3f41bee77af1457ece3ba23fb859cc`).
- **Selection:** the `final/` word files `english-words`, `american-words`,
  `british-words` and `canadian-words` at sizes 10, 20 and 35. The proper-name,
  abbreviation, contraction and upper-case lists are left out. Lines that are not
  entirely `a` to `z` (possessives, accented and capitalized words) are dropped.
- **Size cutoff:** 35, SCOWL's recommended "small" size. Size 50 adds words such
  as "codex", "tailwind" and "resend", which are more often names in dictations.
- **License:** SCOWL's permissive notice, reproduced in full in
  [SCOWL-LICENSE.txt](SCOWL-LICENSE.txt). Keep that file with any copy of the list;
  the packages install it as `/usr/share/doc/agentdictate/SCOWL-LICENSE.txt`.

To regenerate, extract the release and run, for each region and size, the filter
above (`grep -xE '[a-z]+'` on each file read as Latin-1), then `LC_ALL=C sort -u`.
