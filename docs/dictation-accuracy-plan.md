# Dictation accuracy: corpus analysis and cleanup plan

Analysis of the full AgentDictate transcript history (2026-05-13 → 2026-08-31:
3,596 transcripts, ~201k words, ~34 dictations/day, ~56 words each) to decide
how to fix mis-transcribed vocabulary. Plan only — nothing implemented yet.

## What the corpus shows

Miss rates for domain terms (raw transcripts, unigram/bigram counts):

| Term | Right | Wrong | Miss rate | Main wrong forms |
| --- | ---: | ---: | ---: | --- |
| serverlord | 4 | 21 | 84% | "server lord" 16, "server load" 5 |
| shadcn | 1 | 5 | 83% | "ChatCN" 5 |
| Claude Desktop | 4 | 10 | 71% | "cloud desktop" 9 |
| Claude Code | 33 | 58 | **64%** | "cloud code" 58 |
| worktree(s) | 69 | 113 | **62%** | "work tree" 75, "work trees" 38 |
| Vercel | 13 | 19 | 59% | "verso" 11, "vercell" 5, "versal" 3 |
| Phonehost | 4 | 5 | 56% | "phone host" |
| Leadlord | 66 | 70 | **51%** | "lead lord" 48, "lead load" 22 |
| Codex | 462 | 21 | 4% | "codecs" 17, "codec" 4 |

Verified **not** errors (checked in context): `Zernio` (spelled out on tape,
correct), `Botlord` (correct, casing varies), `white`, `trust`, `traffic`,
`subagent(s)` (242 hits, 0 misses), `AGENTS.md`/`CLAUDE.md` (surprisingly
perfect), Telnyx, PostHog, Calendly, Convex (59/59 correct).

**Ambiguous, do NOT auto-replace**: bare `cloud` (92 hits — many are genuine
infrastructure "cloud"; only the `cloud code` / `cloud desktop` bigrams are
safe), `server load` (real phrase, though all sampled hits meant serverlord —
verify each before adding), `traffic` (never Traefik in samples), `codec`
(real word, only 4 hits).

Noise to ignore: "Test en deux" mic-test recordings.

Current state: 7 replacement rules exist (codecs, ChatCN, Clode, lead lord,
lead load, verso, Cloud Code) and fired only 43 times — they were added late.
Cleanup AI has never run (`cleaned_transcript == raw` on all 3,596 rows).
`transcription_prompt` is **empty** even though the app already sends it to
the OpenAI transcription API (`crates/agentdictate-app/src/openai.rs:154,302`).

## Pipeline facts that shape the plan

Order is: **speech (with `transcription_prompt`) → optional cleanup LLM →
replacement rules** (`agentdictate-runtime/src/runtime.rs:234`). Replacements
run last, so deterministic rules always get the final word — cleanup can never
"unfix" them. Cleanup failures fall back to the raw transcript (typed
`cleanup_error`, non-fatal). Confirmed cleanup models: `gpt-5.4-nano`
($0.05/$0.40 per 1M), `gpt-5.4-mini` ($0.25/$2.00), `gpt-5.5` ($1.25/$10).

## The plan: three layers, cheapest first

### Layer 1 — Fill `transcription_prompt` (free, zero latency, fixes at the source)

The transcribe API biases recognition toward prompt vocabulary. This is the
highest-leverage change and costs nothing. Proposed value (Settings →
transcription prompt; keep under ~100 words — Whisper-family models only read
the last ~224 tokens):

> Vocabulary: Leadlord, Phonehost, serverlord, Botlord, Zernio, Codex, Codex
> CLI, Claude, Claude Code, Claude Desktop, ChatGPT, OpenAI, Anthropic,
> Convex, Clerk, Vercel, shadcn, Tailwind, TanStack, Zustand, Effect, Vite,
> pnpm, worktree, subagent, AGENTS.md, CLAUDE.md, Traefik, Coolify, Telnyx,
> PostHog, Calendly, Trigger.dev, tmux, systemd, GNOME, Wayland, AgentDictate,
> GPUI, Bitwarden, OpenRouter, Discord, webhook, monorepo, TypeScript.

Expected effect: most of the "cloud code"/"lead lord"/"work tree" class never
happens; Whisper strongly prefers prompted spellings.

### Layer 2 — Extend the replacement rules (free, deterministic, instant)

All case-insensitive, whole-word. New rules:

| Source | Replacement | Note |
| --- | --- | --- |
| cloud desktop | Claude Desktop | unambiguous in this corpus |
| work tree | worktree | 75 hits; always means git worktree here |
| work trees | worktrees | 38 hits |
| server lord | serverlord | 16 hits |
| phone host | Phonehost | |
| versal | Vercel | |
| vercell | Vercel | |
| chat gpt | ChatGPT | |
| open ai | OpenAI | |
| agent dictate | AgentDictate | casing/spacing |

Verify-first (ambiguous, add only after eyeballing future hits): `server load
→ serverlord`, `codec → Codex`. Never add: `cloud → Claude`, `traffic →
Traefik`.

Housekeeping on existing rules: `codecs → codex` should produce `Codex`
(casing); `ChatCN → Shadcn` should produce `shadcn` (the brand is lowercase).

### Layer 3 — Cleanup AI model

> **Decision 2026-09-01:** enabled immediately per Thomas, with `gpt-5.6-luna`
> at reasoning effort `low` ($0.20/$1.20 per 1M — cheaper than gpt-5.4-mini
> since the July 30 price cut) instead of the gpt-5.4-mini suggestion below.
> The measurement loop still applies: audit raw vs cleaned diffs after a week.

Original recommendation was to hold off initially. Reasoning:

- Layers 1+2 are deterministic, free, and add zero latency; together they
  address every high-frequency miss found in 201k words.
- Cleanup adds a serial LLM round-trip (~1–2s) to *every* dictation before the
  paste lands. That's the only real cost (money is noise — see below), and it
  buys correction only for the *long tail*: terms not yet in the glossary, and
  context-dependent cases like "cloud plan" vs "Claude plan" that no
  deterministic rule can safely touch.
- The paste targets are AI coding agents, which tolerate residual typos well;
  the dangerous errors were exactly the high-frequency brand confusions that
  layers 1+2 eliminate.

**If/when enabling it** (the config to flip on, ready to go):

- **Model: `gpt-5.4-mini`, reasoning effort `low`.** Nano is cheaper but
  small models over-edit — restraint ("change nothing unless certain") is the
  hard part of this task, and mini is the smallest confirmed model that
  reliably follows a do-not-rephrase instruction. `gpt-5.5` adds latency and
  30× cost for nothing on 56-word transcripts. Cost at current volume
  (~1,020 dictations/month, ~330 input + ~75 output tokens each): **≈$0.24 /
  month** — genuinely irrelevant; choose on quality and latency only.
- **Style: "Light cleanup"** (the app appends "Keep wording and structure
  close to the transcript. Do not invent details.").
- **Custom cleanup prompt** (replaces the default, which is too vague about
  restraint):

  > You are correcting a dictated prompt that will be pasted into an AI coding
  > agent. Fix only transcription errors: misheard technical terms (use the
  > glossary), punctuation, casing, and clear duplicated filler. Never
  > rephrase, summarize, reorder, translate, or add content. Preserve the
  > speaker's wording and intent exactly. If you are not certain a word is a
  > transcription error, leave it unchanged. Output only the corrected
  > transcript.
  >
  > Glossary: Leadlord, Phonehost, serverlord, Botlord, Zernio, Codex, Claude,
  > Claude Code, Claude Desktop, ChatGPT, OpenAI, Convex, Clerk, Vercel,
  > shadcn, Tailwind, TanStack, Zustand, Vite, pnpm, worktree, subagent,
  > AGENTS.md, CLAUDE.md, Traefik, Coolify, Telnyx, PostHog, Trigger.dev,
  > tmux, systemd, GNOME, Wayland, AgentDictate, GPUI, Bitwarden, OpenRouter.
  > Common confusions: "cloud code" → "Claude Code", "cloud desktop" →
  > "Claude Desktop", "lead lord/lead load" → "Leadlord", "work tree" →
  > "worktree".

## Measurement loop

The DB makes this fully measurable — after shipping layers 1+2, wait ~1 week
(~230 dictations), then re-run the miss-rate analysis on rows created after
the change (`transcript_history.created_at`, compare raw n-gram counts for
the same term table above). Decision rule: if residual domain-term misses are
under ~2/week, skip layer 3 permanently; otherwise enable it and use the
stored `raw_transcript` vs `cleaned_transcript` diff to audit that the model
only fixes vocabulary (any rephrasing → drop to nano-style stricter prompt or
turn it back off).

## Out of scope / rejected

- Bare-word rules for ambiguous tokens (cloud, traffic, codec) — false
  positives corrupt prompts silently, worse than the original error.
- Switching transcription model: `gpt-transcribe` already gets rare terms
  like AGENTS.md and subagent right; the errors are vocabulary-bias, not
  model-quality, problems.
- Local/offline STT: out of scope per product direction.
