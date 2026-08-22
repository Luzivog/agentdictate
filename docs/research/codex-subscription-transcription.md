# ChatGPT subscription transcription for AgentDictate

Research date: 2026-08-23

## Decision

AgentDictate should not ship ChatGPT or Codex subscription transcription as a supported default today. A controlled replay now proves that a separate local process can use Codex App Server authentication to call the private buffered endpoint successfully. This makes a user-controlled local experiment technically viable, but it does not turn the private endpoint into a documented product contract.

ChatGPT desktop has a plan-backed voice-dictation feature, but OpenAI does not document that feature as an API for third-party applications. OpenAI documents ChatGPT sign-in as subscription access for Codex clients. For general OpenAI API calls, including `POST /v1/audio/transcriptions`, OpenAI directs developers to Platform API keys. Even Enterprise Codex access tokens are documented for trusted Codex automation, with an explicit instruction to keep using Platform API keys for general API calls.

The supported path remains the one AgentDictate already uses: an OpenAI Platform API key and the file-transcription endpoint. The current default model, `gpt-transcribe`, costs an estimated $0.0045 per audio minute. At that rate, 10 hours costs about $2.70, 30 hours costs about $8.10, and 100 hours costs about $27.00. An undocumented subscription-token bridge would trade a modest likely saving for high credential, reliability, and support risk.

A read-only query of this machine's AgentDictate database found 2,770 sessions and 1,203.58 audio minutes between 2026-05-13 and 2026-08-22. AgentDictate recorded an estimated $6.5591 in historical transcription cost. Repricing the same minutes at the current `gpt-transcribe` estimate gives $5.42. The observed pace annualizes to about $19.38. This local aggregate contains no transcript text, but it makes the likely saving unusually small.

This is a no-go for a supported product release, not a technical impossibility. Inspection and live replay identify a working private ChatGPT path. The unknown is whether OpenAI permits any client other than ChatGPT to call it. The public documentation reviewed here does not grant that permission or describe such an interface.

## Implementation status

On 2026-08-23, the owner authorized the personal, explicitly experimental provider described in this report. AgentDictate now has a typed `ChatGptSubscription` route alongside the existing `OpenAiApi` route. Existing installations remain on `OpenAiApi` unless the user selects the subscription source.

The subscription adapter starts a short-lived Codex App Server process, requests the current ChatGPT authentication in memory, requires ChatGPT sign-in, derives the account identifier from the token claim, and posts AgentDictate's WAV recording to the private buffered transcription endpoint. It never reads or imports `~/.codex/auth.json`, persists no ChatGPT credential, removes `OPENAI_API_KEY` from the App Server child environment, retries authentication once after `401` or `403`, and never falls back to paid API transcription.

The provider is persisted on the durable recording job and completed history row, so restart retries retain the original route. Subscription transcription records zero Platform transcription cost. Optional cleanup remains independent: it can use the configured Platform API key, while a cleanup failure still delivers the raw transcript.

The compiled Rust adapter passed a live acceptance using `/usr/share/sounds/alsa/Front_Center.wav` and the signed-in Codex account. ChatGPT returned the expected `Front, center.` transcript. This proves the implemented transport works with the current private contract; it does not resolve the support, policy, quota-accounting, or protocol-stability risks below.

## Keep three voice features separate

OpenAI documents three related but different products:

- **Voice dictation in ChatGPT desktop** records an utterance and inserts editable text into the composer. The documented shortcut is `Ctrl+Shift+D`. [ChatGPT prompting: Use voice dictation](https://learn.chatgpt.com/docs/prompting#use-voice-dictation)
- **ChatGPT Voice in Desktop** is a duplex conversation feature. OpenAI says it uses GPT-Live for the conversation and has a separate plan-dependent allowance. That allowance does not establish how composer dictation is metered. [ChatGPT and Codex pricing: ChatGPT Voice in Desktop](https://learn.chatgpt.com/docs/pricing#chatgpt-voice-in-desktop)
- **OpenAI API transcription** is the developer product. A completed recording uses `POST /v1/audio/transcriptions`. Live audio uses a Realtime transcription session. Both are usage-priced API products. [Transcription overview](https://developers.openai.com/api/docs/guides/transcription), [file transcription guide](https://developers.openai.com/api/docs/guides/speech-to-text), and [Realtime transcription guide](https://developers.openai.com/api/docs/guides/realtime-transcription)

The first feature proves that a ChatGPT plan includes dictation in the ChatGPT product. It does not prove that the plan includes general-purpose transcription calls from AgentDictate.

## Facts established by OpenAI documentation

### ChatGPT sign-in is scoped to Codex clients

Codex supports ChatGPT sign-in for subscription access and API-key sign-in for usage-based access. OpenAI names the supported ChatGPT-authenticated clients as ChatGPT desktop, Codex CLI, the IDE extension, and Codex cloud. It does not describe ChatGPT sign-in as general OpenAI API authentication. [OpenAI authentication for ChatGPT and Codex](https://learn.chatgpt.com/docs/auth#openai-authentication)

OpenAI draws a harder line for automation. Enterprise workspaces can issue Codex access tokens for trusted, non-interactive Codex workflows, but the same documentation says to continue using Platform API keys for general OpenAI API calls. Plus and Pro do not get this Enterprise token capability in the current feature matrix. [Codex access tokens for enterprise automation](https://learn.chatgpt.com/docs/auth#use-codex-access-tokens-for-enterprise-automation) and [feature availability](https://learn.chatgpt.com/docs/pricing#feature-availability)

The local ChatGPT/Codex credential cache is therefore not a supported AgentDictate credential source. The authentication guide also warns that the cache contains access tokens and must be treated like a password. AgentDictate should never read, copy, or import it. [Credential storage](https://learn.chatgpt.com/docs/auth#credential-storage)

### Dictation is a plan feature, not an API-key feature

The current ChatGPT and Codex feature matrix lists voice dictation across ChatGPT plans and shows it as unavailable under API-key authentication. This is evidence that OpenAI treats dictation as a ChatGPT product entitlement rather than as the normal Platform transcription API. [ChatGPT and Codex feature availability](https://learn.chatgpt.com/docs/pricing#feature-availability)

The official pricing and dictation pages do not state:

- which model performs that transcription;
- whether dictation draws from Codex credits, the ChatGPT Voice allowance, another allowance, or a fair-use pool;
- whether OpenAI permits a third-party client to use the same service;
- any OAuth scope or SDK for third-party dictation.

OpenAI does document limits and models for ChatGPT Voice in Desktop. That adjacent documentation makes the absence of equivalent dictation details worth preserving as an unknown rather than filling with an assumption. [ChatGPT and Codex pricing](https://learn.chatgpt.com/docs/pricing#chatgpt-voice-in-desktop)

### The installed desktop package reveals the private endpoints

The installed first-party package is `chatgpt 26.818.31338`, with bundled `codex-cli 0.149.0-alpha.4`. Static source inspection found two dictation paths:

- Normal dictation sends `POST https://chatgpt.com/backend-api/transcribe`. The multipart request contains a file, normally `audio/webm`, with a `codex.<extension>` filename and an optional `language` field. The client expects JSON with a `text` field. Evidence: `/usr/lib/chatgpt/resources/app.asar!/webview/assets/app-initial-BOhZp99F.js:8730-8733` and `/usr/lib/chatgpt/resources/app.asar!/.vite/build/window-all-closed-BazhJdtt.js:11`.
- Optional streaming dictation connects to `wss://chatgpt.com/backend-api/dictation/stream`. It sends mono PCM16, uses server voice-activity detection, and waits for final transcript events. The UI falls back to the multipart endpoint if streaming fails or returns no usable transcript. Evidence: `/usr/lib/chatgpt/resources/app.asar!/webview/assets/app-initial-BOhZp99F.js:8725`, `/usr/lib/chatgpt/resources/app.asar!/.vite/build/main-B2sRTTQY.js:854`, and `/usr/lib/chatgpt/resources/app.asar!/webview/assets/global-dictation-page-DR5X5r97.js:1`.

The renderer does not attach a reusable bearer token. It marks the request for the Electron main process, which obtains ChatGPT authentication from the local App Server, adds bearer and account context, attaches an integrity state, and can retry once after an eligible `401`. Evidence: `/usr/lib/chatgpt/resources/app.asar!/.vite/build/main-B2sRTTQY.js:854`, `/usr/lib/chatgpt/resources/app.asar!/.vite/build/src-Bqg9CB1K.js:662`, and `/usr/lib/chatgpt/resources/app.asar!/.vite/build/window-all-closed-BazhJdtt.js:11`.

ChatGPT login itself runs through App Server methods such as `account/login/start` and `account/login/completed`. The desktop bundle uses a private auth-status call that can return a token to its privileged main process. The standalone `codex-cli 0.149.0` generated schema does not expose that token-returning shape. This difference is another sign that AgentDictate must not treat the desktop's private bridge as a public credential API.

These artifacts establish what the current desktop build does. They do not establish permission, stability, model identity, or usage accounting. AgentDictate cannot point `ReqwestOpenAiTransport` at `/backend-api/transcribe` and expect parity. The first-party request depends on ChatGPT bearer auth, account context, integrity state, and desktop-owned refresh behavior.

### A controlled live capture confirmed the buffered contract

On 2026-08-23, temporary in-memory hooks observed one user-authorized dictation through the running first-party ChatGPT/Codex application. The hooks recorded structural metadata only. They redacted authentication and account values, did not retain audio or transcript text, and were removed after the response. The Node inspector used to install and read the hooks was also closed.

The observed request was:

```text
POST https://chatgpt.com/backend-api/transcribe

Authorization: Bearer [REDACTED]
ChatGPT-Account-Id: [REDACTED]
Content-Type: multipart/form-data; boundary=----codex-transcribe-<UUID>
originator: Codex Desktop

file:
  filename: codex.webm
  Content-Type: audio/webm;codecs=opus
  bytes: 33,019
```

The complete multipart body was 33,253 bytes. It contained one part named `file`; this request had no `language`, `model`, `prompt`, or `response_format` part. The hook at `electron.net.fetch` did not observe an integrity header on this request. Chromium may add ordinary browser headers, such as `User-Agent`, after that call.

The backend returned HTTP `200` after about 2.8 seconds with `Content-Type: application/json`. The JSON object had four keys: `asset_format`, `asset_pointer`, `asset_ttl`, and `text`. The reported text length was 16 characters, exactly matching the submitted message, `Okay, it's done.` No streaming WebSocket events occurred.

This closed the earlier desktop technical-proof gap. The next experiment tested replay from a separate process.

### A separate helper replay also succeeded

A one-shot Node helper then spawned `codex app-server` over stdio with `OPENAI_API_KEY` removed from its child environment. It initialized with a truthful probe client name, called the undocumented `getAuthStatus({ includeToken: true })`, required `authMethod: "chatgpt"`, and read the account ID from the nested `https://api.openai.com/auth.chatgpt_account_id` claim. The token and account ID remained in memory and were never printed or written to disk.

The helper sent the system fixture `/usr/share/sounds/alsa/Front_Center.wav`, which is 1.428 seconds of mono PCM16 speech, without modifying or transcoding it:

```text
POST https://chatgpt.com/backend-api/transcribe

Authorization: Bearer [REDACTED]
ChatGPT-Account-Id: [REDACTED]
Content-Type: multipart/form-data; boundary=----codex-transcribe-<UUID>
originator: Codex Desktop
User-Agent: Mozilla/5.0 (...) Chrome/151.0.0.0 Safari/537.36

file:
  filename: codex.wav
  Content-Type: audio/wav
  bytes: 137,134
```

The complete body was 137,354 bytes. The endpoint returned HTTP `200` with `Front, center.` after 2.909 seconds. The transcript matched the fixture, and the JSON response had the same four keys as the desktop request. The WebM fallback did not run. No ambient API key was present, no API key header was constructed, and the only network destination in the helper was the private ChatGPT endpoint.

The helper exited normally, no additional App Server process remained, and no temporary audio or credential file was created. This proves the core subscription-backed transcription path can be reproduced outside Electron and accepts AgentDictate's existing WAV format. The remaining blockers are private-interface stability, credential handling, policy, quota accounting, and production error behavior.

### An earlier T3 Code prototype proved only limited feasibility

A July 2026 T3 Code experiment previously attempted the private multipart route. It launched a short-lived `codex app-server` under a selected account's `CODEX_HOME`, used the undocumented `getAuthStatus({ includeToken: true })` call, rejected non-ChatGPT authentication, and sent a server-side multipart request to `/backend-api/transcribe`. It refreshed authentication once after a `401` or `403` and did not deliberately fall back to an API key.

That experiment is useful architecture evidence, but it was not a successful live transcription proof:

- its successful transcript tests used a fake App Server peer and stub HTTP response;
- the live request used invalid audio and reached ChatGPT's validator, which returned HTTP `400` with `Unable to determine audio duration`;
- no recorded live request returned HTTP `200` or an intelligible transcript;
- the transcription files remained untracked in a dirty worktree and were never committed;
- the checkout and installed T3 artifact no longer exist, so there is no current executable to validate.

The experiment also exposed a credential-isolation trap. It deleted `OPENAI_API_KEY` from a copied child environment but used a process launcher that merged the parent environment back in. A Rust helper would need `Command::env_remove("OPENAI_API_KEY")` or a fully explicit environment, with a test that places the key in the real parent process. This matters even when the flow rejects API-key authentication: an ambient key can still change which account App Server selects or sees.

The reusable ideas are the short-lived helper, account-specific `CODEX_HOME`, strict ChatGPT-auth check, one refresh retry, bounded inputs, typed errors, and keeping credentials out of the UI. The private endpoint, desktop-header imitation, and token-returning auth method are precisely the parts that should not be carried forward.

### Codex App Server exposes an experimental audio path

OpenAI documents Codex App Server as the interface for embedding Codex authentication, conversations, approvals, and streamed events into another product. Its standard transport is stdio. The alternative WebSocket transport is explicitly experimental and unsupported for production workloads, and clients can opt into methods outside the stable protocol with `experimentalApi`. [Codex App Server](https://learn.chatgpt.com/docs/app-server)

The installed `codex-cli 0.149.0` can generate a version-matched schema with:

```text
codex app-server generate-json-schema --experimental --out <temporary-directory>
```

That first-party generated schema contains experimental requests named `thread/realtime/start`, `thread/realtime/appendAudio`, and `thread/realtime/stop`. It also contains `thread/realtime/transcript/delta` and `thread/realtime/transcript/done` notifications. `ThreadRealtimeStartParams` requires a thread ID and either text or audio output, supports WebSocket or WebRTC upstream transport, and describes handoffs to a backing Codex model. `ThreadRealtimeAppendAudioParams` accepts base64 audio plus a sample rate and channel count. The final transcript notification contains a role, text, and thread ID.

The generated schema has no method named `dictation` or `transcribe`. The public App Server documentation does not mention the realtime methods at all. This looks like a thread-scoped voice-conversation protocol that happens to emit transcripts, not a stable speech-to-text API. It remains the safer credential boundary if OpenAI documents it for transcription, but the direct buffered route is now the only path proven to return a pure transcript.

This candidate needs a real entitlement test. The schema does not say whether it consumes the ChatGPT Voice allowance, Codex credits, another quota, or Platform billing. It also does not promise that user-role transcript events are complete enough to replace file transcription.

### The supported transcription API uses Platform authentication and billing

The API quickstart requires a Platform API key, and its examples send that key as a bearer token. It also routes developers to Platform billing for higher limits. [OpenAI API quickstart](https://developers.openai.com/api/docs/quickstart#create-and-export-an-api-key)

For bounded recordings, OpenAI recommends `gpt-transcribe` and `POST /v1/audio/transcriptions`. The official request example uses `Authorization: Bearer $OPENAI_API_KEY`. [File transcription guide](https://developers.openai.com/api/docs/guides/speech-to-text)

For live audio, OpenAI recommends `gpt-live-transcribe` in a Realtime transcription session over WebSocket or WebRTC. The public endpoint catalog names `v1/realtime/transcription_sessions`. [Realtime transcription guide](https://developers.openai.com/api/docs/guides/realtime-transcription) and [`gpt-live-transcribe` model page](https://developers.openai.com/api/docs/models/gpt-live-transcribe)

Current estimated API prices are:

| Model | Workflow | Estimated price per minute |
| --- | --- | ---: |
| `gpt-transcribe` | Completed file | $0.0045 |
| `gpt-4o-mini-transcribe` | Completed file | $0.003 |
| `gpt-4o-transcribe` | Completed file | $0.006 |
| `gpt-live-transcribe` | Live transcription | $0.017 |

Source: [OpenAI API pricing: transcription models](https://developers.openai.com/api/docs/pricing#transcription-models).

## What AgentDictate does now

The installed application uses the Rust implementation.

- `crates/agentdictate-app/src/openai.rs:163-200` stores an API key in `ReqwestOpenAiTransport`, sets `https://api.openai.com/v1` as the default API base, and builds a bearer authorization header.
- `crates/agentdictate-app/src/openai.rs:239-323` reads the completed WAV file and posts a multipart request to `{api_base}/audio/transcriptions`.
- `crates/agentdictate-app/src/process.rs:42-60` constructs that transport from `settings.openai_api_key` and injects it into `OpenAiTranscriber`.
- `crates/agentdictate-core/src/settings.rs:149-159` defaults transcription to `gpt-transcribe`. It also enables text cleanup with `gpt-5.4-nano` by default.
- `crates/agentdictate-app/src/openai.rs:352-398` sends cleanup through `{api_base}/responses` with the same API key.
- A read-only aggregate query against `~/.local/share/agentdictate/agentdictate.sqlite` counted rows and summed duration and estimated cost from `dictation_sessions`. It did not select transcript or error text.

The aggregate is reproducible without reading user content:

```sql
SELECT
  COUNT(*),
  MIN(started_at),
  MAX(started_at),
  ROUND(SUM(duration_seconds) / 60.0, 2),
  ROUND(SUM(COALESCE(estimated_transcription_cost, 0)), 4)
FROM dictation_sessions;
```

The last point matters. Replacing only transcription authentication would not make AgentDictate subscription-only while cleanup remains enabled. A complete no-API-key design would also need a supported cleanup provider, a local cleanup model, or an explicit choice to disable cleanup.

The existing `OpenAiTransport` trait is a useful implementation seam. If OpenAI later publishes a supported ChatGPT-plan transcription flow, AgentDictate can add another transport without rewriting recording, persistence, or delivery. The current concrete transport combines the endpoint and credential, so the new route would still need an explicit authentication type rather than a string labeled `api_key`.

## Inferences, not confirmed facts

- The private `/backend-api/transcribe` service may use a public transcription model behind a product gateway or another model. The bundle does not identify it.
- A bearer token that works in ChatGPT or Codex is not proof that it is valid or authorized for `api.openai.com/v1/audio/transcriptions`. Token audiences and product entitlements can differ even when one company operates both services.
- Calling `codex exec` or the Codex SDK does not expose a documented audio-transcription input. The generated experimental App Server schema does expose realtime audio and transcript events, but those methods are absent from the published protocol reference and are shaped as a voice conversation.
- A private ChatGPT endpoint could change with any desktop release. It would have no public versioning, compatibility promise, or error contract for AgentDictate.

## Unknowns that block a supported default release

These questions need an answer from OpenAI documentation or support before code work:

1. Does OpenAI offer a supported OAuth flow that lets a native third-party app use a personal ChatGPT plan for speech-to-text?
2. Is ChatGPT desktop composer dictation exposed through a public endpoint, SDK, plugin capability, or Codex app-server method?
3. Which entitlement and limit does composer dictation consume?
4. May a third-party client store or refresh the resulting credential?
5. Which retention, training, residency, and workspace policies apply to audio sent through that product path?
6. Is personal use by the subscription owner treated differently from distributing AgentDictate to other users?
7. If subscription transcription becomes unavailable mid-request, may AgentDictate fall back to paid API usage, or must it fail closed to avoid an unexpected charge?
8. What allowance and data policy apply to the experimental App Server thread-realtime protocol?
9. Can App Server return a complete user transcript without starting or charging for a Codex response?

An internal endpoint name alone would answer none of these permission and support questions.

## Risk and supportability verdict

| Area | Private ChatGPT endpoint | Experimental App Server realtime | Platform API route |
| --- | --- | --- | --- |
| Official support | No documented third-party flow | First-party generated schema, absent from the public method reference, production unsupported | Documented developer product |
| Authentication | Would require reusing or reproducing a ChatGPT client session | App Server owns the existing Codex login | Project API key |
| Credential safety | High risk because the token also protects ChatGPT or Codex access | Better: AgentDictate need not receive the upstream credential | Scoped to the Platform project and replaceable |
| Protocol stability | Private desktop implementation can change without notice | Explicitly experimental and version-specific | Versioned public API and model documentation |
| Billing | Allowance and accounting unknown | Allowance and accounting unknown | Published per-minute estimate and usage records |
| Failure behavior | Unknown errors, limits, and refresh behavior | Generated schema exists, but realtime semantics are undocumented | Documented HTTP API behavior |
| Distribution | No documented permission for AgentDictate users | App Server is an embedding interface, but these methods have no production promise | Intended for application integrations |

Verdict: **do not ship the private ChatGPT endpoint as a supported or default provider.** The completed replay makes an explicit local experimental provider technically defensible if the user accepts protocol drift and policy uncertainty. Keep the Platform route as the stable default, isolate the private adapter, and make it removable without disturbing recording, history, or delivery.

## Phased plan

### Phase 0: Get the missing product answer

1. Ask OpenAI support one narrow question: "Can a native application owned and used by the ChatGPT subscriber call `https://chatgpt.com/backend-api/transcribe` or the ChatGPT dictation-stream service and consume the subscriber's composer-dictation allowance? If yes, which public endpoint, OAuth scope, SDK, integrity requirements, and usage policy apply?"
2. Ask separately whether Codex ChatGPT-login tokens may call any audio transcription method. Include OpenAI's own documentation that directs general API calls to Platform API keys.
3. Save the written response or public documentation link with this note. A successful private request is not approval.
4. Stop this route if OpenAI answers no, points to the Platform API, or cannot name a supported interface.

Exit criterion: OpenAI provides a public interface or written support statement with enough detail to implement and operate it.

### Phase 1: Prove capture and replay, completed

1. Observe one first-party desktop request without retaining audio, transcript text, or credential values.
2. Reproduce its multipart contract from a separate one-shot helper.
3. Spawn App Server over stdio with `OPENAI_API_KEY` removed and require ChatGPT authentication.
4. Keep the token and account claim in memory only.
5. Send a fixed system WAV and require the returned text to match its known speech.
6. Stop the helper and verify that no extra App Server, inspector, temporary audio, or credential file remains.

Result: both the desktop request and separate replay returned HTTP `200`. The replay accepted AgentDictate's WAV format and returned the expected transcript. Technical feasibility is proven. Allowance accounting, supportability, and production error behavior remain open.

### Phase 1b: Measure desktop dictation accounting

1. Record the ChatGPT desktop version and the signed-in plan.
2. Transcribe the same fixed fixture through composer dictation and the one-shot helper, then compare transcripts and allowance changes.
3. Confirm only that the already identified multipart or streaming path ran. Do not capture authorization, cookies, bodies, account IDs, integrity state, or query strings.
4. Stop if measurement requires installing a TLS interception certificate, modifying the ChatGPT package, or exposing credential values.

Exit criterion: a measured allowance change for desktop dictation and enough metadata to compare it with App Server realtime. This remains research evidence, not implementation authority.

### Phase 2: Choose a supported product design

If OpenAI does not publish a stable subscription flow and the private experiment is not acceptable:

1. Keep `ReqwestOpenAiTransport` and Platform API-key authentication.
2. Keep `gpt-transcribe` as the default file workflow unless representative AgentDictate recordings show that another model is materially better.
3. Show the published per-minute estimate and local recorded minutes in settings so users can understand likely cost.
4. Keep cleanup billing separate in the UI because cleanup uses the Responses API.
5. Consider a local or OpenAI-compatible transcription provider as a separate product decision. Do not disguise it as ChatGPT subscription use.

If OpenAI publishes a supported subscription flow or promotes the App Server methods to a documented production contract:

1. Add a typed provider choice such as `OpenAiApi` or `ChatGptSubscription`. Missing persisted values must default to `OpenAiApi`, so upgrades do not silently change the user's billing route.
2. Split the current transport into a speech interface and a cleanup interface. Route speech through either the Platform API or the supported subscription adapter; keep Responses API cleanup independent.
3. Persist the selected provider on each durable job and completed session, not only in current settings. A retry after restart must use the original route, and history repricing must know whether a request incurred Platform cost.
4. Use only the documented OAuth, device, or App Server flow. Do not import `~/.codex/auth.json`, desktop cookies, integrity state, or a raw ChatGPT token.
5. Store any documented refresh credential in the operating-system credential store. Keep it out of config, logs, diagnostics, history, and crash reports.
6. Keep subscription transcription and paid API fallback as explicit user choices. Never charge the API key after a subscription limit without confirmation. The existing paid `whisper-1` completeness retry must remain inside the API provider.
7. If subscription transcription succeeds but API cleanup is unavailable, deliver the raw transcript. Do not turn a cleanup-key problem into a lost dictation.
8. Add typed provider errors for missing authentication, expired sessions, missing entitlement, rate limits, service failures, malformed responses, and empty transcripts; persist only sanitized user-facing text.
9. Update settings to show the active transcription source and live Codex connection state. In subscription mode, label the model as managed by Codex unless the supported contract exposes model choice.

Exit criterion: mocked contract tests pass and the design has no dependency on ChatGPT's private credential cache.

### Phase 3: Validate the real entitlement

1. Use a dedicated test account or workspace with explicit permission.
2. Transcribe the same short, medium, and five-minute fixtures through ChatGPT desktop and AgentDictate.
3. Compare text, latency, language handling, cancellation, offline errors, quota exhaustion, and token refresh.
4. Verify the ChatGPT usage dashboard changes as OpenAI documented and the Platform usage dashboard records no transcription charge.
5. Exercise 401, 403, 429, network loss, expired refresh credentials, and a desktop or API version change.
6. Confirm that diagnostics contain no audio, transcript, cookie, authorization header, refresh token, or account identifier.
7. Run focused Rust transport and workflow tests, then the repository's final gate once after disk-heavy gate coordination.

Exit criterion: the real account consumes the documented subscription allowance, produces no Platform charge, refreshes credentials correctly, and passes the security review.

### Phase 4: Release behind a kill switch

1. Mark the route experimental until OpenAI states that the interface is stable.
2. Add a remote or local kill switch that disables new subscription requests without deleting credentials or changing the user's paid-API setting.
3. Report the active provider and whether the last request used subscription allowance or Platform billing.
4. Fail closed if entitlement accounting is ambiguous.
5. Remove the route if OpenAI withdraws support or the endpoint requires private-client behavior.

Exit criterion: two release cycles without protocol drift, billing ambiguity, or credential incidents.

## Recommendation for the next decision

The next decision is whether to build a personal, explicitly unsupported provider while Phase 0 remains unresolved. If authorized, implement the typed provider and persistence design in Phase 2, keep the Platform route as the default, and do not add automatic paid fallback. Do not present the private route as supported ChatGPT integration.

The existing local history already supplies the cost baseline: 1,203.58 minutes across about 102 days, which annualizes to roughly $20 at today's `gpt-transcribe` estimate. Unless usage grows sharply, subscription transcription is worth pursuing for product simplicity or account experience, not for present-day cost savings.
