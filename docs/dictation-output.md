# Dictation output and evaluation

This page explains what AgentDictate does to recognized speech before it pastes it,
how to steer recognition with vocabulary and context, what happens when a dictation
fails, and how to measure a change with `agentdictate-evaluate`. For the full
pipeline, see [the architecture overview](architecture.md#dictation-pipeline).

Each recording stores a snapshot of its options when it starts: output mode,
language, context, vocabulary, and streaming. API credentials are never stored with
it. **Transcribe again** reuses that snapshot and any text already recognized, so a
retry neither changes behavior nor pays twice.
Older jobs without a snapshot use the current settings.

## Output modes

- **Dictate**, the default, sends the context and vocabulary hints with the audio,
  then applies vocabulary aliases to the result.
- **Literal** sends only the language hint and applies no aliases.
  Use it for exact strings whose spelling you cannot predict. Speech recognition
  still cannot guarantee exact characters.

Choose the default in **Settings**, **Dictation output**, **Output mode**. For one
recording, choose **Start literal dictation** in the tray, or start it from a
terminal, and stop it with the normal shortcut:

```bash
agentdictate start --mode literal
agentdictate stop
```

A one-off mode never changes the saved setting. The tray ignores mode starts while a
dictation is busy, and the daemon rejects a start while it is already recording.

## Vocabulary

The **Vocabulary** editor takes one spelling per line. A spelling can be followed by
`=` and a comma-separated list of spoken forms, its aliases:

```text
Kubernetes
PostgreSQL = postgres q l, post gress
kubectl = cube control, cube cuttle
GitHub Actions
```

- Every spelling is sent to OpenAI as a recognition keyword (`keywords[]`), which
  makes the model more likely to write it that way. A spelling without aliases is
  only a hint.
- Aliases are automatic corrections. After recognition, each alias is replaced by
  its spelling. Matching ignores case and needs whole words. At any position the
  longest alias wins, and the pass runs once, so a correction never feeds another
  alias.
- Aliases never change protected spans: text in backticks or code fences, text in
  double or single quotes, URLs, paths starting with `/`, `./`, or `~/`, flags
  starting with `--`, and words that contain a slash.
- The editor accepts up to 100 entries. Spellings and aliases must be unique
  regardless of case, at most 128 bytes, and free of control characters and angle
  brackets.

When a word keeps coming out wrong, add its spelling first. Add an alias only when
that spoken form should always mean the spelling. An alias such as `Rust = rest`
would also rewrite every real "rest". To reproduce a failure, copy the transcript
from History; AgentDictate never watches what you type in other apps.

The retired **Replacements** screen is gone. On the first daemon start after the
upgrade, each enabled whole-word rule became an alias of its replacement's spelling,
and the daemon log lists every rule it moved or could not express as vocabulary.

## Context and language

- **Context prompt**, under **Dictation**, describes what you usually talk about.
  It is sent as the transcription `prompt`. Keep spellings in Vocabulary.
- **Current work context**, under **Dictation output**, is optional text about the
  task at hand. It is appended to the prompt and marked as data, not instructions.
  Clear it when you switch projects.

Nothing is collected automatically: no repository, window contents, selected text, or
conversation.

**Language** is automatic detection, one language, or **English and French**. Each
language is sent as a `languages[]` hint.

## Streaming

**Stream speech** is an experimental, OpenAI API-only option, off by default. While
you speak, it tails the saved WAV, resamples it from 16 to 24 kHz, and streams it to
`gpt-live-transcribe`. Stopping the recording commits the audio, and only the final
transcript is accepted; nothing is pasted before that.

If the stream fails, returns something invalid, or has no final text within 8 seconds
of the stop, AgentDictate uploads the saved WAV for normal file transcription
instead. A failed stream can still be billed, on top of the fallback. Esc discards
the recording without a fallback upload. Usage records the model that produced the
text and does not count failed streaming attempts. The estimated prices are $0.017
per audio minute for `gpt-live-transcribe` and $0.0045 for `gpt-transcribe`.

## Empty captures and failures

- **Silence.** When recognition returns nothing and the WAV is near-silent, the
  dictation ends quietly: no paste, no History entry, no Recovery item. Near-silent
  means a PCM16 peak of at most 128 and an RMS of at most 32, roughly -48 dBFS and
  -60 dBFS. This is not a duration cutoff, so a short recognized word still pastes.
  The audio follows the **Preserve temporary audio** setting.
- **Empty result from audible audio.** The dictation goes to Recovery with its audio,
  because something was said.
- **Network or API errors.** A request that fails before OpenAI answers is resent
  once, and a rejected WebM/Opus upload is resent once as WAV. Any other error sends
  the dictation to Recovery with its audio.
- **Paste problems.** If the focused window keeps changing, or the text cannot be
  published, nothing is pasted and the dictation stays in Recovery. Once the paste
  shortcut has been sent, AgentDictate never sends it again on its own.

Recovery lives in the **History** page. **Transcribe again** and **Paste again** copy
the text to the clipboard instead of pasting, because AgentDictate's own window has
the focus. Press Ctrl+V where you want it.

## Evaluate a change

`agentdictate-evaluate` replays cases through the production vocabulary handling and,
on request, the production transcription transports. It never opens the microphone
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
| `audio` | Absolute path of an audio file, for `speech` and `live` modes |
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

The other modes call OpenAI and cost money. They need a configuration with an
OpenAI API key.

- `--mode speech` uploads each case's `audio` through the production file transport.
- `--mode live` decodes each `audio` file with ffmpeg, paces it in real time through
  the streaming adapter, and records the stop-to-final time and the model that
  actually answered, so a fallback cannot pass as a successful stream.
- `--model <id>` overrides the transcription model.

Word error rate and exact-match fields only measure agreement with the reference you
supplied. To decide whether a change helps your own speech, record 60 to 100
consented utterances, verify their references, and hold a third of them back for the
final comparison. Include vocabulary lookalikes, negations, exact strings,
self-corrections, long requests, and any second language you use. Replay the same
audio with and without the change, and review the outputs blind. Keep recordings,
configurations, and results outside the repository.
