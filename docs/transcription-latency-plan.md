# Transcription latency: root cause and fix plan

Diagnosis of the 2026-09-01 report that a ~3-minute dictation took over a
minute to transcribe after stopping. Measured, not guessed: numbers below come
from the daemon logs, the history database, and a live timing harness that
replayed a retained recording against the OpenAI API. Status: Phases 1 and 2 were implemented on 2026-09-01
(Opus-compressed uploads with WAV fallback, per-stage timing logs, whisper-1
fallback pass removed). Phase 3 (realtime streaming) remains open.

## Symptom

Stop-to-paste wall time, from `recording finalized` → `dictation flow
completed` in `agentdictated` logs:

| Date | Jobs | Avg wait | Long recordings (≥2 min) avg wait |
| --- | ---: | ---: | ---: |
| 2026-08-24 … 08-31 | 6–131/day | 4.1–5.2 s | 5.6–9.2 s |
| 2026-09-01 | 11 | 25.1 s | **60.7 s** (45.5 s and 76.0 s) |

Same model (`gpt-transcribe`), same 16 kHz mono WAV capture, same code path.
A 178 s recording that took 5.9 s of overhead on 08-25 took 46.0 s on 09-01.

## Root cause: uncompressed WAV upload on a slow uplink

The daemon uploads the raw PCM WAV (32 KB/s of audio; a 3-minute dictation is
~5.7 MB) in one blocking multipart POST. Live harness on 2026-09-01, same
62.5 s / 2.0 MB retained recording, two runs per model:

| Upload | Model | Total time | Measured upload speed |
| --- | --- | ---: | ---: |
| WAV 2.0 MB | gpt-transcribe | 14.7 s / 8.4 s | 136–239 KB/s |
| WAV 2.0 MB | gpt-4o-transcribe | 12.2 s / 13.1 s | ~150 KB/s |
| WAV 2.0 MB | gpt-4o-mini-transcribe | 13.9 s / 12.0 s | ~155 KB/s |
| WAV 2.0 MB | whisper-1 | 9.4 s / 11.3 s | ~195 KB/s |
| **Opus/OGG 186 KB** | **gpt-transcribe** | **2.6 s** | — |

`time_starttransfer ≈ time_total` in every WAV run: the request spends nearly
all its time uploading, then the model answers in ~1–4 s. The Opus run (24 kbps,
30× smaller payload) produced a transcript of identical length and content
quality in 2.6 s end to end.

Arithmetic check against the worst job (187.8 s audio, 6.0 MB WAV, 76.2 s
wait): 6.0 MB at ~136 KB/s ≈ 44–70 s upload + ~4 s model + 3.7 s cleanup
(measured live, see below) + ~1 s delivery. It reconciles.

So: the model is not slow and the code did not regress — the **uplink to
api.openai.com degraded around 08-31/09-01** (~10+ Mbps effective before,
~1–2 Mbps now), and the architecture multiplies that by 30× by uploading
uncompressed PCM. The app is one network-condition change away from this
happening again anywhere (hotel Wi-Fi, tethering, Imperial dorms).

Not the cause, checked and excluded:

- **Cleanup (`gpt-5.6-luna`, effort low)**: replayed today's real 382-word
  transcript through `/v1/responses` with the production instruction: 3.7 s.
  The overlay also already shows it as a separate "Cleaning up…" stage
  (`process.rs` cleanup-started observer), so the "Transcribing" label the
  user stares at is genuinely transcription (upload) time.
- **whisper-1 fallback second pass** (`openai.rs::transcribe_audio`): today's
  long transcripts were far above the suspicious-shortness threshold, so it
  did not fire. (It *would* silently double latency when it does — see
  observability below; nothing logs it today.)
- **Accuracy-stack changes of 08-31** (glossary transcription prompt, cleanup
  enable): a 176 s job *before* the settings change already showed 25.9 s
  overhead; cleanup adds only ~4 s.

## External context (researched 2026-09-01)

- `gpt-transcribe` (July 2026) is OpenAI's recommended file-transcription
  model; median throughput ~40× real time (~4.5 s of model time for a 3-min
  clip). `whisper-1` and the `gpt-4o-transcribe` family are deprecated,
  shutting down 2027-02-26 — the whisper-1 fallback needs a replacement
  before then.
- `/v1/audio/transcriptions` accepts ogg/opus (name the part `*.ogg`), mp3,
  m4a, flac, etc. Whisper-lineage models downsample to 16 kHz mono
  internally; community benchmarks show no accuracy loss down to ~32 kbps.
- `stream=true` (SSE deltas) only streams the *response*; it starts after the
  upload completes. It improves perceived progress, not time-to-final-text —
  not the fix here.
- The instant-feel dictation apps (Wispr Flow ~700 ms, Aqua Voice ~450 ms)
  stream audio *while the user speaks*. OpenAI's equivalent: Realtime
  WebSocket transcription — `gpt-transcribe` committed-turn mode (append
  pcm16/24 kHz chunks during recording, commit at stop, final text arrives
  almost immediately) or `gpt-live-transcribe` ($0.017/min) for live deltas.

## Plan

### Phase 1 — Compress the upload (small change, removes ~90 % of the wait)

Encode the finalized WAV to Opus-in-OGG before upload in
`ReqwestOpenAiTransport::transcribe_once`:

- Encode at stop: 16 kHz mono, 24–32 kbps. A 3-min WAV encodes in well under
  a second; payload drops 5.7 MB → ~0.6 MB, upload drops from tens of seconds
  to ~1–3 s even on a 1–2 Mbps uplink.
- Mechanism: shell out to `ffmpeg` (`-ac 1 -ar 16000 -c:a libopus -b:a 32k`),
  consistent with the app's existing pw-record/xsel subprocess pattern. If
  `ffmpeg` is missing or encoding fails, fall back to uploading the WAV and
  log it — never lose a dictation to the optimization. Packaging: declare
  ffmpeg as a recommended dependency (deb `Recommends:`, AppImage docs);
  the fallback keeps the app functional without it.
- Keep the durable capture as WAV — recovery, retry, and history semantics
  unchanged; the OGG is a transient upload artifact next to the WAV (or in
  memory), deleted with it.
- The whisper-1 fallback pass reuses the compressed file, so its cost when it
  fires drops too.

### Phase 2 — Per-stage observability (do in the same PR as Phase 1)

Today the log is silent between `recording finalized` and `paste command
submitted`; nothing attributes time to upload vs model vs cleanup, and the
whisper-1 fallback is invisible. Add INFO-level timing logs in the pipeline:

- `transcription request completed`: model, audio seconds, payload bytes,
  encode ms, HTTP ms, and transcript chars — one line per attempt, so the
  fallback pass shows up as a second line with its own model.
- `fallback transcription triggered`: word counts and threshold that fired.
- `cleanup completed`: model, effort, input/output tokens, HTTP ms.
- Extend `dictation flow completed` with total stop-to-paste ms.

That makes the next "why is it slow" answerable from one `grep job_id` instead
of a forensic session, and cheaply confirms Phase 1's effect in production.

### Phase 3 (optional, later) — Stream audio during recording

If sub-second stop-to-text is wanted, move to the Realtime WebSocket
committed-turn mode: open the session at record start, append pcm16 chunks as
PipeWire delivers them, commit at the stop hotkey; the transcript is complete
moments later regardless of uplink speed, because the audio already left the
machine while speaking. This is a real architecture change (recorder must tee
audio to the network, new failure modes: mid-session drops, reconnect,
last-chunk flush) — decide after seeing how far Phase 1 gets. Phase 1 remains
useful regardless as the retry/recovery path.

### Housekeeping surfaced by the diagnosis

- Replace or drop the `whisper-1` fallback before its 2027-02-26 shutdown.
- The 180 s request timeout plus a machine that suspends on idle means a slow
  job can outlive the user's presence; consider a systemd idle-inhibit while
  a job is processing (optional — Phase 1 makes waits short enough that this
  mostly stops mattering).
