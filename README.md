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
- Guided download/verification default Whisper Base Multilingual model.
- Conversation Timeline utterance/turn assembly preserving source, speaker role,
  timestamps, utterance IDs, segment IDs, and diagnostics. A dedicated per-utterance
  timer finalizes an utterance on silence alone (`same_speaker_utterance_gap_ms`,
  default 1800ms) — it does not wait for the next segment, a flush, or capture to stop.
- Streaming response suggestion: an eligible utterance from the other person
  (`speaker = OtherPerson`, `source = SystemOutput`) automatically triggers an LLM call
  (Ollama local by default, or a user-chosen cloud provider) that streams a suggested
  reply — or a `[SKIP]` marker when the speech doesn't need one — back to the frontend.
  See `docs/response-suggestion.md`.

Answer overlay (a floating window outside the timeline) and persistent conversation
history (SQLite) are not implemented yet.

## Stack

- Tauri 2, stable Rust, Tokio
- React 18, TypeScript (strict), Vite, Tailwind CSS, Zustand
- `whisper-rs` / whisper.cpp local speech-to-text
- Ollama (default) or OpenAI/DeepSeek/Anthropic (opt-in) for response suggestion
- SQLite planned, not implemented yet
- `tracing` structured logging

## Layout

- `src/` — React/TypeScript frontend
- `src-tauri/` — Rust core (Tauri commands, audio pipeline, transcription,
  model manager, conversation timeline, response suggestion)
- `docs/` — architecture audit, design notes, roadmap, including
  `docs/response-suggestion.md` and `docs/session-experience.md`
- `prompts/` — LLM prompt templates (added in Ollama integration phase)
- `tests/` — cross-cutting/integration tests; Rust unit tests live alongside
  modules under `src-tauri/src`

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
cargo test --target x86_64-unknown-linux-gnu
cd ..
npm run typecheck
npm run lint
npm run build
```
