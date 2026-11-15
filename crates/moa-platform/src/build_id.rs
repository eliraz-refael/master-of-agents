//! The build id: names one installed binary, so a daemon can tell it was
//! started from an older build than the client talking to it.

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

/// `fs::canonicalize`, or `p` unchanged when that fails.
pub fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// FNV-1a, 64 bit.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// FNV-1a-64 over `canonical path, 0x00, length (u64 LE), mtime ns (u128 LE)`,
/// as 16 lowercase hex digits.
pub fn build_id_of(exe: &Path) -> io::Result<String> {
    let meta = std::fs::metadata(exe)?;
    let mtime = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let mut bytes = canonical(exe).as_os_str().as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(&meta.len().to_le_bytes());
    bytes.extend_from_slice(&mtime.to_le_bytes());
    Ok(format!("{:016x}", fnv1a64(&bytes)))
}

/// The build id of the running executable, computed once; `"unknown"` if it can't be.
/// Call it at startup: it hashes the file at the executable's path, so a first call made
/// after an upgrade replaced that file would name the new build, not the running one.
pub fn build_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        std::env::current_exe()
            .and_then(|p| build_id_of(&p))
            .unwrap_or_else(|_| "unknown".to_string())
    })
}
