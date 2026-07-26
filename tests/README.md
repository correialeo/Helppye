Cross-cutting/integration tests that span the frontend and Rust core. Rust unit tests for the audio pipeline live alongside their modules under `src-tauri/src/audio/` (`#[cfg(test)]`), not here.
