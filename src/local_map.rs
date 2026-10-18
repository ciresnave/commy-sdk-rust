//! Local shared-memory mode (plan S1/S2): map the service file the server names, prove we share
//! its machine with a nonce read from a side file, and fall back to WSS ranges on any failure.
//!
//! The nonce lives in a 0600 side file in the tenant directory, never in the service file, and is
//! echoed to the server over the authenticated connection. An IP match alone is too weak.

use crate::message::Secret;
use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

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
    /// The file could not be mapped.
    MapFailed(String),
    /// The offer named a path without a nonce path, or the reverse.
    IncompleteOffer,
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

/// The result of trying to enter local mode.
pub enum LocalMapOutcome {
    Local(Box<LocalMap>),
    Fallback(FallbackReason),
}

impl LocalMap {
    /// Try to enter local mode for `path`, proving locality with the nonce in `nonce_path`.
    pub fn open(path: &Path, nonce_path: &Path) -> LocalMapOutcome {
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => f,
            Err(e) => return LocalMapOutcome::Fallback(FallbackReason::OpenFailed(e.to_string())),
        };
        match file.metadata() {
            Ok(m) if m.len() == 0 => return LocalMapOutcome::Fallback(FallbackReason::EmptyFile),
            Ok(_) => {}
            Err(e) => return LocalMapOutcome::Fallback(FallbackReason::OpenFailed(e.to_string())),
        }
        let raw = match std::fs::read_to_string(nonce_path) {
            Ok(s) => s,
            Err(e) => {
                return LocalMapOutcome::Fallback(FallbackReason::NonceUnreadable(e.to_string()))
            }
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
    pub fn view(&self) -> &[u8] {
        &self.view
    }

    /// Base pointer of the caller's view (the region S3 protects).
    pub fn view_ptr(&self) -> *mut u8 {
        self.view.as_ptr() as *mut u8
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
        let m = local(LocalMap::open(&f, &n));
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
