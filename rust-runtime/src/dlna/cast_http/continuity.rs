//! Per-reader TS counters and adaptation-only discontinuity markers. The
//! algorithm follows Kirakara-Show's MpegTsContinuityRewriter (MIT, FMPeach);
//! see NOTICE.md in this module. PCR/PTS/DTS and all payload bytes are preserved.

pub(super) const PACKET: usize = 188;

pub(super) fn valid(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len().is_multiple_of(PACKET)
        && bytes.chunks_exact(PACKET).all(|p| {
            p[0] == 0x47
                && p[1] & 0x80 == 0
                && p[3] & 0xc0 == 0
                && match (p[3] >> 4) & 3 {
                    1 => true,
                    2 => p[4] == 183,
                    3 => p[4] <= 182,
                    _ => false,
                }
        })
}

pub(super) struct Continuity {
    next: Box<[u8; 8192]>,
    seen: Box<[bool; 8192]>,
}

impl Continuity {
    pub(super) fn new() -> Self {
        Self {
            next: Box::new([0; 8192]),
            seen: Box::new([false; 8192]),
        }
    }

    pub(super) fn rewrite(&mut self, bytes: &mut [u8]) {
        for p in bytes.chunks_exact_mut(PACKET) {
            let pid = (usize::from(p[1] & 31) << 8) | usize::from(p[2]);
            // Null packets have no continuity requirement.
            if pid == 8191 {
                continue;
            }
            let payload = p[3] & 0x10 != 0;
            let counter = if payload || !self.seen[pid] {
                self.next[pid]
            } else {
                self.next[pid].wrapping_sub(1) & 15
            };
            p[3] = (p[3] & 0xf0) | counter;
            if payload {
                self.next[pid] = (counter + 1) & 15;
                self.seen[pid] = true;
            }
        }
    }

    // Emit one packet at a time so even 8191 seen PIDs cannot allocate an
    // unbounded marker batch. Adaptation-only packets do not advance CC.
    pub(super) fn discontinuities(&self) -> impl Iterator<Item = [u8; PACKET]> + '_ {
        self.seen.iter().enumerate().filter_map(|(pid, seen)| {
            if !seen {
                return None;
            }
            let mut p = [0xff; PACKET];
            p[0] = 0x47;
            p[1] = (pid >> 8) as u8;
            p[2] = pid as u8;
            p[3] = 0x20 | (self.next[pid].wrapping_sub(1) & 15);
            p[4] = 183;
            p[5] = 0x80;
            Some(p)
        })
    }
}
