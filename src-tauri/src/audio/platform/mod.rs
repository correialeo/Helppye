//! Platform-specific system-audio capture providers.
//!
//! None of these are implemented yet — see `docs/meetily-audio-audit.md` section 6/7.
//! The Meetily reference has no working Linux system-audio capture (only a device-name
//! heuristic), a macOS implementation planned for adaptation later (Core Audio Process
//! Tap, tracked in `docs/third-party-components.md`), and a Windows implementation that
//! still needs to be designed around WASAPI loopback. Each stub below returns
//! `AudioCaptureError::Unsupported` honestly rather than pretending to capture anything.

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
