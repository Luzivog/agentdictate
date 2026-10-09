# Dictation output and evaluation

This page explains what AgentDictate does to recognized speech before it pastes it,
how to steer recognition with vocabulary and context, what happens when a dictation
fails, and how to measure a change with `agentdictate-evaluate`. For the full
pipeline, see [the architecture overview](architecture.md#dictation-pipeline).

Each recording stores a snapshot of its options when it starts: output mode,
language, context, and vocabulary. API credentials are never stored with
it. **Transcribe again** reuses that snapshot and any text already recognized, so a
retry neither changes behavior nor pays twice.
Older jobs without a snapshot use the current settings.

## Output modes

- **Dictate**, the default, sends the context and vocabulary hints with the audio,
  then writes [spoken symbols](#spoken-symbols) as symbols, applies vocabulary
  corrections to the result, and writes some spoken [numbers](#numbers) as digits.
- **Literal** sends only the language hint and applies no corrections.
  Use it for exact strings whose spelling you cannot predict. Speech recognition
  still cannot guarantee exact characters.

To make Literal the default, turn on **Exact mode** under **Settings**, **Show
advanced settings**. For one recording, choose **Start literal dictation** in the
tray, or start it from a terminal, and stop it with the normal shortcut:

```bash
agentdictate start --mode literal
agentdictate stop
```

A one-off mode never changes the saved setting. The tray ignores mode starts while a
dictation is busy, and the daemon rejects a start while it is already recording.

## Words

The **Words** screen lists the spellings AgentDictate should always use. Each word
has a **Spelling** and an optional **Sounds like**: comma-separated spoken forms,
its aliases.

| Spelling | Sounds like |
| --- | --- |
| Kubernetes | |
| PostgreSQL | postgres q l, post gress |
| kubectl | cube control, cube cuttle |
| GitHub Actions | |

Add a word in the top row, and use **Edit**, **Delete** and the filter box on the
list. Every change is saved at once, and **Saved ✓** confirms it.

- Every spelling with a letter or digit is sent to OpenAI as a recognition keyword
  (`keywords[]`), which makes the model more likely to write it that way. A symbol
  spelling such as `/` is not sent, because keywords name words in the audio.
- Sounds like entries are automatic corrections. After recognition, each one in
  the text becomes its spelling. Matching ignores case and needs whole words. At any
  position the longest match wins, and the pass runs once, so a correction never
  feeds another.
- A symbol spelling, made only of symbols such as `/` or `-`, joins the words around
  it: with `/ = slash`, "ChatGPT slash Codex" becomes `ChatGPT/Codex` and "and slash
  or" becomes `and/or`. `/ = slash` is built in: it applies without the entry,
  unless another entry uses the word "slash". `/` instead starts a path at the
  start of a line or after a word such as "the", "in", "do", "seeing" or "delete":
  "the slash home" becomes `the /home`. A comma right after the previous word is
  dropped and the same rules apply, except that `/` always joins: "clean up code,
  slash refactor" becomes `clean up code/refactor`, and "remove, delete, slash
  update" becomes `remove, delete/update`. The spoken word stays when the symbol
  cannot join: at the end of a sentence, after other punctuation ("PRs? slash do
  we"), when another word names the symbol ("slash command", "a trailing slash"),
  after "dot" or a helper word such as "can", "must" or "should" ("we can slash
  prices"), and before a small
  word such as "how", "are", "since", "at", "our" or "every" ("Slash how do we…",
  "the slash at the end", "slash our burn rate"), except "and" and "or".
- The spoken symbols below are built in. A symbol entry for one of their words,
  such as `- = dash` or `@ = at`, never overrides them; it only applies to what
  they leave as words, so `- = dash` still makes "read dash only" `read-only`.
- In the same pass, a spelling written with the wrong case gets the spelling's
  case: `agents.md` becomes `AGENTS.md`, `T3 code` becomes `T3 Code`, and "the
  codex config" becomes "the Codex config". It stays when it is already right
  inside a longer spelling (`CLAUDE.md` with `Claude` also listed), when `.` or `-`
  joins it to another word (`openai.rs`, `agentdictate-core`), when the spelling is
  one word and a `.` starts it or an `@` touches it (`.codex`, `@codex`,
  `hello@leadlord.ai`), and when the only change would lowercase capital initials
  (`Read-only` or `Read-Only` for `read-only`).
- A one-word spelling that is also a common English word, such as `Rust`, `Go`,
  `Effect`, `Swift`, `Convex` or `IT`, keeps the lowercase or capitalized form, which
  may be the ordinary word ("rust", "a side effect"). The list of common words is
  [SCOWL](http://wordlist.aspell.net/) at size 35, bundled with the app. To fix
  such a word everywhere, add its lowercase form as a Sounds like entry, such as
  `rust` for `Rust`; that entry also leaves `.` and `@` names alone. History records
  case fixes with the other corrections.
- Corrections never change protected spans: text in backticks or code fences, text
  in double or single quotes, URLs, paths starting with `/`, `./`, or `~/`, flags
  starting with `--`, and words that contain a slash.
- The screen keeps up to 100 words. Spellings and Sounds like entries must be
  unique regardless of case, at most 128 bytes, and free of control characters,
  angle brackets and `=`. A Sounds like entry cannot contain a comma or repeat its
  own spelling exactly; a different casing, such as `github` for `GitHub`, fixes
  the case.

When a word keeps coming out wrong, add its spelling first. Add a Sounds like entry
only when that spoken form should always mean the spelling: `rest` for `Rust` would
also rewrite every real "rest". When a transcript in History got a word wrong,
expand it and choose **Fix a word**. Type what AgentDictate heard and how it should
be spelled, and **Add to Words** adds the heard phrase to that word's Sounds like,
or adds the word. AgentDictate never watches what you type in other apps.

The retired **Replacements** screen is gone. On the first daemon start after the
upgrade, each enabled whole-word rule became a Sounds like entry of its replacement's
spelling, and the daemon log lists every rule it moved or could not express as a
word.

## Spoken symbols

Dictate mode writes these spoken forms as symbols, before vocabulary corrections, so
the result still gets its casing from Words ("agents dot md" becomes `agents.md`,
then `AGENTS.md`). Words are separated by spaces on one line.

| Say | Get | When |
| --- | --- | --- |
| dot slash install dot sh, dot dot slash dot dot slash src | `./install.sh`, `../../src` | Always. Further "slash" parts and a file extension join the path. |
| dash dash parallel, dash dash force dash with dash lease | `--parallel`, `--force-with-lease` | Always after "dash dash", for a name of two or more letters. A capitalized name is lowercased, and a small word never ends it: "dash dash watch dash and then" is `--watch dash and then`. A "dash" hyphenated to a word of the name counts as the spoken word: "dash dash no-dash isolate" is `--no-isolate`. |
| ls dash l | `ls -l` | One letter other than "a" or "i", after a lowercase word that is a command such as ls, rm, git, cargo or docker, or is not a common English word. "em dash a model" stays. |
| package dot json, Next dot JS, leadlord dot ai, name dot sites dot leadlord.ai | `package.json`, `Next.js`, `leadlord.ai`, `name.sites.leadlord.ai` | The last part ends in a web domain (ai, ca, co, com, dev, fr, gg, io, org, uk, xyz), which is lowercased whole, or a file extension (md, json, ts, rs, py, toml, yaml, html, css, png, pdf and other common ones), which is lowercased. No part is a small word, and the first is not a word such as "the", "a" or "use". A domain named only by common English words stays words unless Words spells one ("the early dot com days"; add `Stripe` for "stripe dot com"), as does any domain before bubble, boom, crash, era, days, company or startup. A common word capitalized by a sentence start is lowercased ("Package dot json" is `package.json`), except before `.js` (`Node.js`). |
| the dot env file, the dot env dot local file | the `.env` file, the `.env.local` file | At a line start or after a word such as "the", "to", "a" or "and", before env, git, gitignore, github, gitattributes, vscode, codex, claude, ssh, npmrc, nvmrc, editorconfig, prettierrc, eslintrc or dockerignore. Further "dot" parts join it. |
| hello at leadlord dot ai, send it to team at leadlord.ai | `hello@leadlord.ai`, `team@leadlord.ai` | The domain is a web domain as above, and either the name is hello, hi, info, contact, admin, noreply, postmaster or webmaster, or "email", "send", "write", "mail", "address", "account" or a similar word comes up to five words earlier in the sentence, or "email", "address", "account" or "inbox" follows. After "the", "our", "my", a pronoun or a similar word, only a name from that list followed by such a word counts ("the hello at leadlord.ai account"). Never for a contraction ("we're at") or after "plus". Lowercased. |
| snake underscore case | `snake_case` | Between two words that are not small words, contractions, or helper words such as "must", and not before a word that names the symbol ("the underscore key"). "we must underscore safety" stays. |
| T O K S, use the C L I. | `TOKS`, use the `CLI.` | Three or more capital letters separated by single spaces. A run starts with "A" or "I" only after "the", "this", "our" or a similar word ("so I A B tested" stays). A final "I" before a lowercase word stays a word ("B C D I think" is "BCD I think"), and a run before "or"/"and" and another letter is a list of options ("A B C or D"). |

Everything else stays words. That includes "dot dot dot", "the yellow dot", "make
the dot go away" (go, log, lock, net, app and other ordinary words are not
endings), "a dash review", "OCX dash star", "the at sign", "look at leadlord.ai",
"I work at leadlord.ai", "the team at leadlord.ai", "I'm at the office", "hello plus
one at leadlord.ai", "Plan B I think", and spelled letters joined by hyphens, such as
"Z-E-R-N-I-O". Spoken symbols never change
protected spans, the words of a spelling with several words, or those of a Sounds
like entry that does more than fix case, so a Words entry such as
`Plan-B = plan dash b` still decides its own text.
History records each rewrite with its other corrections, and Literal mode leaves
every spoken symbol as heard.

## Numbers

The transcription model already writes measurements as digits ("300 milliseconds",
"25%", "port 5173"), but it spells out small numbers after a label and when you
count. After vocabulary corrections, Dictate mode writes these two cases as digits:

- **A label and its number.** One of step, phase, question, option, decision,
  issue, item, lane, wave, tier, level, part, section, version, round, ticket, task,
  chapter, page, slide, lecture, module, week, stage, milestone, sprint, plan, case,
  test or PR, optionally followed by "number", then a number from zero to
  ninety-nine: "Wave one" becomes `Wave 1`, "question twenty-one" becomes
  `question 21`, and "issue number four" becomes `issue number 4`. "dot" or "point"
  and another number make a decimal: "version two dot five" becomes `version 2.5`.
  Before any other word, such as "step one point is", the label stays in words.
- **A run of three or more numbers** from zero to twenty, separated by spaces or
  commas: "One two three" becomes `1 2 3` and "two, three, four" becomes `2, 3, 4`.

Everything else stays in words: counts such as "two minutes", "one second", "these
two" or "one more", a pair such as "one two" or "Test one two", "one by one", a
number joined to another word ("step one-liner"), and digits already written.
Numbers inside protected spans or inside a spelling from Words ("Phase One") never
change, and Literal mode leaves every number as heard. History keeps the words as
heard with the dictation and records each conversion with its other corrections.

## Context and language

**About your work**, under **Settings**, **Show advanced settings**, describes what
you usually talk about. It is sent as the transcription `prompt`. Keep spellings in
Words. The retired **Current work context** setting was appended to it on upgrade.

Nothing is collected automatically: no repository, window contents, selected text, or
conversation.

**Language** is automatic detection, one language, or **English and French**. Each
language is sent as a `languages[]` hint.

## Empty captures and failures

- **Silence.** When recognition returns nothing and the WAV is near-silent, the
  dictation ends without a paste, History entry, or Recovery item, and the overlay
  and a notification say "Didn't hear anything". Near-silent
  means a PCM16 peak of at most 128 and an RMS of at most 32, roughly -48 dBFS and
  -60 dBFS. This is not a duration cutoff, so a short recognized word still pastes.
  The audio follows the **Keep audio recordings** setting.
- **Empty result from audible audio.** The dictation goes to Recovery with its audio,
  because something was said.
- **Network or API errors.** A request that fails to reach OpenAI is resent once,
  and a WebM/Opus upload OpenAI cannot decode is resent once as WAV. A request that reached
  OpenAI but got no answer within 180 seconds is not resent. Any other error sends
  the dictation to Recovery with its audio.
- **Paste problems.** If the focused window keeps changing, or the text cannot be
  published, nothing is pasted and the dictation stays in Recovery. Once the paste
  shortcut has been sent, AgentDictate never sends it again on its own.

Recovery lives in the **History** page. **Transcribe again** and **Paste again** copy
the text to the clipboard instead of pasting, because AgentDictate's own window has
the focus. Press Ctrl+V where you want it. An item nobody retries or deletes expires
7 days after it last changed, with its audio unless **Keep audio recordings** is on;
each item says when. A recording longer
than 5 seconds that you cancelled with Esc also waits there, as **Cancelled —
transcribe anyway?**, for 24 hours.

## Evaluate a change

`agentdictate-evaluate` replays cases through the production text normalization and,
on request, the production transcription transport. It never opens the microphone
or pastes into another app. Build it once:

```bash
cargo build --locked -p agentdictate-app --bin agentdictate-evaluate
```

Each line of the case file is a JSON object:

| Field | Meaning |
| --- | --- |
| `id` | Case name |
| `text` | The recognized text, before normalization, for `offline` mode |
| `expected` | Optional exact output after normalization; `offline` mode fails on a mismatch |
| `preserve` | Substrings the output must keep, compared without case |
| `audio` | Absolute path of an audio file, for `speech` mode |
| `reference_verified` | Set to `true` only after a person has checked `expected` against the audio |

`fixtures/dictation/cases.jsonl` holds the synthetic offline cases. They cover
negations, numbers, spoken symbols, operators, paths, quotes, retractions, and
lookalike words. The
`alias` case expects the aliases `AgentDictate = agent dictate` and
`worktrees = work trees`, so give the tool a configuration that defines them.
`--config` defaults to your real `config.json`, which it only reads. A candidate file
can hold just the keys you want to change:

```bash
cat > /tmp/evaluate-config.json <<'EOF'
{"vocabulary": [
  {"spelling": "AgentDictate", "aliases": ["agent dictate"]},
  {"spelling": "worktrees", "aliases": ["work trees"]}
]}
EOF
target/debug/agentdictate-evaluate \
  --cases fixtures/dictation/cases.jsonl \
  --config /tmp/evaluate-config.json \
  --output /tmp/dictation-results.jsonl \
  --mode offline
```

The tool writes one JSON line per run, with the raw and delivered text, the checks,
and the options used, to a new file with mode 0600. It refuses to overwrite an
existing file, so use a new path per run. It prints a line per case, pass or fail
with its word error rate (WER) after normalization and, as `raw`, before it. A summary
follows: runs passed, exact matches, and the total WER, which is the sum of word
edits over the sum of reference words. The tool exits with an error if any check or
request failed, keeping the results. An unknown flag or `--mode` is an error.

`--mode speech` uploads each case's `audio` through the production transport. It
calls OpenAI and costs money, so it needs a configuration with an OpenAI API key.
Only `preserve` and the request decide whether a speech run passes; WER and exact
matches are reported. These flags change one thing at a time for an A/B comparison:

| Flag | Effect |
| --- | --- |
| `--model <id>` | Another transcription model than `gpt-transcribe` |
| `--upload-format webm\|wav\|flac` | Upload exactly this format, with no WAV fallback or retry. `webm` uses the production encoder settings, and `flac` needs ffmpeg |
| `--prompt <text>`, `--no-prompt` | Replace or drop the About your work prompt |
| `--no-keywords` | Send no `keywords[]`; aliases still apply afterwards |
| `--language <codes>` | Replace the language hint, such as `en` or `en,fr`; `""` detects |
| `--repeat <n>` | Run each case n times and report the WER spread and distinct transcripts |

To build speech cases from your own dictations, turn on **Keep audio recordings**,
dictate as usual, then export:

```bash
target/debug/agentdictate-evaluate export-cases --output /tmp/my-cases.jsonl
```

It reads the database without changing it and writes one case per kept recording
whose dictation still has its text in History. A case's `text` is what the model
heard, and its `expected` is the delivered text, with `reference_verified: false`.
Correct `expected` against the audio before trusting its WER. The file has mode 0600,
holds your transcripts, and is never overwritten.

Word error rate and exact-match fields only measure agreement with the reference you
supplied. To decide whether a change helps your own speech, record 60 to 100
consented utterances, verify their references, and hold a third of them back for the
final comparison. Include vocabulary lookalikes, negations, exact strings,
self-corrections, long requests, and any second language you use. Replay the same
audio with and without the change, and review the outputs blind. Keep recordings,
configurations, and results outside the repository.
