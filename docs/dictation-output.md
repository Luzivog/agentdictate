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
  then applies vocabulary aliases to the result.
- **Literal** sends only the language hint and applies no aliases.
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

- Every spelling is sent to OpenAI as a recognition keyword (`keywords[]`), which
  makes the model more likely to write it that way. A word without Sounds like is
  only a hint.
- Sounds like entries are automatic corrections. After recognition, each one in
  the text becomes its spelling. Matching ignores case and needs whole words. At any
  position the longest match wins, and the pass runs once, so a correction never
  feeds another.
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
  and a rejected WebM/Opus upload is resent once as WAV. A request that reached
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

`agentdictate-evaluate` replays cases through the production vocabulary handling and,
on request, the production transcription transport. It never opens the microphone
or pastes into another app. Build it once:

```bash
cargo build --locked -p agentdictate-app --bin agentdictate-evaluate
```

Each line of the case file is a JSON object:

| Field | Meaning |
| --- | --- |
| `id` | Case name |
| `text` | The recognized text to process in `offline` mode |
| `expected` | Optional exact output; `offline` mode fails on a mismatch |
| `preserve` | Substrings the output must keep, compared without case |
| `audio` | Absolute path of an audio file, for `speech` mode |
| `reference_verified` | Set to `true` only after a person has checked `expected` against the audio |

`fixtures/dictation/cases.jsonl` holds the synthetic offline cases. They cover
negations, numbers, operators, paths, quotes, retractions, and lookalike words. The
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

The tool writes one JSON line per case, with the output, the checks, and the options
used, to a new file with mode 0600. It refuses to overwrite an existing file, so use
a new path per run. It prints how many cases passed and exits with an error if any
check or request failed, keeping the results.

`--mode speech` uploads each case's `audio` through the production file transport.
It calls OpenAI and costs money, so it needs a configuration with an OpenAI API key.
`--model <id>` overrides the transcription model.

Word error rate and exact-match fields only measure agreement with the reference you
supplied. To decide whether a change helps your own speech, record 60 to 100
consented utterances, verify their references, and hold a third of them back for the
final comparison. Include vocabulary lookalikes, negations, exact strings,
self-corrections, long requests, and any second language you use. Replay the same
audio with and without the change, and review the outputs blind. Keep recordings,
configurations, and results outside the repository.
