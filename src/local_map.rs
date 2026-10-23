// SPDX-License-Identifier: MIT
//! Local shared-memory mode (plan S1/S2): map the service file the server names, prove we share
//! its machine with a nonce read from a side file, and fall back to WSS ranges on any failure.
//!
//! The nonce lives in a 0600 side file in the tenant directory, never in the service file, and is
//! echoed to the server over the authenticated connection. An IP match alone is too weak.

use crate::message::Secret;
use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Largest nonce side file accepted. Commy nonces are short hex strings.
pub const MAX_NONCE_BYTES: u64 = 256;

/// `Ok` only for a regular file that is not a symlink. Checked before opening so a FIFO or device
/// the server names is never opened.
fn require_regular(path: &Path) -> Result<(), FallbackReason> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => Ok(()),
        Ok(_) => Err(FallbackReason::NotRegularFile(path.display().to_string())),
        Err(e) => Err(FallbackReason::OpenFailed(e.to_string())),
    }
}

/// Why local mode was not entered. The caller stays on WSS ranges.
#[derive(Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// The service file could not be opened read-write (other host, container, proxy, no rights).
    OpenFailed(String),
    /// The service file is empty and cannot be mapped.
    EmptyFile,
    /// The nonce side file could not be read.
    NonceUnreadable(String),
    /// The nonce side file holds nothing.
    NonceEmpty,
    /// The service file or the nonce file is not a regular file (device, directory, FIFO, symlink).
    NotRegularFile(String),
    /// The nonce side file is larger than `MAX_NONCE_BYTES`.
    NonceTooLarge,
    /// The file could not be mapped.
    MapFailed(String),
    /// The offer named a path without a nonce path, or the reverse.
    IncompleteOffer,
}

/// Map the prepared (not yet live) file of a resize for the requester to fill. Same rules as
/// [`LocalMap::open`] (regular file, no symlink), no nonce: the handshake is redone after the commit.
/// The file must be exactly `size` bytes.
pub fn open_fill_target(path: &Path, size: u64) -> Result<MmapMut, FallbackReason> {
    require_regular(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| FallbackReason::OpenFailed(e.to_string()))?;
    match file.metadata() {
        Ok(m) if m.len() == size && size > 0 => {}
        Ok(m) => {
            return Err(FallbackReason::OpenFailed(format!(
                "prepared file is {} bytes, expected {size}",
                m.len()
            )))
        }
        Err(e) => return Err(FallbackReason::OpenFailed(e.to_string())),
    }
    // SAFETY: as in `LocalMap::open`; the server created this file for us.
    unsafe { MmapMut::map_mut(&file) }.map_err(|e| FallbackReason::MapFailed(e.to_string()))
}

/// A service file mapped read-write, with the nonce that proves the mapping is local.
///
/// `view` is what callers see (and what the S3 write trap will protect). `alias` is a second
/// read-write mapping of the same pages that only the SDK uses to apply inbound ranges, so applying
/// them neither trips the trap nor echoes back as a local change (plan S2).
pub struct LocalMap {
    path: PathBuf,
    view: MmapMut,
    alias: MmapMut,
    nonce: Secret<String>,
}

fn read_nonce(path: &Path) -> Result<String, FallbackReason> {
    require_regular(path).map_err(|r| match r {
        FallbackReason::OpenFailed(e) => FallbackReason::NonceUnreadable(e),
        other => other,
    })?;
    let unreadable = |e: std::io::Error| FallbackReason::NonceUnreadable(e.to_string());
    let mut buf = String::new();
    let n = std::fs::File::open(path)
        .map_err(unreadable)?
        .take(MAX_NONCE_BYTES + 1)
        .read_to_string(&mut buf)
        .map_err(unreadable)?;
    if n as u64 > MAX_NONCE_BYTES {
        return Err(FallbackReason::NonceTooLarge);
    }
    Ok(buf)
}

/// What the local-mapping handshake produced for one service.
#[derive(Debug)]
pub struct NegotiatedFile {
    /// The id range frames use for this service.
    pub file_id: u32,
    /// The confirmed local mapping, or `None` to stay on WSS ranges.
    pub local: Option<LocalMap>,
}

/// The result of trying to enter local mode.
pub enum LocalMapOutcome {
    Local(Box<LocalMap>),
    Fallback(FallbackReason),
}

impl LocalMap {
    /// Try to enter local mode for `path`, proving locality with the nonce in `nonce_path`.
    ///
    /// **Trust boundary:** the server chooses `path` and `nonce_path`. They must be regular files
    /// (symlinks, devices, FIFOs and directories are refused), but a server that names a regular
    /// file the user can write can still make us map it read-write. Only connect to servers you trust.
    pub fn open(path: &Path, nonce_path: &Path) -> LocalMapOutcome {
        if let Err(r) = require_regular(path) {
            return LocalMapOutcome::Fallback(r);
        }
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => f,
            Err(e) => return LocalMapOutcome::Fallback(FallbackReason::OpenFailed(e.to_string())),
        };
        match file.metadata() {
            Ok(m) if m.len() == 0 => return LocalMapOutcome::Fallback(FallbackReason::EmptyFile),
            Ok(_) => {}
            Err(e) => return LocalMapOutcome::Fallback(FallbackReason::OpenFailed(e.to_string())),
        }
        let raw = match read_nonce(nonce_path) {
            Ok(s) => s,
            Err(r) => return LocalMapOutcome::Fallback(r),
        };
        let nonce = raw.trim();
        if nonce.is_empty() {
            return LocalMapOutcome::Fallback(FallbackReason::NonceEmpty);
        }
        // SAFETY: the file is opened read-write and kept alive by the mappings. Another process
        // truncating it would fault us; that is the shared-memory contract, not a memory-safety
        // guarantee this crate can add.
        let (view, alias) = unsafe {
            match (MmapMut::map_mut(&file), MmapMut::map_mut(&file)) {
                (Ok(v), Ok(a)) => (v, a),
                (Err(e), _) | (_, Err(e)) => {
                    return LocalMapOutcome::Fallback(FallbackReason::MapFailed(e.to_string()))
                }
            }
        };
        LocalMapOutcome::Local(Box::new(LocalMap {
            path: path.to_path_buf(),
            view,
            alias,
            nonce: Secret::from(nonce),
        }))
    }

    /// Enter local mode from a server offer. The wire type does not enforce that `path` and
    /// `nonce_path` come together, so one without the other is treated as no offer.
    pub fn from_offer(path: Option<&Path>, nonce_path: Option<&Path>) -> LocalMapOutcome {
        match (path, nonce_path) {
            (Some(p), Some(n)) => Self::open(p, n),
            _ => LocalMapOutcome::Fallback(FallbackReason::IncompleteOffer),
        }
    }

    /// The service file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Mapped length in bytes.
    pub fn len(&self) -> usize {
        self.view.len()
    }

    /// True when the mapping is empty (never the case for a successfully opened map).
    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// The caller's view of the file.
    ///
    /// Other processes write this memory while the slice is alive, so the slice is not stable.
    /// Treat it as a doc-level convenience until rotation guards cover the view; do not hold
    /// references across awaits or assume two reads agree.
    pub fn view(&self) -> &[u8] {
        &self.view
    }

    /// Base pointer of the caller's view (the region S3 protects).
    pub fn view_ptr(&mut self) -> *mut u8 {
        self.view.as_mut_ptr()
    }

    /// Base pointer of the SDK-only writable alias (the trap writes inbound ranges through it).
    pub fn alias_ptr(&mut self) -> *mut u8 {
        self.alias.as_mut_ptr()
    }

    /// Write into the caller's view (what the program itself does). False when out of bounds.
    pub fn write_view(&mut self, offset: u64, bytes: &[u8]) -> bool {
        let Some(end) = offset.checked_add(bytes.len() as u64) else {
            return false;
        };
        if end > self.view.len() as u64 {
            return false;
        }
        self.view[offset as usize..end as usize].copy_from_slice(bytes);
        true
    }

    /// Apply an inbound range through the SDK-only alias; the view sees it, the trap does not.
    pub fn apply_inbound(&mut self, offset: u64, bytes: &[u8]) -> bool {
        let Some(end) = offset.checked_add(bytes.len() as u64) else {
            return false;
        };
        if end > self.alias.len() as u64 {
            return false;
        }
        self.alias[offset as usize..end as usize].copy_from_slice(bytes);
        true
    }

    /// The nonce to echo to the server. Redacted in `Debug`.
    pub fn nonce(&self) -> &Secret<String> {
        &self.nonce
    }
}

impl std::fmt::Debug for LocalMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalMap")
            .field("path", &self.path)
            .field("len", &self.view.len())
            .field("nonce", &self.nonce)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn fallback(o: LocalMapOutcome) -> FallbackReason {
        match o {
            LocalMapOutcome::Fallback(r) => r,
            LocalMapOutcome::Local(_) => panic!("expected fallback, got local mode"),
        }
    }

    fn local(o: LocalMapOutcome) -> LocalMap {
        match o {
            LocalMapOutcome::Local(m) => *m,
            LocalMapOutcome::Fallback(r) => panic!("expected local mode, got {r:?}"),
        }
    }

    #[test]
    fn missing_service_file_falls_back() {
        let d = dir();
        let nonce = d.path().join("n");
        std::fs::write(&nonce, "abc").unwrap();
        let r = fallback(LocalMap::open(&d.path().join("absent.bin"), &nonce));
        assert!(matches!(r, FallbackReason::OpenFailed(_)), "{r:?}");
    }

    #[test]
    fn empty_service_file_falls_back() {
        let d = dir();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, b"").unwrap();
        std::fs::write(&n, "abc").unwrap();
        assert_eq!(fallback(LocalMap::open(&f, &n)), FallbackReason::EmptyFile);
    }

    #[test]
    fn unreadable_nonce_falls_back() {
        let d = dir();
        let f = d.path().join("svc.bin");
        std::fs::write(&f, [0u8; 64]).unwrap();
        let r = fallback(LocalMap::open(&f, &d.path().join("no-nonce")));
        assert!(matches!(r, FallbackReason::NonceUnreadable(_)), "{r:?}");
    }

    #[test]
    fn offer_with_path_but_no_nonce_path_falls_back() {
        let d = dir();
        let f = d.path().join("svc.bin");
        std::fs::write(&f, [0u8; 64]).unwrap();
        assert_eq!(
            fallback(LocalMap::from_offer(Some(&f), None)),
            FallbackReason::IncompleteOffer
        );
        assert_eq!(
            fallback(LocalMap::from_offer(None, Some(&f))),
            FallbackReason::IncompleteOffer
        );
    }

    #[test]
    fn directory_as_service_file_is_refused() {
        let d = dir();
        let n = d.path().join("n");
        std::fs::write(&n, "x").unwrap();
        let r = fallback(LocalMap::open(d.path(), &n));
        assert!(matches!(r, FallbackReason::NotRegularFile(_)), "{r:?}");
    }

    #[test]
    fn directory_as_nonce_file_is_refused() {
        let d = dir();
        let f = d.path().join("svc.bin");
        std::fs::write(&f, [0u8; 64]).unwrap();
        let r = fallback(LocalMap::open(&f, d.path()));
        assert!(matches!(r, FallbackReason::NotRegularFile(_)), "{r:?}");
    }

    #[test]
    fn oversize_nonce_file_is_refused() {
        let d = dir();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, [0u8; 64]).unwrap();
        std::fs::write(&n, vec![b'a'; MAX_NONCE_BYTES as usize + 1]).unwrap();
        assert_eq!(
            fallback(LocalMap::open(&f, &n)),
            FallbackReason::NonceTooLarge
        );
        std::fs::write(&n, vec![b'a'; MAX_NONCE_BYTES as usize]).unwrap();
        local(LocalMap::open(&f, &n));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_service_file_is_refused() {
        let d = dir();
        let real = d.path().join("real.bin");
        let link = d.path().join("link.bin");
        let n = d.path().join("n");
        std::fs::write(&real, [0u8; 64]).unwrap();
        std::fs::write(&n, "x").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let r = fallback(LocalMap::open(&link, &n));
        assert!(matches!(r, FallbackReason::NotRegularFile(_)), "{r:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn device_as_nonce_file_is_refused_without_reading() {
        let d = dir();
        let f = d.path().join("svc.bin");
        std::fs::write(&f, [0u8; 64]).unwrap();
        let r = fallback(LocalMap::open(&f, Path::new("/dev/zero")));
        assert!(matches!(r, FallbackReason::NotRegularFile(_)), "{r:?}");
    }

    #[test]
    fn blank_nonce_falls_back() {
        let d = dir();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, [0u8; 64]).unwrap();
        std::fs::write(&n, " \r\n").unwrap();
        assert_eq!(fallback(LocalMap::open(&f, &n)), FallbackReason::NonceEmpty);
    }

    #[test]
    fn good_file_and_nonce_enter_local_mode_with_trimmed_nonce() {
        let d = dir();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, [7u8; 4096]).unwrap();
        std::fs::write(&n, "s3cret-nonce\r\n").unwrap();
        let mut m = local(LocalMap::open(&f, &n));
        assert!(!m.view_ptr().is_null());
        assert_eq!(m.len(), 4096);
        assert_eq!(m.view()[0], 7);
        assert_eq!(m.nonce().expose(), "s3cret-nonce");
    }

    #[test]
    fn nonce_never_appears_in_debug_output() {
        let d = dir();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, [0u8; 64]).unwrap();
        std::fs::write(&n, "s3cret-nonce").unwrap();
        let m = local(LocalMap::open(&f, &n));
        assert!(!format!("{m:?}").contains("s3cret-nonce"));
    }

    #[test]
    fn inbound_range_is_visible_in_view_and_in_file() {
        let d = dir();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, [0u8; 128]).unwrap();
        std::fs::write(&n, "x").unwrap();
        let mut m = local(LocalMap::open(&f, &n));
        assert!(m.apply_inbound(10, &[1, 2, 3]));
        assert_eq!(&m.view()[10..13], &[1, 2, 3]);
        m.alias.flush().unwrap();
        assert_eq!(&std::fs::read(&f).unwrap()[10..13], &[1, 2, 3]);
    }

    #[test]
    fn inbound_range_out_of_bounds_is_refused_and_changes_nothing() {
        let d = dir();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, [0u8; 128]).unwrap();
        std::fs::write(&n, "x").unwrap();
        let mut m = local(LocalMap::open(&f, &n));
        assert!(!m.apply_inbound(126, &[1, 2, 3, 4]));
        assert!(!m.apply_inbound(u64::MAX, &[1]));
        assert!(m.view().iter().all(|b| *b == 0));
    }
}
