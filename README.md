# Helppye

Real-time meeting copilot. Tauri 2 (Rust core) + React/TypeScript frontend,
local-first: local audio capture, local transcription, local LLM integration
planned for a later phase.

## Status

Audio capture and local transcription foundations implemented:

- Microphone capture via `cpal`.
- Windows system-output capture via WASAPI Loopback.
- VAD, speech segmentation, bounded transcription queue.
- Local Whisper transcription through `whisper-rs`.
- Pluggable transcription: `TranscriptionProvider`/`TranscriptionSession` contracts, a
  registry that reports declared capabilities, and per-source session lifecycle with
  stale/duplicate events dropped in the backend. Whisper local runs through this
  interface; streaming backends are contractually represented but not implemented — the
  registry says so instead of pretending. See `docs/transcription-providers.md`.
- Deterministic transcript normalization (whitespace, repeated punctuation, sentence
  capitalization, configurable technical vocabulary) between the provider and the
  timeline. The raw text is never discarded. See `docs/transcript-normalization.md`.
- Guided download/verification default Whisper Base Multilingual model.
- Conversation Timeline utterance/turn assembly preserving source, speaker role,
  timestamps, utterance IDs, segment IDs, and diagnostics. A dedicated per-utterance
  timer finalizes an utterance on silence alone (`same_speaker_utterance_gap_ms`,
  default 1800ms) — it does not wait for the next segment, a flush, or capture to stop.
- Streaming response suggestion: an eligible utterance from the other person
  (`speaker = OtherPerson`, `source = SystemOutput`) automatically triggers an LLM call
  (Ollama local by default, or a user-chosen cloud provider). Provider streams are
  buffered and validated before a reply — or `[SKIP]` — is published to the frontend.
  The prompt is built from an immutable per-utterance snapshot with hard limits
  (2 prior remote utterances, 1 prior user answer, 3000 context chars). Response backends live behind a
  common registry and a generalized OpenAI-compatible client (LM Studio, OpenRouter, any
  compatible endpoint) with validated `base_url`, credential modes, custom headers, and
  sanitized logging. See `docs/response-suggestion.md`.
- End-to-end pipeline telemetry: 12 monotonic milestones per utterance and 5 derived
  latencies, content redacted by default. See `docs/telemetry.md`.
- Transcription benchmark harness (`cargo run --bin benchmark`) for comparing backends on
  latency, WER, and lost technical terms. See `benchmarks/README.md`.
- A full desktop-app onboarding flow (welcome → profile → language → permissions →
  guided audio test → AI provider → review → ready) and a compact session window
  focused on the suggestion, with technical diagnostics (turn/utterance IDs, latency
  breakdown, raw events) moved behind an explicit "developer mode" toggle instead of
  living in the main window. See `docs/onboarding.md`, `docs/design-system.md`, and
  `docs/frontend-architecture.md`.

Answer overlay as a separate floating OS window (the session window is already a
compact, focused view within the single app window — see `docs/design-system.md`
§Janela de sessão) and persistent conversation history (SQLite) are not implemented
yet.

## Stack

- Tauri 2, stable Rust, Tokio
- React 18, TypeScript (strict), Vite, Tailwind CSS, Zustand
- `whisper-rs` / whisper.cpp local speech-to-text
- Ollama (default) or LM Studio / OpenAI / DeepSeek / Anthropic / OpenRouter / any
  OpenAI-compatible endpoint (opt-in) for response suggestion
- SQLite planned, not implemented yet
- `tracing` structured logging

## Layout

- `src/` — React/TypeScript frontend, organized by domain: `app/` (init, routing,
  the `AppScreen` state machine), `components/` (UI primitives), `features/` (one
  folder per screen), `hooks/`, `services/` (typed Tauri command/event wrappers),
  `stores/` (Zustand), `types/`, `utils/`. See `docs/frontend-architecture.md`.
- `src-tauri/` — Rust core (Tauri commands, audio pipeline, transcription,
  model manager, conversation timeline, response suggestion)
- `docs/` — architecture audit, design notes, roadmap, including
  `docs/transcription-providers.md`, `docs/transcript-normalization.md`,
  `docs/response-suggestion.md`, `docs/telemetry.md`, `docs/session-experience.md`,
  `docs/frontend-architecture.md`, `docs/design-system.md`, `docs/onboarding.md`,
  `docs/shortcuts.md`, and `docs/adr/` (architecture decision records)
- `benchmarks/` — transcription benchmark harness fixtures and instructions
- `prompts/` — LLM prompt templates (added in Ollama integration phase)
- `tests/` — cross-cutting/integration tests; Rust unit tests live alongside
  modules under `src-tauri/src`; a few lightweight frontend logic tests
  (`*.test.ts`, run via `npx tsx`) live alongside the modules they cover under `src/`

## Relationship Meetily

`meetily/` (sibling directory, not part of the package) is a reference-only clone
of Zackriya Solutions' Meetily project (MIT licensed), used exclusively for
architecture research. See `docs/meetily-audio-audit.md`,
`docs/third-party-components.md`, and `NOTICE`.

## Validation

```bash
cd src-tauri
cargo fmt --check
cargo check --target x86_64-unknown-linux-gnu
cargo clippy --target x86_64-unknown-linux-gnu -- -D warnings
cargo test --target x86_64-unknown-linux-gnu
cd ..
npm run typecheck
npm run lint
npm run build
```
