# Helppye

Real-time meeting copilot. Tauri 2 (Rust core) + React/TypeScript frontend,
local-first: local audio capture and local transcription, with local LLM integration
planned for a later phase.

## Status

Audio capture and local transcription foundations are implemented:

- Microphone capture via `cpal`.
- Windows system-output capture via WASAPI Loopback.
- VAD, speech segmentation, and bounded transcription queue.
- Local Whisper transcription through `whisper-rs`.
- Guided download/verification of the default Whisper Base Multilingual model.
- Conversation Timeline that merges completed transcripts from microphone and system
  output while preserving source, speaker role, order, and timestamps.

Question detection, answer overlay, Ollama integration, and persistent conversation
history are not implemented yet.

## Stack

- Tauri 2, stable Rust, Tokio
- React 18, TypeScript (strict), Vite, Tailwind CSS, Zustand
- `whisper-rs` / whisper.cpp for local speech-to-text
- Ollama and SQLite planned, not implemented yet
- `tracing` structured logging

## Layout

- `src/` — React/TypeScript frontend
- `src-tauri/` — Rust core (Tauri commands, audio pipeline, transcription, model
  manager, conversation timeline)
- `docs/` — architecture audit, design notes, roadmap
- `prompts/` — LLM prompt templates (added in Ollama integration phase)
- `tests/` — cross-cutting/integration tests; Rust unit tests live alongside modules
  under `src-tauri/src`

## Relationship With Meetily

`meetily/` (sibling directory, not part of the package) is a reference-only clone of
Zackriya Solutions' Meetily project (MIT licensed), used exclusively for architecture
research. See `docs/meetily-audio-audit.md` for the full audit and
`docs/third-party-components.md` for anything adapted from it, with attribution tracked
in `NOTICE`.

## Development

```bash
npm install
npm run tauri dev # requires a working Rust toolchain
```

Common validation commands:

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
