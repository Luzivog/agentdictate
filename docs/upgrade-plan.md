# AgentDictate upgrade record, September 2026

This document records the end-to-end upgrade that started from `main` at `aaef150` on
22 September 2026: what the audit found, what was decided, what shipped, and what was
deliberately left out. The other docs describe the product as it is now; this one explains how it
got there.

## How the work was done

An audit ran seven parallel lanes: latency with real usage data, speech-to-text research, code
simplification, Linux integration, UI/UX, build/tests/docs, and runtime/data. A correctness bug
hunt and two skeptical verification passes followed. The verifiers tried to refute 30 load-bearing
claims against the code: 27 held, 3 were partly true and were corrected, and none were refuted.
Implementation then ran in parallel lanes on separate worktrees. Each lane rebased onto `main`,
and `main` only moved forward after the full `./run-tests.sh` gate passed. A final integration
review checked where the lanes met, and its findings were fixed before release.

## What changed

**Correctness and safety**
- Deleted and cleared History now stays deleted. A finished dictation moves its text into History
  in one transaction, and the in-flight job row is removed.
- Error paths that could wedge the daemon or strand a job are fixed:
  - deleting a Recovery item while recording;
  - storage failures after transcription;
  - panics in hotkey actions;
  - poisoned locks;
  - failed Quit.
- Every external command (`pactl`, `ffmpeg`, `systemctl`) has a deadline.
- A crash while audio is ducked no longer leaves the output quiet: the ducking state is written to
  disk and restored at the next start.
- The test gate can no longer press a real hotkey: its virtual keyboard is grabbed and ignored by
  running daemons.
- A corrupt database is set aside instead of crash-looping the daemon.

**Speed**
- The paste no longer waits for the overlay to fade and exit. A per-launch check confirms the
  overlay can't take focus. This took the gate wait from about 160 ms to 0.
- The recorder starts before audio ducking. The overlay launches at the key press. Busy-wait loops
  are gone.
- The upload is encoded to WebM/Opus while you speak, so almost nothing is left to encode at stop.
  On replayed recordings the stop→encoded wait fell from a median of 176 ms to 6 ms, and from
  628 ms to 5 ms for clips of 60 s or more.
- Focus and clipboard handling run in-process over X11, which removes 6–8 subprocess spawns per
  paste. The key chord no longer sleeps after its last release.
- SQLite runs in WAL mode with immediate write transactions.
- Transcription runs on its own thread. The daemon lock is never held during network I/O.

**Transcription**
- One built-in model (`gpt-transcribe`) with vocabulary keywords.
- Uploads use WebM/Opus, fall back to WAV, and retry once on a connection failure.
- The Realtime streaming path was evaluated and then removed (see "Not shipped").

**Product**
- Failures are visible: a short notice on the overlay, a desktop notification with Try again /
  Paste again, and plain wording in Recovery.
- "Paste last dictation" is available from the tray, the CLI and notifications.
- A result that arrives late, or that the target app never took, is copied with a "press Ctrl+V"
  notice instead of being pasted blindly.
- Home shows whether dictation is ready, or one fix card.
- First-run setup covers four steps:
  1. API key check.
  2. Keyboard and paste access, with a one-click grant.
  3. Microphone test.
  4. A "Try it" box.
- History has local times, full text on expand, copy confirmation, and delete.
- The Words screen replaces the vocabulary text box and the legacy Replacements. History has
  "Fix a word".
- Settings apply as they change. Six everyday controls are shown; the rest are under Advanced.
- The shortcut is captured by physical key, so it works on any keyboard layout.
- A second window launch raises the existing window.
- An Esc-cancelled recording longer than 5 s stays in Recovery for 24 hours.

**Data and privacy**
- The schema is versioned with numbered migrations. Sessions and History are merged into one
  `dictations` table.
- "Keep transcripts" offers don't keep, 30 days, or forever. Deletes overwrite the text on disk.
- Recovery items expire after 7 days.
- The settings window reads the database directly and never waits behind a dictation.

**Removed**
- The cleanup/Organize pipeline.
- The model catalog.
- Pricing tables.
- The ChatGPT-subscription route.
- The ChatGPT history import.
- Replacements.
- Full-text search. Search is now a plain "contains".
- Dead settings.
- Python-era migrations.
- The vendored GPUI; the app now uses `gpui-pre` with `gpui-component`.
- The `xsel`, `xdotool` and `xprop` dependencies.

**Build and release**
- The daemon no longer links GPUI: 27 MB became about 12 MB.
- `cargo deny` audits the shipped graph, and formatting is checked in the gate.
- The installer restarts a running daemon, and its exit codes separate "missing access" from
  "insecure access".
- `./run.sh` runs an isolated dev instance.
- The release workflow runs the tests and pins its tools by checksum.
- The docs were consolidated into one current set.

## Decisions

| Decision | Outcome |
| --- | --- |
| Delete the ChatGPT-subscription route | Deleted |
| Delete the ChatGPT desktop history import | Deleted |
| Retire Replacements in favour of vocabulary | Words screen; enabled rules migrate to aliases |
| History search | Plain case-insensitive "contains" |
| Stream audio while speaking | Evaluated and rejected (see below) |
| Paste during the overlay fade | Shipped, with a per-launch focus-safety check |
| Start/stop sounds | Settings deleted (never implemented) |
| Transcript retention | Keep forever by default; real deletes; Recovery expires after 7 days |
| Recoverable Esc-cancel | Kept for 24 h when longer than 5 s |
| One-click native access | `agentdictate setup-access` and the setup screen, via `pkexec` after confirmation |
| Settings save model | Apply on change |
| Automatic transcription retry | Once, only before any HTTP status |
| Local offline model | Not now |
| Recovery retries | Copy only, never paste into AgentDictate's own window |
| Late results | Copy with a notice when they arrive more than 8 s plus 30 ms per audio second after stop |

## Not shipped, and why

- **Realtime streaming** with commits at natural pauses. Latency passed its gate: the overall
  median fell from 914 ms to 790 ms on replayed recordings. Quality failed. At 59–82 % of commit
  points the text differed from buffered transcription, usually a spurious sentence break
  ("everything. And have a" instead of "everything and have a"). The baseline at random positions
  was 11–18 %, and prompting did not help. Single-commit streaming gave no latency gain. Buffered
  upload with encode-while-recording shipped instead.
- **Splitting long recordings into background uploads.** Independent chunks would show the same
  seam problem.
- **HTTP connection pre-warming.** No measurable gain in cold-versus-warm trials.
- **A cleanup LLM, noise suppression, or a second provider.** `gpt-transcribe` already punctuates
  and drops fillers. Enhancement front-ends make modern speech recognition worse. A second
  provider needs a measured win first.
- **The GlobalShortcuts portal.** It is not available on GNOME 46, where this was built and tested.
  Revisit on GNOME 48+ or KDE.
- **Clipboard restore after paste and a failure state on the tray icon.** Notifications and "Paste
  last dictation" cover the need.
- **Host changes that need an administrator.** Removing another application's world-accessible
  input rule, and installing the xkbcommon development packages so the linker workaround can go,
  are left to the machine's owner. The app's Home card and `./install.sh` exit code 3 point at
  the rule when it exists.

## Measuring from here

The daemon log records per-stage timings for every dictation:
- capture-ready time;
- encode time, and whether encoding happened during recording;
- request time;
- gate, focus, clipboard and inject times;
- `stop_to_paste_ms`.

The before/after comparison for end-to-end latency needs a few days of normal use. Compare the
`stop_to_paste_ms` distribution by recording length with the pre-upgrade baseline: median
1,560 ms, p90 3,258 ms.
