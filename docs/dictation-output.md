# Dictation output and evaluation

AgentDictate now keeps the recognition result before attempting cleanup. Each new
recording stores its output mode, context, vocabulary, cleanup configuration, and
replacement rules without API credentials. A retry reuses saved raw text and the
original options. Historical jobs without an options snapshot retain the legacy
fallback to current settings. Imported ChatGPT dictations still bypass this pipeline.

## Everyday controls

- **Dictate:** recognition, optional cleanup, then spelling normalization.
- **Literal:** skips recognition context/keywords, cleanup, vocabulary aliases, and
  legacy replacements. Speech recognition still cannot guarantee exact characters.
- **Organize:** explicitly requests paragraphs or bullets from the cleanup model,
  even when cleanup is off for ordinary Dictate. It must preserve the stated request.

Choose the default under Settings → Dictation output. For one recording, choose
**Start literal dictation** or **Start and organize request** in the tray. Stop with
the normal hotkey. These overrides do not change saved settings. Headless controls
are also available:

```sh
agentdictate start --mode literal
agentdictate stop
```

`start --mode organize`, plain `start`, and `cancel` are also supported. Mode starts
are ignored by the tray while busy and rejected by the daemon while recording.
Protocol version 4 prevents an older daemon from silently ignoring an override.

## Maintain vocabulary once

The Vocabulary editor accepts one canonical spelling per line. Canonical spellings
become recognition keywords for supported models and possible spellings in cleanup.
Only explicitly supplied aliases authorize automatic corrections:

```text
Codex
Claude Code
AgentDictate = agent dictate
worktree = work tree
worktrees = work trees
```

After correcting a recurring error, add the canonical spelling here first. Add an
alias only when the spoken form should consistently mean that spelling. For example,
keep `Codex` as a hint without turning every ordinary `codecs` into Codex. Reuse the
history transcript to reproduce a failure; nothing monitors typing in another app.

Aliases match original text once, prefer the longest match at a position, and do
not cascade. Code spans, quotes, URLs, paths, and flags are protected conservatively.
Use Literal for exact strings whose syntax cannot be inferred. Existing entries in
the Replacements screen remain explicit legacy expansions with their original
sequential semantics; they do not gain these protections. Avoid defining the same
correction in both collections.

Context prompt describes the speaking situation. Current work context is optional
text you supply for the active task; clear it when moving projects. No repository,
window contents, selected text, or conversation is scraped automatically. The
effective cleanup preview shows the assembled instructions and vocabulary.

`gpt-transcribe` receives `keywords[]` and comma-separated language hints as
`languages[]`. Older file models accept a single language hint and reject a language
list locally. Subscription transcription remains separate and never falls back to
paid API speech. Cleanup is an OpenAI API operation regardless of speech provider.

## Failure behavior

Cleanup has a configurable deadline, defaulting to 3,000 ms. Incomplete, malformed,
refused, empty, or timed-out responses fall back to saved raw text. A conservative
guard also rejects changes to literal spans, signed numbers/operators, acronyms,
negations, conditional words, and large content reductions/expansions. It can reject
a legitimate edit, including removal of repeated uncertainty or numeric
self-corrections. Passing the guard is not proof of semantic equivalence. Fallback
reasons are retained in history; spelling normalization still runs on fallback.

Stream speech is an optional OpenAI API path using `gpt-live-transcribe`, currently
$0.017 per audio minute. It tails durable audio while speaking, resamples PCM16 from
16 to 24 kHz, and uses the stop action as its commit boundary. No interim draft is
pasted. Only the matching final item is accepted; a failed, invalid, or timed-out stream uses the selected file model and saved WAV. Failed live attempts
can incur provider charges in addition to the fallback. User cancellation discards the recording instead of uploading a fallback. Usage
history records the
successful speech model; it does not account for every failed streaming attempt.

## Local rollout decision, 5 September 2026

The selected personal defaults are buffered `gpt-transcribe`, English, a compact
vocabulary, and Dictate without a mandatory cleanup call. Optional cleanup and
Organize use `gpt-5.4-nano` with `none` reasoning and a 3-second deadline. Streaming
remains off by default. These personal choices do not overwrite other installations'
saved model settings. The local migration backs up and disables the 18 audited
database rules; ten narrow aliases move into the protected vocabulary collection. Ambiguous
forms such as landlord, codecs, Cloud Code, verso, and Versal remain ordinary text.

A bounded comparison used the same initial 12 synthetic text cases:

| Candidate | Median request time | Observations |
| --- | ---: | --- |
| Existing custom prompt / Astra low | 1,458 ms | One deadline fallback and one guard fallback |
| Faithful prompt / Astra low | 1,546.5 ms | Two conservative guard fallbacks |
| Faithful prompt / Nano none | 759 ms | All 12 outputs unchanged, no transport/guard failures |
| Cleanup off | No cleanup request | Keeps recognition output plus narrow spelling normalization |

The expanded 18-case set passed all offline checks. A second Nano run passed
17/18 request checks: one transport failure retained the original double-negation
text. No delivered critical-token failures were observed. A second live replay
finalized in 728 ms and repeated the punctuation issue. An explicit Organize
example preserved its checked constraints. These results retain failures rather
than treating raw fallback as a successful provider response.

The existing-prompt comparison uses the new deadline/guard and mode suffix, so it
is not a byte-for-byte replay of the previous implementation. Calls overlapped;
these are small descriptive samples, not controlled provider latency rankings.

On one approximately five-second Flite-generated clip, live recognition finalized
964 ms after simulated stop. The buffered path took 657 ms including encoding and
request. Both omitted a word; live output also merged a question and prohibition
with worse punctuation. This supports retaining the buffered default, not a general
claim about live recognition quality. The replay never opened a microphone or
delivered text to another application.

Private receipts and pre-rollout backups live under
`~/.local/state/agentdictate/output-improvement/`. Recordings, credentials, and local
history are not checked into this repository.

## Repeat the evaluation

Build the headless replay tool:

```sh
cargo build --locked -p agentdictate-app --bin agentdictate-evaluate
target/debug/agentdictate-evaluate \
  --cases fixtures/dictation/cases.jsonl \
  --output /tmp/dictation-results.jsonl \
  --mode offline
```

Use a new output path per run. Results use owner-only file permissions. `--config`
selects a separate candidate configuration without modifying the live settings.
`--mode cleanup --model gpt-5.4-nano --effort none` calls the paid cleanup API.
`--mode speech` uploads each supplied audio file. `--mode live` paces supplied audio
through the production live adapter, measures stop-to-final time, and records the
actual model so buffered fallback cannot masquerade as successful streaming.
Network evaluation requires an OpenAI API configuration; it does not use a signed-in
subscription account. Keep all private fixtures and results outside the repository.

Each JSONL case has `id`, `text`, optional `expected`, and `preserve` substrings.
Audio cases add an absolute `audio` path and `reference_verified`. Only mark that
flag true after a human has checked the reference against the recording. WER and
exact-match fields describe agreement with the supplied reference; critical-token
checks cannot certify preserved authority or intent. The tool exits unsuccessfully
on transport or explicit-check failures but retains the receipt.

For a personal accuracy decision, collect the planned 60–100 consented utterances,
reserve a held-out third, and include normal vocabulary lookalikes, negation, exact
strings, corrections, long requests, and bilingual/noisy speech if relevant. Replay
identical audio with and without hints; review blind, and measure corrections and
receiving-agent misunderstanding. No personal accuracy gain or latency percentile
is claimed from the synthetic checks. Automatic context collection, correction
capture, outside providers, local inference, and audio-native intent extraction stay
conditional experiments until a measured residual error justifies them.
