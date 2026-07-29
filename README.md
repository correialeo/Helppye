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
  timestamps, utterance IDs, segment IDs, and diagnostics.
- Rule-based local question detection for `OtherPerson` turns from system output,
  with candidate/updated/confirmed/dismissed frontend events and visual highlight.

Answer overlay, Ollama integration, and persistent conversation history are not
implemented yet.

## Stack

- Tauri 2, stable Rust, Tokio
- React 18, TypeScript (strict), Vite, Tailwind CSS, Zustand
- `whisper-rs` / whisper.cpp local speech-to-text
- Ollama and SQLite planned, not implemented yet
- `tracing` structured logging

## Layout

- `src/` — React/TypeScript frontend
- `src-tauri/` — Rust core (Tauri commands, audio pipeline, transcription,
  model manager, conversation timeline, question detection)
- `docs/` — architecture audit, design notes, roadmap, including
  `docs/question-detection.md`
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
