//! Scan-mode change tracking over a local mapping (plan S6/S7).
//!
//! `SharedFile` owns a [`LocalMap`] and a [`dirty_ranges::Scanner`] baseline. The program writes
//! through the mapping (or `write_range`) and calls [`SharedFile::commit`], which diffs against the
//! baseline and returns the changed bytes as range frames. Inbound frames are applied through the
//! SDK-only alias and the baseline in one step, so they never come back as local changes.
//!
//! Scan mode is the safe default; the write trap (S3) will replace the diff, not this API.

use crate::local_map::LocalMap;
use commy_wire::{ByteRange, RangeFrame, RangesUpdated, MAX_RANGES_PER_MESSAGE};
use dirty_ranges::{Mode, Range, Region, Scanner};

/// Why an inbound frame was not applied.
#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// The frame names another file.
    WrongFile { expected: u32, got: u32 },
    /// The frame reaches outside the mapping.
    OutOfBounds,
}

/// What happened to an inbound frame that was accepted for processing.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Applied,
    /// Its `seq` is lower than the last applied for this file.
    DroppedStale,
}

/// A write that was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteError {
    OutOfBounds,
    /// A rotation drain barrier is open; writes resume on the new file.
    Draining,
}

/// Where a file's bytes live: a mapping shared with the server (local mode) or a private copy
/// kept in sync by range frames (remote mode).
enum Backing {
    Mapped(LocalMap),
    Heap(Vec<u8>),
}

impl Backing {
    fn view(&self) -> &[u8] {
        match self {
            Backing::Mapped(m) => m.view(),
            Backing::Heap(v) => v,
        }
    }

    fn len(&self) -> usize {
        self.view().len()
    }

    fn write_view(&mut self, offset: u64, bytes: &[u8]) -> bool {
        match self {
            Backing::Mapped(m) => m.write_view(offset, bytes),
            Backing::Heap(v) => write_into(v, offset, bytes),
        }
    }

    fn apply_inbound(&mut self, offset: u64, bytes: &[u8]) -> bool {
        match self {
            Backing::Mapped(m) => m.apply_inbound(offset, bytes),
            Backing::Heap(v) => write_into(v, offset, bytes),
        }
    }
}

fn write_into(buf: &mut [u8], offset: u64, bytes: &[u8]) -> bool {
    let Some(end) = offset.checked_add(bytes.len() as u64) else {
        return false;
    };
    if end > buf.len() as u64 {
        return false;
    }
    buf[offset as usize..end as usize].copy_from_slice(bytes);
    true
}

/// Why the write trap could not be armed.
#[derive(Debug)]
pub enum TrapError {
    /// Only a file mapped from the server can be trapped.
    NotLocal,
    /// The trap is already armed.
    AlreadyArmed,
    /// The mapping is smaller than one OS page.
    TooSmall,
    /// The trap is not armed.
    NotArmed,
    /// dirty-ranges refused (alignment, registry full, OS error).
    Region(dirty_ranges::Error),
}

/// The write trap over the whole pages of a mapped file.
struct Trap {
    region: Region,
    /// Bytes the region covers: the mapping rounded down to whole OS pages.
    region_len: usize,
}

/// A file's bytes plus the baseline that makes its changes visible.
pub struct SharedFile {
    // Declared before `map` so the trap is released (pages unprotected) before the mapping goes.
    trap: Option<Trap>,
    map: Backing,
    /// Baseline for the bytes the trap does not cover (all of them when there is no trap).
    scanner: Scanner,
    /// Offset of the first byte `scanner` tracks.
    scan_base: usize,
    file_id: u32,
    seq: u64,
    last_applied: u64,
    draining: bool,
}

impl SharedFile {
    /// Track `map` for `file_id`; its current bytes are the baseline.
    pub fn new(map: LocalMap, file_id: u32) -> Self {
        let scanner = Scanner::new(map.view());
        Self {
            trap: None,
            map: Backing::Mapped(map),
            scanner,
            scan_base: 0,
            file_id,
            seq: 0,
            last_applied: 0,
            draining: false,
        }
    }

    /// Remote mode: track a private copy of the file, as sent by a `Resync`. `seq` is the
    /// sequence the snapshot is current at.
    pub fn from_snapshot(bytes: Vec<u8>, file_id: u32, seq: u64) -> Self {
        let scanner = Scanner::new(&bytes);
        Self {
            trap: None,
            map: Backing::Heap(bytes),
            scanner,
            scan_base: 0,
            file_id,
            seq,
            last_applied: seq,
            draining: false,
        }
    }

    /// Turn on the write trap: the whole pages of the mapping become read-only and the first write to
    /// each page marks it dirty, so `commit` no longer reads every page. At most `pool_pages / 2` pages
    /// are trapped (two baseline buffers each); the rest, and any partial last page, stay on the scan.
    ///
    /// # Safety
    /// While armed: no thread may block `SIGSEGV`/`SIGBUS` while writing the mapping (Unix), and nothing
    /// may read from the OS into the mapping (`read`, `recv`), which would fail with `EFAULT`. Writes
    /// through [`SharedFile::write_range`] and [`SharedFile::as_mut_ptr`] are fine.
    pub unsafe fn arm_trap(&mut self, pool_pages: usize) -> Result<(), TrapError> {
        if self.trap.is_some() {
            return Err(TrapError::AlreadyArmed);
        }
        let Backing::Mapped(m) = &mut self.map else {
            return Err(TrapError::NotLocal);
        };
        let region_len = m.len() / page_size::get() * page_size::get();
        if region_len == 0 {
            return Err(TrapError::TooSmall);
        }
        let (base, alias) = (m.view_ptr(), m.alias_ptr());
        // SAFETY: both pointers map the same file for `m.len() >= region_len` bytes, and `trap` drops
        // before `map` (field order). The caller upholds the signal and OS-read contract.
        let region = unsafe { Region::arm(base, region_len, pool_pages, Some(alias)) }
            .map_err(TrapError::Region)?;
        let base_off = region.armed_pages() * region.page_size();
        // Trap is the only owner of the pages below `base_off` now; the scan keeps the rest.
        self.scanner = Scanner::new(&self.map.view()[base_off..]);
        self.scan_base = base_off;
        self.trap = Some(Trap { region, region_len });
        Ok(())
    }

    /// Switch whole pages (`range` in bytes, page aligned, inside the trapped pages) between trapped
    /// writes and scanning. Scanned pages cost a read per commit but never fault. Nothing is lost.
    pub fn set_page_mode(
        &mut self,
        range: std::ops::Range<usize>,
        mode: Mode,
    ) -> Result<(), TrapError> {
        let t = self.trap.as_mut().ok_or(TrapError::NotArmed)?;
        t.region.set_mode(range, mode).map_err(TrapError::Region)
    }

    /// Base pointer of the file's bytes for writing in place; `None` unless the file is mapped.
    pub fn as_mut_ptr(&mut self) -> Option<*mut u8> {
        match &mut self.map {
            Backing::Mapped(m) => Some(m.view_ptr()),
            Backing::Heap(_) => None,
        }
    }

    /// Everything written since the last call: the trapped pages' dirty ranges plus the scan of the rest.
    fn changed(&mut self) -> Vec<Range> {
        let mut out = match self.trap.as_mut() {
            Some(t) => t.region.flush(),
            None => Vec::new(),
        };
        let base = self.scan_base;
        let tail = self
            .scanner
            .scan(&self.map.view()[base..])
            .expect("scanner and mapping have the same length");
        out.extend(tail.into_iter().map(|r| Range {
            offset: r.offset + base,
            len: r.len,
        }));
        out
    }

    /// True when the bytes are a mapping shared with the server (changes travel as ranges only).
    pub fn is_local(&self) -> bool {
        matches!(self.map, Backing::Mapped(_))
    }

    /// Open or close the drain barrier: while open, `write_range` is refused.
    pub fn set_draining(&mut self, draining: bool) {
        self.draining = draining;
    }

    pub fn is_draining(&self) -> bool {
        self.draining
    }

    /// The highest sequence this file has sent or seen.
    pub fn last_seq(&self) -> u64 {
        self.seq.max(self.last_applied)
    }

    /// Unflushed local writes as `(offset, bytes)`, found by diffing against the baseline (which
    /// advances). Used to carry writes into the new file after a revocation.
    pub fn take_pending_writes(&mut self) -> Vec<(u64, Vec<u8>)> {
        let changed = self.changed();
        let view = self.map.view();
        changed
            .iter()
            .map(|r| (r.offset as u64, view[r.offset..r.offset + r.len].to_vec()))
            .collect()
    }

    pub fn file_id(&self) -> u32 {
        self.file_id
    }

    /// The sequence number the next committed frames will carry.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Adopt the server's sequence (from `FlushAck` or `ResyncDone`).
    pub fn set_seq(&mut self, seq: u64) {
        self.seq = seq;
    }

    pub fn view(&self) -> &[u8] {
        self.map.view()
    }

    /// Copy `bytes` into the mapping at `offset`. Nothing is sent until [`SharedFile::commit`].
    pub fn write_range(&mut self, offset: u64, bytes: &[u8]) -> Result<(), WriteError> {
        if self.draining {
            return Err(WriteError::Draining);
        }
        if self.map.write_view(offset, bytes) {
            Ok(())
        } else {
            Err(WriteError::OutOfBounds)
        }
    }

    /// Diff the mapping against the baseline and return the changes as frames (at most 1 MiB each,
    /// all sharing the current `seq`). Empty when nothing changed. The baseline advances.
    pub fn commit(&mut self) -> Vec<RangeFrame> {
        // The scanner was created for this mapping's length, so a length mismatch is impossible.
        let changed = self.changed();
        let view = self.map.view();
        changed
            .iter()
            .flat_map(|r| {
                RangeFrame::split(
                    self.file_id,
                    self.seq.max(self.last_applied),
                    r.offset as u64,
                    &view[r.offset..r.offset + r.len],
                )
            })
            .collect()
    }

    /// Local-mode commit: diff against the baseline and return the changed ranges, in messages of
    /// at most `MAX_RANGES_PER_MESSAGE` ranges. No data travels; the server reads the shared file.
    pub fn commit_local(&mut self) -> Vec<Vec<ByteRange>> {
        let changed = self.changed();
        changed
            .chunks(MAX_RANGES_PER_MESSAGE)
            .map(|c| {
                c.iter()
                    .map(|r| ByteRange {
                        offset: r.offset as u64,
                        len: r.len as u64,
                    })
                    .collect()
            })
            .collect()
    }

    /// Another process changed these ranges of the shared file: take the live bytes as the new
    /// baseline so they are not reported as local changes. An unflushed local write overlapping a
    /// range is absorbed (the last writer to the mapping wins). Ranges outside the file are
    /// ignored. Returns how many ranges were re-baselined.
    pub fn on_ranges_updated(&mut self, u: &RangesUpdated) -> usize {
        if u.file_id != self.file_id {
            return 0;
        }
        self.last_applied = self.last_applied.max(u.seq);
        let mut n = 0;
        for r in &u.ranges {
            let Some(end) = r.offset.checked_add(r.len) else {
                continue;
            };
            if end > self.map.len() as u64 {
                continue;
            }
            let (start, end) = (r.offset as usize, end as usize);
            // Trapped pages: baseline from the live bytes without writing (a write-back could
            // overwrite a newer foreign write).
            let region_len = self.trap.as_ref().map_or(0, |t| t.region_len);
            let mut ok = true;
            if let (Some(t), true) = (self.trap.as_mut(), start < region_len) {
                ok &= t
                    .region
                    .rebaseline(start, end.min(region_len) - start)
                    .is_ok();
            }
            let from = start.max(self.scan_base);
            if from < end {
                let live = &self.map.view()[from..end];
                ok &= self
                    .scanner
                    .apply_remote(from - self.scan_base, live)
                    .is_ok();
            }
            if ok {
                n += 1;
            }
        }
        n
    }

    /// The server's answer to a flush. A lower `seq` than already seen is legal; keep the max.
    pub fn on_flush_ack(&mut self, seq: u64) {
        self.last_applied = self.last_applied.max(seq);
        self.seq = self.seq.max(seq);
    }

    /// Simulate another process writing the shared file (through the SDK-only alias).
    #[cfg(test)]
    pub(crate) fn foreign_write_for_test(&mut self, offset: u64, bytes: &[u8]) -> bool {
        self.map.apply_inbound(offset, bytes)
    }

    /// Write `bytes` through the alias and into whichever baseline covers each part. Bounds checked.
    fn apply_bytes(&mut self, offset: usize, bytes: &[u8]) -> bool {
        let region_len = self.trap.as_ref().map_or(0, |t| t.region_len);
        let split = region_len.saturating_sub(offset).min(bytes.len());
        if let (Some(t), true) = (self.trap.as_mut(), split > 0) {
            if t.region.apply_remote(offset, &bytes[..split]).is_err() {
                return false;
            }
        }
        let rest_off = offset + split;
        if split < bytes.len() && !self.map.apply_inbound(rest_off as u64, &bytes[split..]) {
            return false;
        }
        // Baseline the part the scanner tracks (it starts at `scan_base`, never past `region_len`).
        let from = offset.max(self.scan_base);
        let end = offset + bytes.len();
        from >= end
            || self
                .scanner
                .apply_remote(from - self.scan_base, &bytes[from - offset..])
                .is_ok()
    }

    /// Apply an inbound frame to the mapping and the baseline.
    pub fn apply_inbound(&mut self, frame: &RangeFrame) -> Result<Applied, ApplyError> {
        if frame.file_id != self.file_id {
            return Err(ApplyError::WrongFile {
                expected: self.file_id,
                got: frame.file_id,
            });
        }
        if frame.seq < self.last_applied {
            return Ok(Applied::DroppedStale);
        }
        let len = self.map.len() as u64;
        let in_bounds = frame
            .offset
            .checked_add(frame.data.len() as u64)
            .is_some_and(|end| end <= len);
        if !in_bounds || !self.apply_bytes(frame.offset as usize, &frame.data) {
            return Err(ApplyError::OutOfBounds);
        }
        self.last_applied = frame.seq;
        Ok(Applied::Applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_map::{LocalMap, LocalMapOutcome};

    fn shared(size: usize) -> (tempfile::TempDir, SharedFile) {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("svc.bin");
        let n = d.path().join("n");
        std::fs::write(&f, vec![0u8; size]).unwrap();
        std::fs::write(&n, "x").unwrap();
        let LocalMapOutcome::Local(m) = LocalMap::open(&f, &n) else {
            panic!("local mode expected");
        };
        (d, SharedFile::new(*m, 5))
    }

    fn frame(file_id: u32, seq: u64, offset: u64, data: &[u8]) -> RangeFrame {
        RangeFrame {
            file_id,
            seq,
            offset,
            data: data.to_vec(),
        }
    }

    #[test]
    fn untouched_mapping_commits_nothing() {
        let (_d, mut s) = shared(8192);
        assert!(s.commit().is_empty());
    }

    #[test]
    fn write_range_then_commit_yields_exact_frame_and_then_nothing() {
        let (_d, mut s) = shared(8192);
        s.set_seq(41);
        s.write_range(100, &[1, 2, 3]).unwrap();
        let frames = s.commit();
        assert_eq!(frames, vec![frame(5, 41, 100, &[1, 2, 3])]);
        assert!(s.commit().is_empty(), "baseline must advance");
    }

    #[test]
    fn out_of_bounds_write_is_refused_and_changes_nothing() {
        let (_d, mut s) = shared(4096);
        assert_eq!(s.write_range(4095, &[1, 2]), Err(WriteError::OutOfBounds));
        assert_eq!(s.write_range(u64::MAX, &[1]), Err(WriteError::OutOfBounds));
        assert!(s.view().iter().all(|b| *b == 0));
        assert!(s.commit().is_empty());
    }

    #[test]
    fn a_change_larger_than_one_mib_splits_into_frames_sharing_one_seq() {
        let size = 3 * 1024 * 1024;
        let (_d, mut s) = shared(size);
        s.set_seq(7);
        s.write_range(0, &vec![9u8; size]).unwrap();
        let frames = s.commit();
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f.seq == 7 && f.file_id == 5));
        assert!(frames
            .iter()
            .all(|f| f.data.len() <= commy_wire::MAX_PAYLOAD));
        let total: usize = frames.iter().map(|f| f.data.len()).sum();
        assert_eq!(total, size);
        assert_eq!(frames[1].offset, frames[0].data.len() as u64);
    }

    #[test]
    fn inbound_frame_lands_in_view_and_is_not_reported_as_a_local_change() {
        let (_d, mut s) = shared(8192);
        assert_eq!(
            s.apply_inbound(&frame(5, 1, 10, &[4, 5, 6])),
            Ok(Applied::Applied)
        );
        assert_eq!(&s.view()[10..13], &[4, 5, 6]);
        assert!(s.commit().is_empty(), "inbound bytes must not echo back");
    }

    #[test]
    fn local_write_next_to_an_inbound_one_is_still_reported() {
        let (_d, mut s) = shared(8192);
        s.write_range(0, &[1]).unwrap();
        s.apply_inbound(&frame(5, 1, 100, &[2])).unwrap();
        let frames = s.commit();
        assert_eq!(frames.len(), 1);
        assert_eq!(
            (frames[0].offset, frames[0].data.as_slice()),
            (0, &[1u8][..])
        );
    }

    #[test]
    fn commit_local_reports_ranges_without_data_and_advances_baseline() {
        let (_d, mut s) = shared(8192);
        s.write_range(10, &[1, 2]).unwrap();
        s.write_range(5000, &[3]).unwrap();
        let msgs = s.commit_local();
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0],
            vec![
                ByteRange { offset: 10, len: 2 },
                ByteRange {
                    offset: 5000,
                    len: 1
                }
            ]
        );
        assert!(s.commit_local().is_empty());
    }

    #[test]
    fn commit_local_splits_at_the_range_cap() {
        let (_d, mut s) = shared(8192);
        // 1025 isolated single-byte changes: every other byte.
        for i in 0..=MAX_RANGES_PER_MESSAGE as u64 {
            s.write_range(i * 2, &[1]).unwrap();
        }
        let msgs = s.commit_local();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].len(), MAX_RANGES_PER_MESSAGE);
        assert_eq!(msgs[1].len(), 1);
        for m in &msgs {
            commy_wire::validate_ranges(m).unwrap();
        }
    }

    #[test]
    fn ranges_updated_rebaselines_so_foreign_writes_do_not_echo() {
        let (_d, mut s) = shared(8192);
        // Another process writes the shared file: simulate through the SDK-only alias.
        assert!(s.map.apply_inbound(100, &[7, 7, 7]));
        let n = s.on_ranges_updated(&RangesUpdated {
            file_id: 5,
            seq: 3,
            ranges: vec![ByteRange {
                offset: 100,
                len: 3,
            }],
        });
        assert_eq!(n, 1);
        assert!(s.commit_local().is_empty(), "foreign write echoed back");
    }

    #[test]
    fn foreign_write_without_ranges_updated_would_echo() {
        // Control for the test above: without the re-baseline the same write is reported.
        let (_d, mut s) = shared(8192);
        assert!(s.map.apply_inbound(100, &[7, 7, 7]));
        assert_eq!(s.commit_local().len(), 1);
    }

    #[test]
    fn ranges_updated_for_another_file_or_outside_the_file_is_ignored() {
        let (_d, mut s) = shared(4096);
        assert!(s.map.apply_inbound(0, &[9]));
        let other = RangesUpdated {
            file_id: 6,
            seq: 1,
            ranges: vec![ByteRange { offset: 0, len: 1 }],
        };
        assert_eq!(s.on_ranges_updated(&other), 0);
        let outside = RangesUpdated {
            file_id: 5,
            seq: 1,
            ranges: vec![
                ByteRange {
                    offset: 4095,
                    len: 2,
                },
                ByteRange {
                    offset: u64::MAX,
                    len: 2,
                },
            ],
        };
        assert_eq!(s.on_ranges_updated(&outside), 0);
        assert_eq!(
            s.commit_local().len(),
            1,
            "byte 0 is still a pending change"
        );
    }

    #[test]
    fn flush_ack_keeps_the_maximum_seq() {
        let (_d, mut s) = shared(4096);
        s.on_flush_ack(10);
        s.on_flush_ack(4);
        assert_eq!(s.seq(), 10);
    }

    #[test]
    fn stale_seq_is_dropped_equal_seq_applies() {
        let (_d, mut s) = shared(8192);
        assert_eq!(s.apply_inbound(&frame(5, 5, 0, &[1])), Ok(Applied::Applied));
        assert_eq!(
            s.apply_inbound(&frame(5, 4, 1, &[2])),
            Ok(Applied::DroppedStale)
        );
        assert_eq!(s.view()[1], 0, "stale frame must not be written");
        assert_eq!(s.apply_inbound(&frame(5, 5, 2, &[3])), Ok(Applied::Applied));
        assert_eq!(s.view()[2], 3);
    }

    #[test]
    fn frame_for_another_file_or_out_of_bounds_is_refused_untouched() {
        let (_d, mut s) = shared(4096);
        assert_eq!(
            s.apply_inbound(&frame(6, 1, 0, &[1])),
            Err(ApplyError::WrongFile {
                expected: 5,
                got: 6
            })
        );
        assert_eq!(
            s.apply_inbound(&frame(5, 1, 4095, &[1, 2])),
            Err(ApplyError::OutOfBounds)
        );
        assert!(s.view().iter().all(|b| *b == 0));
        assert!(s.commit().is_empty());
    }

    fn armed(size: usize, pool_pages: usize) -> (tempfile::TempDir, SharedFile) {
        let (d, mut s) = shared(size);
        // SAFETY: the tests write only through `write_range` / `as_mut_ptr` on test threads.
        unsafe { s.arm_trap(pool_pages) }.unwrap();
        (d, s)
    }

    fn page() -> usize {
        page_size::get()
    }

    #[test]
    fn trapped_write_commits_exact_frame_then_nothing() {
        let (_d, mut s) = armed(3 * page() + 100, 64);
        s.set_seq(9);
        s.write_range(10, &[1, 2, 3]).unwrap();
        assert_eq!(s.commit(), vec![frame(5, 9, 10, &[1, 2, 3])]);
        assert!(s.commit().is_empty(), "baseline must advance");
    }

    #[test]
    fn in_place_write_through_the_pointer_is_found() {
        let (_d, mut s) = armed(2 * page(), 64);
        let p = s.as_mut_ptr().unwrap();
        // SAFETY: inside the mapping; the fault handler unprotects the page and the write resumes.
        unsafe { p.add(page() + 7).write(0xEE) };
        let m = s.commit_local();
        assert_eq!(
            m,
            vec![vec![ByteRange {
                offset: page() as u64 + 7,
                len: 1
            }]]
        );
    }

    #[test]
    fn partial_last_page_and_untracked_pages_are_still_scanned() {
        // pool of 2 pages tracks one page; page 1 and the 100-byte tail stay on the scan.
        let (_d, mut s) = armed(2 * page() + 100, 2);
        let tail = 2 * page() as u64 + 50;
        s.write_range(3, &[1]).unwrap();
        s.write_range(page() as u64 + 3, &[2]).unwrap();
        s.write_range(tail, &[3]).unwrap();
        let m = s.commit_local();
        assert_eq!(
            m,
            vec![vec![
                ByteRange { offset: 3, len: 1 },
                ByteRange {
                    offset: page() as u64 + 3,
                    len: 1
                },
                ByteRange {
                    offset: tail,
                    len: 1
                },
            ]]
        );
    }

    #[test]
    fn inbound_frames_never_echo_in_trapped_scanned_and_boundary_spans() {
        let (_d, mut s) = armed(2 * page() + 100, 64);
        let edge = 2 * page() - 2; // spans the last trapped page into the partial tail page
        s.apply_inbound(&frame(5, 1, 5, &[7; 3])).unwrap();
        s.apply_inbound(&frame(5, 2, edge as u64, &[8; 4])).unwrap();
        assert_eq!(&s.view()[edge..edge + 4], &[8; 4]);
        assert!(s.commit().is_empty(), "inbound bytes must not echo back");
    }

    #[test]
    fn local_write_beside_an_inbound_one_is_still_reported_when_trapped() {
        let (_d, mut s) = armed(2 * page(), 64);
        s.write_range(0, &[1]).unwrap();
        s.apply_inbound(&frame(5, 1, 100, &[2])).unwrap();
        let f = s.commit();
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].offset, f[0].data.as_slice()), (0, &[1u8][..]));
    }

    #[test]
    fn switching_a_page_to_scanned_and_back_loses_nothing() {
        let (_d, mut s) = armed(2 * page(), 64);
        s.set_page_mode(0..page(), Mode::Scanned).unwrap();
        s.write_range(1, &[1]).unwrap();
        s.set_page_mode(0..page(), Mode::Trapped).unwrap();
        s.write_range(page() as u64 + 1, &[2]).unwrap();
        assert_eq!(s.commit_local()[0].len(), 2);
    }

    #[test]
    fn arm_twice_unmapped_and_tiny_are_refused() {
        let (_d, mut s) = armed(page(), 64);
        // SAFETY: refused before anything is protected.
        assert!(matches!(
            unsafe { s.arm_trap(64) },
            Err(TrapError::AlreadyArmed)
        ));
        let mut h = SharedFile::from_snapshot(vec![0; page()], 5, 0);
        assert!(matches!(
            unsafe { h.arm_trap(64) },
            Err(TrapError::NotLocal)
        ));
        assert!(h.as_mut_ptr().is_none());
        assert!(matches!(
            h.set_page_mode(0..page(), Mode::Scanned),
            Err(TrapError::NotArmed)
        ));
        let (_d2, mut tiny) = shared(100);
        assert!(matches!(
            unsafe { tiny.arm_trap(64) },
            Err(TrapError::TooSmall)
        ));
    }

    #[test]
    fn dropping_an_armed_file_unprotects_the_pages() {
        let (d, s) = armed(page(), 64);
        drop(s);
        // Reopen the same file read-write: a still-protected mapping would be a different process view,
        // so check the process-wide effect instead: arming again must succeed (slot released).
        let n = d.path().join("n");
        let f = d.path().join("svc.bin");
        let crate::local_map::LocalMapOutcome::Local(m) = LocalMap::open(&f, &n) else {
            panic!("local");
        };
        let mut again = SharedFile::new(*m, 5);
        // SAFETY: as in `armed`.
        unsafe { again.arm_trap(64) }.unwrap();
    }

    #[test]
    fn ranges_updated_rebaselines_trapped_pages_without_writing() {
        let (_d, mut s) = armed(2 * page(), 64);
        s.write_range(0, &[1]).unwrap(); // dirties the page, capturing its baseline
        assert!(s.foreign_write_for_test(100, &[7, 7, 7]));
        let upd = RangesUpdated {
            file_id: 5,
            seq: 3,
            ranges: vec![ByteRange {
                offset: 100,
                len: 3,
            }],
        };
        assert_eq!(s.on_ranges_updated(&upd), 1);
        assert_eq!(&s.view()[100..103], &[7, 7, 7], "live bytes untouched");
        assert_eq!(
            s.commit_local(),
            vec![vec![ByteRange { offset: 0, len: 1 }]],
            "only the local write is reported"
        );
    }

    #[test]
    fn foreign_write_on_a_dirty_trapped_page_echoes_without_ranges_updated() {
        // Control for the test above.
        let (_d, mut s) = armed(2 * page(), 64);
        s.write_range(0, &[1]).unwrap();
        assert!(s.foreign_write_for_test(100, &[7, 7, 7]));
        assert_eq!(s.commit_local()[0].len(), 2);
    }
}
