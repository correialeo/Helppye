//! Platform-specific system-audio capture providers.
//!
//! Windows is implemented (WASAPI loopback, see `docs/windows-wasapi-loopback.md`).
//! macOS and Linux are not yet — see `docs/meetily-audio-audit.md` section 6/7. The
//! Meetily reference has no working Linux system-audio capture (only a device-name
//! heuristic) and a macOS implementation planned for adaptation later (Core Audio Process
//! Tap, tracked in `docs/third-party-components.md`). Each unimplemented stub below
//! returns `AudioCaptureError::Unsupported` honestly rather than pretending to capture
//! anything.

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::SystemAudioProvider;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::SystemAudioProvider;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::SystemAudioProvider;
