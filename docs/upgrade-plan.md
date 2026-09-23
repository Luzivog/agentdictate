# AgentDictate end-to-end upgrade plan

Date: 22 September 2026. Baseline: `main` at `aaef150`, the installed build, and the live
daemon's logs and database. Thomas accepted every recommendation in §4. The status below
tracks what has landed on `main`; the rest of the plan is kept as written.

This plan comes from seven parallel audit lanes, an independent correctness bug hunt, and two
skeptical verification passes. The lanes were latency with real usage data, speech-to-text
research, code simplification, Linux integration, UI/UX, build/tests/docs, and runtime/data. The
verifiers tried to refute the 30 load-bearing claims against the code: 27 were confirmed, 3 were
partly true (corrected below), and none were refuted. Numbers are measured from the logs and
database unless marked *inferred*. The full lane reports, with every file:line reference, are
archived at `~/.local/state/agentdictate-audit-20260922/`.

## Status

Updated 23 September 2026, at `7b2b4f5` plus the docs consolidation.

| Phase | State | Still open |
| --- | --- | --- |
| 0. Safety and correctness | Landed | D1 host rule (needs sudo) and the in-app world-access warning; COR-8/D19 copy-only for late results |
| 1. Quick latency wins | Landed | None |
| 2. Delete dead weight | Mostly landed, including the GPUI-free daemon (BLD-3) and the docs (BLD-2) | Decision-gated deletions D2 to D5; build ceremony (BLD-12/14) |
| 3. Daemon core | Not started | All |
| 4. Streaming | Not started | All |
| 5. Product and UX | History basics (item 3); GPUI migration (item 10); the unit is rewritten only when it changes (part of item 9); `agentdictate setup-access` as the backend for item 7's grant button | Items 1, 2, 4 to 8, and the rest of item 9 |
| 6. Data and privacy | Not started | All |
| 7. Linux platform | In-process clipboard (LNX-9), Shift+Insert everywhere (LNX-14), 100 ms ducking steps, `agentdictate setup-access` | Layout-independent hotkey (LNX-13), GlobalShortcuts portal |
| 8. Release and tooling | Installer (BLD-15), isolated dev instance and smaller `startup.rs` (BLD-16), tested and pinned release workflow, one CA store (BLD-13) | Cut v0.3.0; delete the linker workaround after D16 |

D1 and D16 need sudo on the host and wait for Thomas.

## 1. The short version

- **Dictation already works well.** Since 7 September, 874 recordings produced no paste,
  recorder or ducking failures. `gpt-transcribe` plus vocabulary keywords cut misses on tracked
  names from 23 % to 2 %. The model and provider are right. Don't switch them.
- **Speed is dominated by long dictations and fixed local overhead.** Median stop-to-paste is
  1.56 s, but dictations of 60 s or more wait 4–8 s. They are 14 % of dictations and 58 % of
  audio. About 300 ms of every paste is local: 168 ms waiting for the overlay to fade and exit,
  then 100 ms of key pacing. OpenAI now runs `gpt-transcribe` over a streaming session at the
  same price. Sending audio while you speak should make stop-to-text about 1 s regardless of
  length.
- **There are real defects to fix first:**
  - A host security hole: another app's udev rule makes every keyboard and `/dev/uinput`
    world-accessible.
  - Deleted history comes back after a daemon restart.
  - Deleting a Recovery item while recording wedges the daemon: stop and quit fail, and the mic
    keeps recording.
  - "Paste again" in Recovery pastes into AgentDictate's own window.
  - A startup race can drop a fresh recording into Recovery.
  - Every full test run can press your real Ctrl+Space.
  - The daemon holds one global lock across the whole transcription upload, with several
    subprocesses that have no timeout.

  The happy path is solid. These bugs live in error paths that have almost never run.
- **About a third of the code supports features that are gone or never used.** Examples:
  - The cleanup/Organize pipeline is forced off.
  - All 18 legacy replacement rules are disabled.
  - The model catalog still lists models OpenAI shuts down on 26 February 2027.
  - Seven settings do nothing.
  - Live streaming has never run in production.

  If the decisions in §4 go as recommended, roughly 10–12k of the 41k Rust lines can go. The
  daemon can stop linking the UI toolkit (27 MB → about 10 MB), and the database can shrink from
  19.9 MB to about 6.7 MB.
- **The product is silent when things go wrong.** Failures are rare: a handful of network errors
  and empty captures in 2,000 jobs. When one happens, though, the overlay just disappears; neither
  the overlay, the tray nor a notification says so. The window never shows whether dictation is
  ready, times are in UTC (an hour off in BST), and first run fails silently for a new user. The
  UX work below fixes those before adding features.

## 2. Where it stands

| Area | Measured today |
| --- | --- |
| Volume | 2,000 native jobs since 18 Aug; median 60 dictations and 41 audio-minutes per day |
| Stop → paste (API + Opus, cleanup off, n=1,031) | p50 **1,560 ms**, p90 3,258 ms, p99 7,425 ms |
| Where the p50 goes | HTTP 1,054 ms (68 %) · post-transcript 303 ms (overlay gate 168, delivery 130) · ffmpeg 152 ms · DB/finalize 27 ms |
| HTTP cost model | ≈ 760 ms fixed + 26.5 ms per audio-second (mostly server compute, not upload) |
| Long dictations (≥ 60 s) | 14 % of dictations, 58 % of audio; wait p50 4.0 s, p90 8.1 s |
| Press → recording / → overlay visible | p50 97 ms / 309 ms |
| Reliability | 94 % delivered; 4.9 % Esc-cancelled (all discarded, one after 132 s); 2 transport failures in ~1,130 (never auto-retried) |
| Accuracy | tracked-term misses 36 % (gpt-4o) → 23 % (gpt-transcribe) → **2 %** (+ keywords) |
| Code | 40.9k Rust lines: app 13.6k, ui 11.3k, runtime 7.6k, linux 5.4k, core 3.0k |
| Daemon | 41.5 MB RSS, 0 % idle CPU; the per-recording GPUI overlay helper is ~105 MB and ~1.6 s CPU per dictation |
| Storage | DB 19.85 MB (42 % job rows, of which a single 5.1 KB settings blob repeated 1,083×; 20 % trigram index); logs 32 MB, never rotated out, 71–78 % GPU noise; `target/` 33 GB |

## 3. Targets

| Metric | Now | After Phase 1 | After the full plan |
| --- | --- | --- | --- |
| Stop → paste p50 | 1.56 s | ~1.35 s | **≤ 1.0 s** (gate for making streaming the default) |
| Stop → paste p90 | 3.26 s | ~3.0 s | **≤ 1.6 s** |
| Dictations ≥ 60 s, p50 | 4.0 s | ~3.8 s | **~1 s** |
| Press → overlay visible | 309 ms | ~200 ms | ~200 ms |
| Failures with a visible explanation | ~0 % | ~0 % | **100 %** |
| Rust lines in `crates/` | 40.9k | 40.9k | **~29–31k** |
| Daemon binary | 27.4 MB | 27.4 MB | ~10 MB |
| DB size / growth | 19.85 MB / ~8 KB per dictation | 10.7 MB | ~6.7 MB / ~1.5 KB |

The streaming targets are *inferred* from OpenAI's documented behaviour and today's cost model.
Phase 4 measures them before anything becomes the default.

## 4. Decisions for Thomas

Each decision unlocks work below. The recommendation is what the plan assumes if you agree.

| # | Decision | Recommendation | Unlocks |
| --- | --- | --- | --- |
| D1 | Remove `/etc/udev/rules.d/99-vibetyper-uinput.rules` (world-readable keyboards, world-writable uinput) and install AgentDictate's own rule? Needs sudo and a re-login. | **Yes, now**, unless VibeTyper is still used | Closes a system-wide keylogging/injection hole |
| D2 | Keep the ChatGPT-subscription transcription route? It was last used 31 Aug, spawns `codex` on every dictation, adds ~3.4 s, and imitates Codex Desktop on an undocumented endpoint. | **Delete** | −850 lines; README can lead with the supported API route |
| D3 | Keep the ChatGPT desktop dictation-history import? It polls every 2 s forever, has no opt-out, ignores Save history, and was last used 8 Sep. | **Delete** (or opt-in, off by default) | −720 lines; zero idle wakeups |
| D4 | Retire legacy Replacements? All 18 rules are disabled; Vocabulary replaced them. | **Yes**, replaced by one "Words" screen | −1.2k lines; one correction engine |
| D5 | History search: typo-tolerant full-text search, or plain "contains"? | **Plain contains** (2.5 ms today, ~25 ms at 10×) | −1.65k lines, −5.6 MB |
| D6 | Stream audio to OpenAI while you speak? Audio leaves during speech; cancelled chunks are already billed; rare punctuation seams at pauses are possible. | **Yes, after the Phase 4 evaluation passes** | Long dictations ~4–8 s → ~1 s |
| D7 | Paste while the overlay is still fading? This reverses the documented "overlay exits before paste" rule (`5691dd1`). The bug hunt found no way the fading popup can take focus. It proposes checking once per overlay launch that the window really is override-redirect, and falling back to today's wait if not. | **Yes**, with that per-launch check | −165 ms on every dictation; a stalled overlay can no longer block a paste |
| D8 | Start/stop sounds: they are "on" in your config but were never implemented. Build or delete? | **Delete** the settings; the overlay is the cue | Honest settings |
| D9 | Transcript retention default, and whether Recovery items expire | **Keep forever**; make Delete real; Recovery expires after 7 days | Privacy semantics that match the UI |
| D10 | Make Esc-cancelled recordings over ~5 s recoverable for 24 h? | **Yes** | No more lost 132-second dictations |
| D11 | Does anyone else run AgentDictate (releases, .deb, AppImage)? | Assume **only you** until a release is cut | How much legacy migration and startup machinery can go |
| D12 | May the app run `pkexec` (after you confirm) to install the input-access rule? | **Yes**, with the manual path documented | One-click setup for non-technical users |
| D13 | Settings apply as you change them (no Save button)? | **Yes** | −600 UI lines; fixes the two-window overwrite |
| D14 | One automatic transcription retry on network errors, at the risk of a rare double charge (~$0.005)? | **Yes** | Fixes ~0.2 % of dictations that are lost today |
| D15 | Local offline model (Parakeet) as a fallback? | **Not now** (2× the errors, no working vocabulary biasing, 1.5–2 GB RAM) | — |
| D16 | Housekeeping: one-time clean of `target/` (~30 GB), `apt install libxkbcommon-dev libxkbcommon-x11-dev`, sweep 39 leftover 18-Aug recordings (24 MB) | **Yes** | Deletes the linker workaround; frees disk |
| D17 | Remove personal corpus statistics and business vocabulary from the public docs? | **Yes** | — |
| D18 | Recovery "Paste again" / "Transcribe again": copy to the clipboard and tell you to press Ctrl+V, instead of pasting into the settings window that has focus? | **Copy only** | Recovery works at all (it has never succeeded according to the DB) |
| D19 | When a result arrives long after you pressed stop (5.4 % take over 5 s; up to 180 s on a network hang), copy instead of pasting into whatever is focused by then? | **Yes, above ~8 s** (today's p99) | No surprise pastes after you've moved on |
| D20 | Do you start dictations from the tray? | Keep the tray items, and fix them | After any failure the tray silently ignores Start |

## 5. Plan by phase

Phases are ordered so that each one shrinks what the next must touch. Every phase ends with the
repository's normal delivery: focused tests, `./run-tests.sh`, commit to `main`, `./install.sh`,
and a daemon restart. IDs refer to the lane reports.

### Phase 0: safety and correctness (small, independent fixes)

1. **Host input security** (D1, LNX-1). Remove the VibeTyper rule and install
   `packaging/70-agentdictate-input.rules`. Then show a readiness warning in the app when input
   devices are world-accessible, not only in `install.sh`.
2. **Stop the test gate from pressing your hotkey** (BLD-1). `native_hotkey/tests.rs` presses
   Ctrl+Space on an ungrabbed virtual keyboard. The daemon logged real start and stop commands
   from it on 23 and 31 Aug. Give the test device a name the daemon ignores, and use unbound keys
   (F24).
3. **Startup maintenance race** (SIMP-7 / RT-9 / COR-4). Hotkeys are live about 130 ms after the
   daemon starts. Post-listener maintenance then opens the database with `Runtime::open`, which
   re-runs crash reconciliation. A recording started in that window lands in Recovery with a
   misleading error and nothing is pasted (the audio is kept). Use `open_background_writer` (a
   one-line change). Add a test that runs maintenance during a recording.
4. **History resurrection** (RT-1). Delete and "Clear history" leave the text in
   `dictation_jobs`, and the startup backfill re-creates the history rows. Fix: when a dictation
   completes, write the history row and delete the job row in one transaction. Add a
   delete → restart → still-deleted test. Side effects: this makes "Save history off" truthful,
   and a one-time migration shrinks the DB to 10.7 MB.
5. **Error-path fixes from the bug hunt.** Each fix is S-sized with one focused test, and the report
   specifies each test.

   | ID | Defect | Fix |
   | --- | --- | --- |
   | COR-1 | Deleting any other Recovery item while recording resets the workflow (`daemon.rs:471`). Stop, cancel, quit and the hotkey then all fail, `pw-record` keeps recording with the overlay hidden, ducking stays on, and systemd finally SIGKILLs the daemon. | Reset only when no recording is active. Make error cleanup (`settle()`) clear state without `?` |
   | COR-2 | Recovery "Paste again" / "Transcribe again" inject into the focused window, which is AgentDictate's own settings window. The job then leaves Recovery. | Copy only, and show "Copied — press Ctrl+V" (D18) |
   | COR-3 | A DB or processing error after transcription strands the job in `transcribing`, where it is invisible to Recovery and can't be retried or deleted. If the handler's re-read also fails, the daemon wedges on "Transcribing". | One best-effort `fail_job` that marks the job failed and keeps the raw text. The handler never uses `?` |
   | COR-5 | A panic in a hotkey action thread skips `ActionFinished`, so the dispatch gate stays "in flight" forever and the hotkey dies. | A drop guard that always sends `ActionFinished`. No `expect` on thread spawns |
   | COR-6 | `pactl`, `ffmpeg`, `systemctl` and the codex reader join have no deadline, and they run under the global lock. One hang freezes the app. | 1 s deadline for `pactl`, taken off the start path; a scaled deadline for ffmpeg with WAV fallback; deadlines for systemctl |
   | COR-7 | The tray ignores Start after any failure (`NeedsAttention`). | Treat it like Ready |
   | COR-8 | Nothing ties the paste to the window you were in at stop. 95 % of pastes go to native Wayland windows, where focus identity can't be observed. | Copy-only above ~8 s after stop, or when the X11 window changed (D19) |
   | COR-9 | Failures before any keystroke are recorded as "may already have been pasted", which blocks retry. | A `NotSent` disposition that stays retryable |
   | COR-10 | A poisoned lock exits with status 0, so systemd never restarts the daemon. A rejected Quit makes later SIGTERMs no-ops. | Exit non-zero in both cases |
   | C16 | Esc deletes the WAV immediately, even with "Preserve temporary audio" on. | Honour the setting now; Phase 5 makes long cancels recoverable |
   | C24 | After a crash mid-duck, the next recording saves the ducked volume as the new baseline, so it never comes back. | Durable ducking state (moved up from Phase 3) |
6. **Transcription robustness.**
   - Retry once on connect, send or body errors that arrive before any HTTP status (LAT-3, D14).
   - Upload WebM/Opus instead of Ogg (RES-2). OpenAI's documented format list no longer includes
     Ogg, although it is still accepted today.
   - Retry as WAV if the API rejects the payload format.
   - Retry the stranded 8 Sep dictation once, or let Thomas discard it.
7. **Durable ducking state** (LNX-6 / C24). Write `{sink, original, applied}` before the first
   volume change, and restore it at startup if the volume still equals `applied`. A crash can then
   no longer leave your headset at 15 %, or poison the next baseline.
8. **SQLite WAL + `synchronous=NORMAL` + `BEGIN IMMEDIATE` writes** (RT-3 / LAT-10). This saves
   15–20 ms per paste, and fixes the "database is locked" startup failures seen in the logs.
9. **Logging** (LAT-11 / BLD-4).
   - Make the level configurable, with the GPU crates at `warn`. Today there is only the fixed
     INFO default, and 71–78 % of lines are overlay GPU noise.
   - Keep 14 files.
   - Log time-to-capture, a per-stage paste breakdown, and retries.
   - Rename `stop_to_paste_ms`, which actually measures flow completion.
10. **Hygiene.**
   - Make HEAD `cargo fmt` clean and add `cargo fmt --all --check` to the gate (BLD-6).
   - Scope `cargo deny` to all features on Linux targets, with explicit allow/ignore lists (BLD-5).
   - Declare `ffmpeg` (Recommends) and `pulseaudio-utils` (Depends) in the .deb and INSTALL
     (BLD-7, LNX-10).
   - Sweep audio of terminal jobs at startup (LAT-12, D16).
   - Stop incremental compilation of vendored GPUI (BLD-8).

### Phase 1: quick latency wins (no architecture change)

| Change | Saving |
| --- | --- |
| Paste while the overlay fades (D7). The helper reports `override_redirect` once per launch, and the daemon then sends Dismiss without waiting; it falls back to today's wait if the check fails. Re-run `scripts/test-overlay-desktop.py` (LAT-1 / LNX-2 / COR report) | −165 ms p50, every dictation |
| Start path in parallel: start the mic first and duck on the ducking thread; `pw-record --latency=20ms`; launch the overlay helper at the key press (LNX-3 / LAT-7 / LAT-8) | recording ~98 → ~50 ms; overlay ~100 ms sooner |
| Replace the `yield_now` busy-spins in recorder and subprocess waits with a pidfd poll or short sleeps (LNX-5) | ~50 ms of pegged CPU per recording |
| Read focus with `x11rb` instead of 4 `xdotool`/`xprop` spawns (LAT-9) | −15 ms |
| No sleep after the final key release (LNX-15) | −25 ms of blocked daemon time |
| Esc is ignored unless recording (an atomic check, no thread or IPC per keypress) (LNX-11) | fewer threads and log lines |

The result should be p50 ≈ 1.35 s. Verify with the new stage log lines and
`scripts/test-overlay-desktop.py`.

### Phase 2: delete dead weight

Do this before the architecture work so later phases touch less code. The first rows change
nothing users see. The decision-gated row removes features you have to approve first.

| Deletion | Lines | Notes |
| --- | --- | --- |
| Cleanup/Organize pipeline, second HTTP client, cleanup overlay hack, evaluator cleanup mode (SIMP-1) | ~1,000 | `daemon.rs` already forces it off. Add `serde(alias = "organize")` so old configs load |
| Model catalog → `const TRANSCRIPTION_MODEL = "gpt-transcribe"`; drop legacy profiles; map deprecated names on load (SIMP-2 / RES-4) | ~1,390 | whisper-1 and the gpt-4o-transcribe family shut down 2027-02-26 |
| Pricing tables, repricing on every save, currency (SIMP-3 / RT-7) | ~540 | Price each dictation once at insert; always USD |
| Python-era migrations, 7 dead settings, dead events/fields, unreachable protocol shims (SIMP-4, LNX-7, UI-4) | ~700 | Unknown JSON fields are already ignored |
| `daily_stats` cache + 12 usage queries → one local-time `GROUP BY` (SIMP-5 / RT-6) | ~300 | Also fixes UTC day bucketing (6–17 % of dictations on the wrong day) |
| Runtime/core duplicate snapshot types; one `active` recording state with `settle()`; typed `RecordingMode` / `PasteShortcut` enums with serde aliases (SIMP-8/9/10) | ~450 | |
| Decision-gated: subscription route (D2), ChatGPT import (D3), Replacements (D4), FTS → contains (D5) | ~4,900 | Includes UI and tests. Stop parsing the stored provider strictly first: 893 sessions and 446 jobs hold `chatgpt_subscription`, and a removed variant would fail config and history loading |
| UI gold-plating: animated sidebar, unused theme tokens, test-only API (UI-10) | ~450 | |
| Test slop: tautologies, "deleted feature is still absent" assertions, pixel geometry, Python parity (SIMP-20, UI-9, BLD-11) | ~2,000 | Keep `durable_runtime.rs`, `daemon_flow.rs`, the IPC, delivery, hotkey-gate and overlay-lifecycle suites |
| Docs: delete six dated plans/audits, fix protocol version and Cleanup instructions, add a docs map to AGENTS.md (BLD-2) | ~1,200 doc lines | This plan becomes the only plan doc |
| Build ceremony: the empty `native-hotkey` feature, unused `async-trait`, `scripts/test-dev.sh`, source-grep packaging asserts (BLD-12/14) | ~300 | |

Also in this phase, **make the daemon GPUI-free** (BLD-3). Move the `--overlay-helper` entry into
the `agentdictate` binary and have the daemon spawn its sibling. The daemon binary goes from
27 MB to about 10 MB, and a daemon-only build needs 207 crates instead of 614.

### Phase 3: daemon core, with no lock held across I/O

Today the whole daemon is one `Arc<Mutex<AgentProcess>>`. `StopRecording` holds it through
finalize, encode, the HTTP call (180 s timeout), overlay teardown and paste (SIMP-11). As a
result, Esc can't cancel a slow transcription, and a settings save freezes the window.

- **Processing worker.** Under the lock: finalize the capture and write the `transcribing`
  checkpoint. On a dedicated worker thread: transcribe. Re-lock only to store and deliver. Cancel
  during processing sets a flag, so the text goes to Recovery instead of being pasted. Keep single
  flight and the durable checkpoint order.
- **`DaemonHandle`** (SIMP-12 / RT-11b). Six in-process callers go through the daemon's own
  socket today: hotkey, tray, max-duration, recorder exit, signal handler and hotkey status. They
  call the handle directly instead, and toggle/phase decisions become one atomic `ToggleRecording`
  command. That removes a race where a hotkey press made during another client's processing turns
  into an unintended Start (COR-12). Server sessions get a read timeout (COR-11).
- **Protocol v6** (SIMP-6). Drop `request_id` (never read) and `ClientCommandTag`. The two
  internal-only commands go away with the handle.
- **The recorder owns max duration and stall detection** (LNX-4). Every start path gets the
  limit, and one ticking loop replaces one parked thread per recording. A mic that stops
  delivering bytes becomes a visible, recoverable state.
- **Corrupt DB** (RT-8). Set the file aside, start fresh, and tell the user. Dictation no longer
  crash-loops.

The safety net is `durable_runtime.rs` and `daemon_flow.rs`. New tests:
- cancel during processing;
- settings save during processing;
- crash between delivered and completed.

### Phase 4: streaming transcription (the big latency lever)

Target pipeline (RES-1 + LAT-4 + RES-5):

```text
key press ─► job row (WAL) ─► pw-record --rate 24000 → stdout PCM
                               ├─► durable WAV writer (recovery source of truth, unchanged)
                               ├─► level meter → overlay waveform, near-silence check
                               └─► Realtime transcription session: gpt-transcribe, keywords, language
                                    append PCM; commit at natural pauses
                                    (≥ 10 s uncommitted and ≥ ~700 ms of silence, RMS vs adaptive floor)
stop ─► final commit ─► await every committed item_id ─► join in commit order
     ─► vocabulary normalisation ─► paste
socket error / timeout ─► WebM/Opus of the durable WAV ─► /v1/audio/transcriptions (one retry)
```

- Same model, keywords and price as today ($0.0045/min). OpenAI documents that `gpt-transcribe`
  in a Realtime session uses earlier committed turns as context, so the pieces are not
  transcribed blind.
- One session per dictation, opened at the key press. This also absorbs the idle-connection
  penalty: requests after more than 30 minutes idle take 1.7 s versus 0.7 s warm.
- Clips under ~10 s stay a single commit, so they have no seams. They still skip the encode and
  upload.
- The pipe from `pw-record` replaces the WAV-tailing and file-polling readiness loop. The WAV
  stays the durable artifact, and recovery/retry semantics don't change.
- Rework `live_transcription.rs` in place. It keeps its session, append and `item_id` handling,
  and loses:
  - `gpt-live-transcribe` and `delay`;
  - the linear 16→24 kHz resampler, which measurably distorts 3–8 kHz;
  - WAV tailing;
  - the "Stream speech" toggle.

  It has never run in production, so nothing depends on its current shape.
- **Gate before it becomes the default (D6).** Use `agentdictate-evaluate` on retained and newly
  consented clips. Measure stop-to-final p50/p95 by length, seam artefacts at commit points,
  vocabulary hits, and fallback rate. Ship as default only if p50 ≤ 1.0 s and seams are not worse
  than buffered in blind review. If seams are a problem, try longer commit thresholds, then
  single-commit streaming.
- **If streaming fails the gate:**
  - Delete `live_transcription.rs` and `tungstenite` (LAT-6, ~500 lines).
  - Keep buffered uploads.
  - Add in-process Opus encoding during recording (LAT-4: −150 ms p50, −0.7 to −1.9 s on long
    clips).
  - Segment long recordings at pauses (LAT-5).
  - Pre-warm the HTTP connection at record start.

### Phase 5: product and UX

Ordered by user value:

1. **Failures are never silent** (UI-1).
   - Add a non-interactive overlay state held for ~2.5 s:
     - "Couldn't transcribe · saved";
     - "Couldn't paste · copied";
     - "Didn't hear anything · check your microphone".
   - Send a freedesktop notification with **Try again / Paste again / Open**. Today neither the
     overlay nor the tray icon changes on failure, so failures are invisible unless the window is
     open.
   - Map errors to a typed `FailureKind`, so users never see raw `reqwest` text.
2. **Paste last dictation** (UI-5 / RES-6). A tray item, `agentdictate paste-last` (bindable in
   GNOME), and the notification action. It reuses the delivery gate and the single-injection
   policy.
3. **History basics** (UI-2).
   - Local time with "Today 14:32".
   - Click to expand the full text; 57 % are clipped today.
   - A "Copied ✓" confirmation.
   - Delete per row, and "Delete all".
4. **Readiness on Home** (UI-3). One line: "Ready — press Ctrl+Space anywhere". If anything is
   wrong, show a single fix card instead: shortcut access, paste access, account, microphone. Plus
   a "Reconnecting…" banner on daemon or version mismatch.
5. **Words screen** (UI-6 / RES-3, D4). A list of spellings with optional "sounds like", replacing
   both Replacements and the `X = y` text box. **Fix a word** from History pre-fills an entry
   (UI-13). Accuracy work now targets new names; the known ones are solved.
6. **Settings: six primary controls, the rest under Advanced, applied on change** (UI-4 / UI-8,
   D13).
   - The primary controls are service, language, shortcut, shortcut behaviour, lower other
     sounds, and keep history.
   - Language choices are validated per service.
   - The desktop sends a typed per-field `SettingChange` instead of a whole `Settings`.
   - This deletes both form macros, the draft/dirty machinery and Save/Discard.
7. **Guided first-run setup** (UI-7, D12). A four-step checklist:
   1. Transcription account.
   2. Shortcut and paste access, with a `pkexec` "Grant access" button.
   3. Microphone test with a live level meter.
   4. Try it in a text box.

   Fix the README/INSTALL "turn Cleanup off" step and make the documented default match the code.
8. **Recoverable cancel** (LNX-11, D10). An Esc after ~5 s keeps the recording in Recovery for
   24 h ("Cancelled — transcribe anyway?").
9. **Single-instance window and fast open** (UI-11). Window open went from 66–130 ms to a median
   341 ms after the 24-Aug service bootstrap. That bootstrap rewrites the unit and runs
   `systemctl daemon-reload` on every window launch and every daemon start. Only rewrite when the
   contents differ, raise the existing window instead of spawning a new one, and drop the
   duplicate initial fetch.
10. **GPUI migration with the redesign** (UI-14). Both vendored patches (override-redirect popup,
    X11 window handle) are already in Zed main and `gpui-pre` 0.3.6. Move to `gpui-pre` plus
    `gpui-component` 0.6 with exact pins, delete `vendor/` and `[patch.crates-io]`, and re-run the
    compositor overlay check. Don't do this as a separate project; the redesign rewrites most
    render code anyway.

### Phase 6: data and privacy

- **Honest retention** (RT-4, D9).
  - Usage counters store numbers only.
  - One "Keep transcripts: don't keep / 30 days / forever" setting.
  - Delete and Clear really delete, with `secure_delete` on.
  - Recovery expires after 7 days with an "expires in N days" note.
  - A startup sweep of terminal-job audio and orphan WAVs older than 1 h.
- **Fast clear** (RT-2). "Clear history" blocks dictation for 5–19 s today (95–260 s at 10×)
  because of per-row index triggers. Drop and recreate the search index inside the transaction
  (0.3 s), or it disappears with D5.
- **Schema v1** (RT-12).
  - Add `user_version` and numbered migrations.
  - Merge `dictation_sessions` + `transcript_history` into one `dictations` table.
  - Drop `daily_stats`, `pricing_settings` and `history_search_state`.
  - Imports become `source` / `source_id` columns.
  - Keep a `.pre-v1` backup, then `VACUUM`.
  - Size goes 19.85 → ~6.7 MB, and growth ~8 KB → ~1.5 KB per dictation.
- **The desktop reads SQLite directly (read-only, WAL); IPC carries commands only** (RT-11a /
  UI-12). History, usage and recoveries never wait behind a dictation. `GetWorkspace` /
  `GetHistoryPage` leave the protocol, and the view-model mapping moves into the ui crate.

### Phase 7: Linux platform and installation

- **Clipboard and focus in-process with x11rb** (LNX-9). One selection-owner thread replaces the
  `xsel`, `xdotool` and `xprop` subprocesses (6–8 spawns per paste). A `SelectionRequest` after
  the chord gives the app its first real "the target took the paste" signal. That makes
  clipboard restore safe to add, and lets the overlay say "Copied, press Ctrl+V" when an app
  ignored the paste. Re-prove with the compositor harness, which already checks Wayland and X11
  retrieval.
- **Paste chord for X11 targets** (LNX-14). Re-test Shift+Insert-everywhere with both selections.
  If it passes, delete terminal detection; if not, extend the terminal list (it misses `kgx`,
  `ptyxis` and others).
- **Layout-independent hotkey capture** (LNX-13). You type on AZERTY, and letter hotkeys are
  matched by QWERTY position. Capture keycodes in the daemon and store a display label.
- **Native access roadmap** (LNX-12):
  - the .deb installs the rule;
  - an in-app `pkexec` grant (D12);
  - on GNOME ≥ 48 and KDE, the GlobalShortcuts portal, so no keyboard read access is needed and
    the chord no longer leaks to the focused app. Keep evdev for GNOME 46.
- **Ducking with fewer spawns.** Raise the ramp step to 100 ms, which takes each dictation from
  27 `pactl` calls to about 15.

### Phase 8: release and tooling

- **Releases** (BLD-9). Cut v0.3.0 (the only release predates everything since 21 Aug) and bump
  the version per tag. Run tests in the tag workflow, pin and checksum `appimagetool`, and stop
  bundling excludelist libraries in the AppImage.
- **Installer** (BLD-15).
  - `systemctl --user try-restart` on upgrade.
  - Build only the two shipped binaries.
  - Separate exit codes for "missing access" and "insecure access".
  - `--setup-native-access` shows the sudo commands and asks y/N before running them.
- **Development mode** (BLD-16 / SIMP-18).
  - Running `./run.sh` currently swaps your production service for `cargo run`. Give dev runs
    their own XDG roots.
  - Once D11 is answered, reduce `startup.rs` (1,033 lines of route-identity machinery) to
    "write the unit, `enable --now`, wait for the socket".
- **Dependency trims, optional** (BLD-13/17). Use one CA-store policy for HTTP and WebSocket. If
  compile time starts to matter, consider replacing reqwest (blocking) with `ureq` and trimming
  GPUI's image codecs.
- **Delete the linker workaround** after D16's package install (BLD-10).

## 6. Where the lanes disagreed

| Topic | Positions | Resolution |
| --- | --- | --- |
| Long dictations | Latency: delete streaming and segment long recordings into background file uploads. Research: stream `gpt-transcribe` over Realtime with pause commits. | **Realtime `gpt-transcribe`** (Phase 4). It is documented and the same price, it carries context between chunks, and it reuses the WebSocket code in place. If the evaluation fails, delete streaming and fall back to segmented uploads. |
| Connection pre-warming | Latency: warm at record start (~−100 ms mean). Research: TLS is only 16–26 ms, so skip it. | Both are partly right. The idle penalty (0.7 → 1.7 s) is real but not TLS. A streaming session opened at the key press absorbs it by construction. Only add pre-warming if buffered uploads stay the default. |
| In-process Opus encoding | Latency: build it (−152 ms). Build: it adds a libopus build dependency. | Only needed if streaming fails its gate. Otherwise ffmpeg stays for the fallback path, now declared as a dependency. |
| Overlay gate before paste | Latency, Linux: remove it (−165 ms). Earlier docs: exit-before-paste is a requirement. Bug hunt: no mechanism exists for the fading override-redirect popup to take focus; waiting for unmap instead of exit saves only 10–20 ms. | Check the override-redirect invariant once per overlay launch and paste without waiting (D7). |
| Vendored GPUI | Build: keep it; a migration is not a bump. UI: migrate with the redesign. | Keep it frozen until Phase 5, then migrate once and delete `vendor/`. |
| Search | Simplify: plain `LIKE` (−1,650 lines). Runtime: keep the word index, drop only trigram. | D5. Recommendation: `LIKE`, because at this scale it is 2.5 ms. |
| Pre-roll ring buffer | Research: consider a warm mic. Linux: no; always-on mic, and weak clipping evidence (1 of 40 files). | **No.** Measure time-to-first-speech after Phase 1 first. |

## 7. Deliberately not doing

- **A cleanup LLM on the hot path.** `gpt-transcribe` already produces zero "um"s in 45k words and
  punctuates almost everything. When cleanup was on, it added p50 2.1 s.
- **Noise suppression or AGC.** Studies show enhancement front-ends make modern ASR worse.
- **A second cloud provider, or a local model, without evidence.** The only A/B worth running is
  ElevenLabs Scribe v2 on 60–100 consented clips. Parakeet waits for an offline need (D15).
- **Chunked HTTP upload while recording.** Undocumented, and the upload is only ~40 ms of a
  median request.
- **In-process PipeWire capture, `wl-clipboard`, direct typing, `ydotool`.** Each was rejected
  with evidence: `pw-record` stops in ~8 ms, wl-clipboard re-lays out the GNOME taskbar, and
  typing is layout-dependent.
- **Rewriting the durable core.** Keep:
  - the checkpoint ordering, single-injection/no-auto-retry delivery, and the recovery
    quarantine-delete;
  - the IPC singleton lock and strict protocol version equality;
  - the recorder-owner thread with `PR_SET_PDEATHSIG`;
  - the `HotkeyDispatchGate`;
  - per-job option snapshots, and the near-silence check.

## 8. How we'll know it worked

- **Stage timings in the log** (Phase 0), so the next "why is it slow" is one grep per job:
  capture-ready, gate, clipboard, inject, HTTP or stream finalize, and retries.
- **An evaluation set.** 60–100 consented clips, a held-out third, human-verified references, and
  normal vocabulary lookalikes. Run through `agentdictate-evaluate` for every transcription
  change: streaming, commit thresholds, Scribe v2.
- **Weekly roll-ups from the database.** Stop→paste p50/p90 by length band, the failure rate by
  `FailureKind`, and the Esc-cancel rate. These are the targets in §3.
- **Code size and the gate.** Lines per crate and test count per phase. The full gate stays
  under ~15 s of test time.

## 9. Suggested order of work

1. Phase 0 in two or three days. Items 1–4 and COR-1/COR-2 come first: they are a security hole,
   privacy bugs, and the two defects a user can trigger by clicking.
2. Phase 1 (one day). Measure.
3. Phase 2 deletions: the invisible ones first, then the decision-gated ones as D2–D5 are
   answered. Make the daemon GPUI-free.
4. Phase 3 on its own, behind the durability tests.
5. Phase 4 as a measured experiment, promoted only through its gate.
6. Phases 5 and 6 together, since the UI redesign and the data model changes meet in History,
   Words, Settings and retention. GPUI migrates here.
7. Phases 7 and 8 as release preparation.
