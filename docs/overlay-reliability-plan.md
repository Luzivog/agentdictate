# Make the recording overlay reliable

Status: implemented; focused checks and the comprehensive local gate passed on 2026-09-04.
Source baseline: `8a76841f185cb72f355ce04d8bceee797f17e855` on `main`, also the current remote `main` when checked.

The user reports that the overlay is missing and clarifies that it has never worked fully cleanly. There is no accepted good version to restore. Repair the current implementation against explicit behavior checks, preserving the protections added for lost recordings, focus stealing, failed paste, and taskbar movement.

At the investigation baseline, the immediate disappearance was **not yet diagnosed conclusively**. Today's helpers reach window creation successfully. Two confirmed weaknesses deserve correction: placement uses stale and misleading display geometry, and readiness/tests do not establish visible output. Prove which mechanism explains the reported disappearance before selecting a rendering fix.

The user authorized implementation after this investigation. The historical findings below describe the source baseline; the implementation record at the end describes the repair and its evidence.

## What the evidence establishes

### Current runtime

- The service was active when inspected, with daemon PID 3568, started at 16:01:14 UTC on September 4. Its X11 and Wayland environment references the current graphical session. The installed daemon and existing release artifact have identical SHA-256 hashes. That proves those two artifacts match, not independently that the artifact was built from today's checkout.
- Four recording helpers reported ready today, at 19:08:36, 19:08:48, 19:11:31, and 19:54:36 UTC. The latest flow recorded for about 12 seconds, transcribed, cleaned, destroyed the helper, and submitted paste. The log shows X11 initialization, the AMD GPU, and a refresh loop. It does not show on-screen window coordinates or composited visibility. See the [latest helper startup](/home/luzivog/.local/state/agentdictate/logs/agentdictated.log.2026-09-04:335), [ready acknowledgment](/home/luzivog/.local/state/agentdictate/logs/agentdictated.log.2026-09-04:412), and subsequent teardown/delivery in that log.
- Current monitor enumeration reports DP-1 at `1920x1080+0+0`, primary DP-2 at `1920x1080+1920+0`, and eDP-1 at `1920x1200+3840+0`. The reported X11 work area is `0,0,5760,1032`. `_NET_CURRENT_DESKTOP` is absent; the parser defaults to desktop zero.
- Work area discovery occurs once at daemon startup, then the supervisor passes that same rectangle to later helpers. See [daemon initialization](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/crates/agentdictate-app/src/bin/agentdictated.rs#L47) and [work-area discovery](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/crates/agentdictate-app/src/system.rs#L33).
- The vendored X11 backend's `primary_display()` describes an X screen, whose bounds cover the combined desktop. It does not select the RandR primary monitor. The overlay intersects its cached work area with those bounds. Its placement tests supply a correct monitor rectangle themselves, so they miss this integration error. See [X11 display bounds](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/vendor/gpui-0.2.2/src/platform/linux/x11/display.rs#L15), [primary display selection](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/vendor/gpui-0.2.2/src/platform/linux/x11/client.rs#L1411), and [placement contract](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/crates/agentdictate-ui/tests/contracts/overlay_contract.rs#L112).

The current combined desktop happens to have the same horizontal center as DP-2. Therefore the display abstraction defect alone does not prove today's missing overlay. The rectangle cached at startup is not logged, and no live helper geometry was captured during the user's recording.

### What happened before

| Period | Evidence | What it means for this repair |
| --- | --- | --- |
| May | [GTK overlay refactor, `8f957ab`](https://github.com/Luzivog/agentdictate/commit/8f957ab) retains notification behavior, no focus, no taskbar entry, and placement above the primary monitor work area. | These requirements predate GPUI. |
| August 18–19 | Task **Fix duplicate transcription paste** repeatedly reports dock flashing, failed delivery, lost speech, and an unwanted overlay app icon. The authorized rewrite plan explicitly separates rendering from durable recording. [Original rationale](/home/luzivog/.codex/sessions/2026/08/18/rollout-2026-08-18T13-54-21-01a014b8-f957-73c2-810b-9a03cbff8413.jsonl:3246); [native rebuild, `23ab5fa`](https://github.com/Luzivog/agentdictate/commit/23ab5fa). | An overlay crash must not discard speech or take down the recorder. |
| August 22 | [`5691dd1`](https://github.com/Luzivog/agentdictate/commit/5691dd1) makes X11 popups unmanaged and requires confirmed helper exit before delivery. | Preserve focus neutrality and the delivery gate. An unconfirmed teardown leaves the transcript retryable. |
| August 23–24 | Task **Diagnose AgentDictate hotkey** identifies stale X11 authorization after GNOME/session replacement. Helpers failed before window creation. [`1b7ccdb`](https://github.com/Luzivog/agentdictate/commit/1b7ccdb) adds graphical-session ownership, IPC-owner checks, and helper readiness. [Historical diagnosis](/home/luzivog/.codex/sessions/2026/08/23/rollout-2026-08-23T18-57-34-01a02f8e-5da9-7421-9db7-5dbe6101eace.jsonl:533). | Today's successful initialization differs from that incident. Do not blindly repeat its fix or revert the focus patch. |
| August 24 | [`0f402a4`](https://github.com/Luzivog/agentdictate/commit/0f402a4) splits the overlay process into modules. | Structural cleanup is not evidence of a behavioral regression. |
| August 26 | Task **Trace Ctrl+Space startup delay** measures 38 starts and explicitly distinguishes window creation from compositor display. A persistent helper is proposed, without implementation authorization in that task. [Timing report](/home/luzivog/.codex/sessions/2026/08/26/rollout-2026-08-26T23-19-28-01a03ff1-3817-7100-9cf6-7695c4837f7b.jsonl:385). | Actual visibility and latency were still unproven. A permanent helper would be a separate design change. |
| August 31 | [`a62b77a`](https://github.com/Luzivog/agentdictate/commit/a62b77a) publishes Cleaning while synchronous processing blocks the normal daemon path. | Keep the live cleanup transition and animated processing feedback. |
| August 31 | [`a3ea6ac`](https://github.com/Luzivog/agentdictate/commit/a3ea6ac) adds 100 ms fade-in, 120 ms fade-out, and 150 ms dismissal hold. | Preserve smooth dismissal and exit-before-paste while testing actual frames. |
| August 31 | [`327ac7e`](https://github.com/Luzivog/agentdictate/commit/327ac7e) identifies transient wl-clipboard toplevels as a cause of dash-to-panel movement and uses xsel for both selections. | Preserve clipboard ownership and avoid reintroducing temporary clipboard windows. |
| September 1 | [`8a76841`](https://github.com/Luzivog/agentdictate/commit/8a76841) compresses uploads and logs processing stages. | Keep durable WAV recovery and transcription behavior outside the overlay repair. |

The record contains a material contradiction. The fade commit says panel-side causes were ruled out; the following clipboard commit identifies panel relayout caused by clipboard windows. Preserve the useful behavior of both changes, but do not accept the earlier causal explanation as settled.

The August 23 task's [completion report](/home/luzivog/.codex/sessions/2026/08/23/rollout-2026-08-23T18-57-34-01a02f8e-5da9-7421-9db7-5dbe6101eace.jsonl:1832) asks the user to check bottom-center visibility. No following confirmation appears there. The August 24 **Commit and push changes** task also leaves that check outstanding. The user later reported seeing a delayed overlay, which establishes occasional visibility, not complete acceptance.

### Why existing verification missed this

- `on_ready()` runs immediately after `open_window()`. It cannot certify visibility. [Production callback](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/crates/agentdictate-ui/src/desktop.rs#L245).
- `dismissal_preserves_the_rendered_card_while_it_fades` never calls `begin_dismissal`. It checks bounds rather than the behavior its name describes. [Test](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/crates/agentdictate-ui/tests/desktop/overlay_rendered.rs#L150).
- Numeric opacity tests establish fade arithmetic. Layout tests establish bounds. Shell-helper tests establish process supervision. None establishes pixels, placement, or focus under the actual compositor.
- Helper stderr is discarded. The panic hook writes to the file log, but after-ready panics do not send a structured helper error. The parent observes exit without preserving its exit code or signal. [Child handling](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/crates/agentdictate-app/src/overlay_process/child.rs#L23); [helper error handling](https://github.com/Luzivog/agentdictate/blob/8a76841f185cb72f355ce04d8bceee797f17e855/crates/agentdictate-app/src/overlay_process/helper.rs#L74).

An isolated probe ran the installed helper with synthetic workflow updates, temporary log storage, and a private Xvfb display. The normal GPU path failed surface creation. With software Vulkan, the helper reported ready and produced a mapped, override-redirect `143x56` window at `890,904`, but captured pixels remained black through Recording, Transcribing, and Cleaning. Unlike today's GNOME logs, this environment had no compositor and did not log a refresh loop. This is a reproducible test-environment failure, **not proof of the same cause on GNOME**. An earlier pixel probe also used an incorrect crop; only captures of the discovered window are relevant. Temporary probe processes and files were removed.

## Competing explanations to test

| Priority | Explanation | Decisive observation |
| --- | --- | --- |
| 1 | Cached work area or combined-desktop bounds put the card outside the intended visible area. | Capture the helper's inherited work area and actual window rectangle during failure. Refresh only geometry; visibility and primary-monitor placement must recover. |
| 2 | The surface is mapped but frames remain transparent or cease advancing. Fade introduces an initially transparent state, but timing correlation is insufficient. | On a matching isolated compositor, observe submitted frames and pixels across fade-in. A diagnostic opaque first frame distinguishes alpha from placement or mapping. Do not ship that diagnostic as the fix. |
| 3 | The surface contains pixels but stacking, clipping, or workspace handling hides them. | Compare the window's own pixels with the composited output, while holding geometry and workflow constant. |
| 4 | Session environment or helper lifetime fails intermittently. | Correlate a failing attempt with initialization errors, helper exit status, session identity, or restart exhaustion. Today's four successful initializations weigh against this as the sole explanation. |

Do not claim an animation invalidation bug merely because `request_animation_frame()` sounds insufficient. This GPUI implementation notifies the current view on the next frame. Whether the backend delivers that frame is a separate question.

## Implementation sequence

### 1. Establish an exact reproduction and preserve the baseline

Keep `main` at its current implementation while collecting one failing attempt. Record helper generation/PID, workflow phase, backend, current monitor layout, inherited work area, actual window bounds, mapping, and frame progress. Capture only overlay pixels, without transcript text or unrelated desktop contents.

Build one small isolated-display harness around the real `--overlay-helper` entry point. Feed synthetic workflow states and fixture audio. First establish a visible positive control. Then make the reported case fail with the same rendering/session conditions. Use GNOME/Mutter with XWayland for final compositor-specific checks; Xvfb alone cannot certify Mutter or dash-to-panel behavior.

Preserve a compact baseline trace for fade, helper teardown, paste ordering, and recording durability. Do not launch windows, record audio, or inject input on the user's active desktop during automated checks.

Exit criterion: one repeatable command catches the missing overlay with a geometry or pixel assertion, and a positive control proves the harness can see a working surface. If only the live desktop reproduces it, collect a targeted observation during a user-driven recording before selecting a code fix.

### 2. Make placement use current primary-monitor geometry

Put native monitor/work-area discovery behind one Linux boundary. Resolve the actual RandR primary monitor for the X11 helper, intersect the applicable work area with that monitor, and calculate placement in a single coordinate system. Explicitly convert physical and logical pixels where needed. Do not substitute the combined X screen for a monitor.

Resolve geometry for each helper launch. While a helper is visible, respond to monitor, primary-output, scale, or work-area changes. Reposition only when geometry changes, not on every waveform frame. Keep external queries bounded and keep them off the recording-critical path.

Prefer an application-scoped geometry fix over rewriting GPUI's global display identity model, which also affects settings windows. Return meaningful geometry/error results instead of silently treating an arbitrary virtual rectangle as valid primary-monitor placement.

Exit criterion: real discovery plus placement passes single-monitor, current three-monitor, primary-not-at-origin, negative-origin, mixed-height, and scaled-display cases. Unplugging/reordering a monitor or changing the dock work area updates placement without restarting the daemon. Preserve the current 72 logical-pixel bottom gap and horizontally centered position.

### 3. Repair the proven rendering/lifecycle failure and report health truthfully

Use the reproduction to choose the smallest correction. If geometry explains the disappearance, do not add an unrelated renderer rewrite. If frame delivery fails, fix the mapped/refresh/presentation lifecycle at its owning boundary and retain the popup's focus policy.

Distinguish window-created from first-frame-submitted acknowledgment. Where presentation feedback exists, report it separately. Neither acknowledgment alone proves that a user can see a nontransparent, unobscured card. Keep that assertion in compositor verification rather than naming an internal event `visible`.

Carry bounded failure diagnostics with helper generation, phase, selected backend, geometry, startup/frame timestamps, and exit status. Retain startup timeout, bounded retries, stale-generation filtering, and parent-death cleanup. Avoid an unbounded stderr reader or logging every animation frame.

After repeated presentation failure, expose overlay health through the existing application status surface and preserve durable recording. Do not silently cancel or discard capture because the renderer failed. Keep the daemon usable if presentation is unavailable.

Preserve artistic fade durations, but do not use a longer sleep as evidence that a frame was shown or focus returned. If dismissal completion changes, acknowledge actual helper exit before paste and retain the bounded teardown deadline. A stalled renderer must neither hang delivery indefinitely nor cause paste into an uncertain target.

Exit criterion: the original failure now passes; a helper that creates a window but stalls cannot be reported as fully healthy; failures remain bounded and cannot lose audio or bypass delivery protection.

### 4. Replace weak checks with behavior tests

Extend existing harnesses rather than creating one executable per small scenario.

| Contract | Required proof |
| --- | --- |
| Visibility and placement | Production helper emits visible overlay pixels at the expected primary-monitor rectangle, including after display changes. |
| Recording content | Keep the 143×56 surface, 20 waveform bars, timer, and long-timer space reservation. |
| Processing | The real pipeline observer publishes Transcribing → Cleaning while work is in progress; the helper renders and animates both. |
| Fades | Exercise `begin_dismissal` in the real view. Check start, intermediate, and terminal opacity; a dismissal during fade-in never brightens. Cover both hidden-update and stdin-EOF paths. |
| Focus and taskbar | In an isolated matching desktop, active typing target stays unchanged, the popup never becomes a normal application entry, and clipboard operations do not move panel icons. Test native Wayland and X11 targets. |
| Delivery | Trace fade/dismissal → confirmed helper exit → one paste submission. On teardown failure, retain retryable text and submit no paste. |
| Recovery | Crash before readiness, stall after creation, exit while visible, stale-generation events, and rapid cancel/restart remain bounded. Recording and saved audio survive renderer failure. |
| Session ownership | Recheck the previous session-replacement and singleton-owner cases without reviving a stale daemon or losing Start-on-login preferences. |
| Cancellation | Escape on an empty recording stays a normal cancellation, without false recovery/attention records. |

Use deterministic clock control for view transitions and existing fake process boundaries for supervisor failures. Keep a small real-compositor check for properties those fakes cannot prove. Report unsupported test environments explicitly.

Run the narrow affected checks first, for example:

```bash
cargo test --locked -p agentdictate-ui --test contracts overlay_contract
cargo test --locked -p agentdictate-ui --features test-support --test desktop overlay_rendered
cargo test --locked -p agentdictate-app --test app overlay_lifecycle
cargo check --locked -p agentdictate-ui --features desktop
```

Add the affected pipeline, Linux geometry, startup, and delivery tests as implementation scope requires. Do not use a high test count or compilation success as a substitute for the compositor check.

### 5. Deliver and verify the installed result

After focused checks and the exact reproduction pass, check disk space and active Cargo/linker workloads. Run `./run-tests.sh` exactly once as the comprehensive final gate. Recheck capacity before the release build.

Follow repository delivery policy: commit the approved implementation on `main`, push to `origin/main`, run `./install.sh`, and restart `agentdictated`. Confirm the service owns the expected installed executable and IPC endpoint. Associate the accepted trace with that installed artifact.

Complete the same visibility, placement, focus, stage-transition, and paste-order checks against the delivered build in the isolated matching environment. If live-only acceptance remains necessary, state that check explicitly instead of declaring the overlay fixed from a ready log. Do not declare the work complete with that acceptance still missing.

## Preserve, change, avoid, and risk

- Preserve durable recording, the headless daemon, bounded helper supervision, focus-neutral X11 popup semantics, primary-monitor placement, waveform/timer design, live Cleaning feedback, smooth fades, xsel selection ownership, and exit-before-paste. Keep unrelated accuracy, ducking, and upload-compression work intact.
- Change stale/incorrect geometry discovery, inadequate health evidence, and tests that do not exercise their claimed behavior. Select any additional rendering correction from a confirmed reproduction.
- Avoid broad historical reverts, ordinary Wayland toplevel fallback, always-on helper redesign as part of this repair, arbitrary synchronization delays, and retries that duplicate paste. The August 26 persistent-helper proposal remains separate latency work.
- Risk: GNOME-specific compositing, scaling, and panel behavior cannot be proven by generic headless layout tests. First-frame submission is weaker than compositor visibility. Fixing the overlay must not couple GPU health to audio survival.

## Sources consulted and remaining gaps

| Source category | Status | Coverage |
| --- | --- | --- |
| Source control | found | Mainline and relevant all-ref history, original GTK overlay, Rust migration, selected patches, current tests, and vendored GPUI. Local checkpoint refs were not treated as shipped code. |
| PRs and issues | empty | Authenticated GitHub lists for all states returned no PRs and no issues for `Luzivog/agentdictate`. There were no review conversations to inspect. |
| Project documents | found | Architecture, packaging/native access, latency and accuracy plans, and repository requirements. |
| Task conversations | found | Scoped Codex task inventory and relevant root rollouts; all six top-level Claude project sessions searched. Duplicate fork history was not counted as corroboration. |
| Infrastructure observability | found | Service status/journal, process environment, installed/release hashes, monitor/work-area queries, and retained daemon logs. |
| Error tracking | found | Local panic/startup/processing logs and historical incident traces. No separate error-tracking service was used. |
| Product analytics | no source | No overlay visibility telemetry source exists in the inspected project records. Personal dictation content was unnecessary and was not analyzed. |

Unresolved: the precise cached startup rectangle, actual helper geometry and pixels during today's failed attempts, the exact current compositor failure mechanism, and a complete clean historical acceptance run. No originating scoped conversation was located for the August 31 fade/clipboard changes; their intent comes from commits. Today's investigation does not certify every old fix. The acceptance matrix is how the repair must earn that confidence.


## Implementation record

The installed baseline was tested on an isolated GNOME Shell/Mutter 46.2 session using the same machine's AMD renderer and XWayland. With three monitors of different heights, the old helper reported `ready` but placed its `143×56` surface at `(2650,1072)`, below the middle monitor's 900-pixel extent. The new helper placed the surface at `(888,1072)` on the actual primary monitor and produced visible composited pixels. This proves a disappearance mechanism in the shipped implementation. It does not reconstruct the unrecorded geometry of every earlier failed attempt.

The repair replaces cached daemon geometry with helper-owned RandR monitor discovery and EWMH work-area observation. Coordinates and size conversion stay at the Linux boundary. Primary-monitor and work-area changes trigger placement updates; ordinary animation frames do not. Replies can contain newer display events, so the watcher drains buffered events before blocking again. The old work-area environment transport and duplicate UI placement API were removed, with geometry assertions moved to the production Linux boundary.

GPUI's X11 window now exposes its existing native handle. The global GPUI display model and unmanaged popup policy are unchanged. The helper refuses a session without X11/XWayland before opening a Wayland toplevel. Window creation and first fully faded-in frame submission are separate milestones. The fade starts with the first render, and its deterministic rendered test now actually dismisses the view and checks intermediate and terminal opacity. Frame submission remains weaker evidence than compositor visibility.

The supervisor retains bounded startup, restart, and teardown behavior. It reads errors after frame submission, retains exit status, and records helper generation/PID and phase changes. Presentation failure reaches the workspace through an inotify signal and snapshot field; the application notice clears when a replacement submits a frame. Recording, saved audio, processing, and the delivery gate remain independent of this health indicator.

### Repeatable compositor check

`packaging/test-overlay-desktop.py` runs the production helper with synthetic audio and workflow updates on a private GNOME desktop. It requires GNOME Shell 46+, XWayland, GTK 3, Python GI/Pillow, Tesseract, xrandr, xprop, xwininfo, and xsel. It uses a private session bus and temporary XDG directories. No active-desktop input or microphone capture is involved.

```bash
/usr/bin/python3 packaging/test-overlay-desktop.py target/debug/agentdictated --target x11
/usr/bin/python3 packaging/test-overlay-desktop.py target/debug/agentdictated --scale 2 --target wayland
/usr/bin/python3 packaging/test-overlay-desktop.py target/debug/agentdictated --monitor 1920x1080 --scale 1.25 --target x11
```

The harness checks real composited waveform pixels and recognizes the Transcribing and Cleaning labels. It verifies primary-monitor placement, monitor changes through Mutter's own configuration API, work-area changes, unmanaged window policy, unchanged focus on a real GTK target, no additional managed application entry, and dismissal through both a hidden update and stdin EOF. It also preserves both X11 clipboard selections and verifies standard clipboard retrieval by native Wayland and X11 targets.

The isolated Mutter session exposes the X11 PRIMARY selection to X11 clients but returns no PRIMARY text to the Wayland target. Standard CLIPBOARD retrieval succeeds on both targets. The harness reports this distinction; it does not claim native Wayland middle-click acceptance. It also does not install or certify the user's dash-to-panel extension. The previous xsel integration is unchanged, and its no-toplevel behavior is checked through the managed-window list.

Focused verification covers Linux placement (including negative coordinates and constrained areas), UI overlay contracts and rendered fades, health notification and rendering, helper startup/crash/recovery/dismissal, and daemon recording durability and exit-before-delivery. The final comprehensive gate also exercises the existing cancellation, session ownership, live processing-stage, clipboard, and delivery protections.

Verification results before release installation: the comprehensive `./run-tests.sh` gate ran once and passed (398 Rust tests, one existing ignored test, native-readiness checks, and dependency advisories/bans/licenses/sources). Focused Clippy checks passed for the app, Linux boundary, and desktop UI. The compositor checks passed at 100%, 125%, and 200% helper scale; observed dismissal completed in roughly 170 ms, within the unchanged two-second delivery deadline. Release acceptance uses the same harness against `~/.local/bin/agentdictated` after installation.
