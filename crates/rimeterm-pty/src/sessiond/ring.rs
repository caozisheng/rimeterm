//! Fixed-capacity byte ring with snapshot support.
//!
//! The daemon appends every stdout chunk from every hosted child into
//! one ring per session. On attach, the ring is replayed into the
//! client's fresh `Term` replica, giving reattach the full scrollback
//! without a grid serialization protocol. Old bytes fall off the front;
//! a replay that starts mid-escape-sequence is fine because the VTE
//! parser resyncs on the next `ESC [` — worst case one garbled line at
//! the very top of history.
//!
//! No `unsafe`, no sub-second-resolution time bookkeeping. Locking is a
//! single `parking_lot::Mutex` — appends are µs-scale, snapshots rare.

use std::collections::VecDeque;

/// A bounded byte ring. Capacity is fixed at construction.
pub struct ByteRing {
    buf: VecDeque<u8>,
    cap: usize,
}

impl ByteRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity.min(1024 * 1024)),
            cap: capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Append bytes, evicting oldest-first when full.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.cap == 0 || bytes.is_empty() {
            return;
        }
        if bytes.len() >= self.cap {
            // Whole-buffer replace — cheaper than draining element-wise.
            self.buf.clear();
            let tail = &bytes[bytes.len() - self.cap..];
            self.buf.extend(tail);
            return;
        }
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        if overflow > 0 {
            self.buf.drain(..overflow);
        }
        self.buf.extend(bytes);
    }

    /// Total bytes currently retained.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Copy out the retained bytes, oldest → newest.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.buf.len());
        let (a, b) = self.buf.as_slices();
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_last_cap_bytes() {
        let mut ring = ByteRing::new(10);
        ring.push(b"0123456789ABCDE");
        assert_eq!(ring.snapshot(), b"56789ABCDE");
        assert_eq!(ring.len(), 10);
    }

    #[test]
    fn wraps_across_internal_segments() {
        let mut ring = ByteRing::new(8);
        ring.push(b"aaaa");
        ring.push(b"bbbb"); // full: aaaabbbb
        ring.push(b"cc"); // evicts aa → aabbbbcc
        assert_eq!(ring.snapshot(), b"aabbbbcc");
    }

    #[test]
    fn oversized_push_replaces_whole_buffer() {
        let mut ring = ByteRing::new(4);
        ring.push(b"XX");
        ring.push(b"0123456789");
        assert_eq!(ring.snapshot(), b"6789");
    }

    #[test]
    fn zero_capacity_swallows_everything() {
        let mut ring = ByteRing::new(0);
        ring.push(b"data");
        assert!(ring.is_empty());
        assert_eq!(ring.snapshot(), b"");
    }

    #[test]
    fn empty_push_is_noop() {
        let mut ring = ByteRing::new(4);
        ring.push(b"");
        assert!(ring.is_empty());
    }

    #[test]
    fn snapshot_of_high_water_mark() {
        let mut ring = ByteRing::new(16);
        for i in 0..40u8 {
            ring.push(&[i]);
        }
        let snap = ring.snapshot();
        assert_eq!(snap.len(), 16);
        assert_eq!(snap[0], 24);
        assert_eq!(snap[15], 39);
    }
}
