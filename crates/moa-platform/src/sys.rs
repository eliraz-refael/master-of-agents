//! The only module in moa that may use `unsafe`: thin, safe wrappers over libc.

/// The calling process's real user id.
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no arguments, touches no memory and cannot fail.
    unsafe { libc::getuid() }
}
