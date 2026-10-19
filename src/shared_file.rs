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
use dirty_ranges::Scanner;

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
}

/// A local mapping plus the baseline that makes its changes visible.
pub struct SharedFile {
    map: LocalMap,
    scanner: Scanner,
    file_id: u32,
    seq: u64,
    last_applied: u64,
}

impl SharedFile {
    /// Track `map` for `file_id`; its current bytes are the baseline.
    pub fn new(map: LocalMap, file_id: u32) -> Self {
        let scanner = Scanner::new(map.view());
        Self {
            map,
            scanner,
            file_id,
            seq: 0,
            last_applied: 0,
        }
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
        let changed = self
            .scanner
            .scan(self.map.view())
            .expect("scanner and mapping have the same length");
        let view = self.map.view();
        changed
            .iter()
            .flat_map(|r| {
                RangeFrame::split(
                    self.file_id,
                    self.seq,
                    r.offset as u64,
                    &view[r.offset..r.offset + r.len],
                )
            })
            .collect()
    }

    /// Local-mode commit: diff against the baseline and return the changed ranges, in messages of
    /// at most `MAX_RANGES_PER_MESSAGE` ranges. No data travels; the server reads the shared file.
    pub fn commit_local(&mut self) -> Vec<Vec<ByteRange>> {
        let changed = self
            .scanner
            .scan(self.map.view())
            .expect("scanner and mapping have the same length");
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
            let live = &self.map.view()[r.offset as usize..end as usize];
            if self.scanner.apply_remote(r.offset as usize, live).is_ok() {
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
        let len = self.scanner.len() as u64;
        let in_bounds = frame
            .offset
            .checked_add(frame.data.len() as u64)
            .is_some_and(|end| end <= len);
        if !in_bounds
            || !self.map.apply_inbound(frame.offset, &frame.data)
            || self
                .scanner
                .apply_remote(frame.offset as usize, &frame.data)
                .is_err()
        {
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
}
