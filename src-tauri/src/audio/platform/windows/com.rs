//! RAII guard for COM apartment initialization on the current thread.

use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::audio::error::AudioCaptureError;

/// Initializes a multithreaded-apartment (MTA) COM apartment on the current thread for
/// the lifetime of this guard, and uninitializes it on drop.
///
/// MTA (rather than STA) is used because capture runs its own polling loop on a dedicated
/// thread with no Win32 message pump — STA requires one to dispatch COM calls made from
/// other threads/apartments, which this design never needs.
///
/// `CoInitializeEx` is reference-counted per thread, so nesting (e.g. a blocking device
/// enumeration call on a thread that also does capture) is safe as long as every
/// `ComGuard` created on a thread is dropped before the thread is reused for a different
/// concurrency model — this crate never mixes apartment types on one thread, so that's
/// always true here.
pub struct ComGuard {
    /// Opts the type out of `Send`/`Sync`: a COM apartment is bound to the thread that
    /// initialized it, so this guard must never cross threads.
    _not_send: std::marker::PhantomData<*mut ()>,
}

impl ComGuard {
    pub fn new() -> Result<Self, AudioCaptureError> {
        // SAFETY: CoInitializeEx has no preconditions beyond being called on the thread
        // that should own the apartment; `pvreserved` must be null (per the Win32 contract),
        // which the `None` argument satisfies. The returned HRESULT is checked below rather
        // than treated as an infallible call, since RPC_E_CHANGED_MODE (this thread already
        // has an incompatible apartment) is a real failure mode we must surface, not panic on.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK and S_FALSE (already initialized, compatible mode) are both success.
        if hr.is_err() {
            return Err(AudioCaptureError::Internal(format!(
                "CoInitializeEx failed: {hr:?}"
            )));
        }
        Ok(ComGuard {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: matches the successful CoInitializeEx call in `new`; must run on the same
        // thread, which holds trivially since `ComGuard` is not `Send`.
        unsafe { CoUninitialize() };
    }
}
