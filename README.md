# Helppye

Real-time meeting copilot. Tauri 2 (Rust core) + React/TypeScript frontend, local-first: local transcription, local LLM via Ollama, no cloud dependency.

## Status

Early foundation. Audio capture infrastructure is being built incrementally; see `docs/` for the architecture audit and roadmap. Question detection and the answer overlay are not implemented yet — audio capture is being stabilized first.

## Stack

- Tauri 2, stable Rust, Tokio
- React 18, TypeScript (strict), Vite, Tailwind CSS, Zustand
- Ollama (local LLM), SQLite (config/history only — not implemented yet)
- `tracing` for structured logging

## Layout

- `src/` — React/TypeScript frontend
- `src-tauri/` — Rust core (Tauri commands, audio pipeline)
- `docs/` — architecture audit, design notes, roadmap
- `prompts/` — LLM prompt templates (added starting in the Ollama integration phase)
- `tests/` — cross-cutting/integration tests; Rust unit tests live alongside their modules under `src-tauri/src`

## Relationship to Meetily

`meetily/` (sibling directory, not part of this package) is a reference-only clone of Zackriya Solutions' Meetily project (MIT licensed), used exclusively for architecture research. See `docs/meetily-audio-audit.md` for the full audit and `docs/third-party-components.md` for anything adapted from it (attribution tracked there and in `NOTICE`).

## Development

Rust toolchain (`cargo`/`rustc`) was not available in the environment this scaffold was created in, so the Tauri/Rust side has not been compiled or run yet — treat it as unverified until built locally.

```bash
npm install
npm run tauri dev   # requires a working Rust toolchain
```
