//! Whole-GOP, bounded multi-reader storage for T3's schema-2 event stream.
use super::continuity;
use crate::experimental_libav::{LibavMetadataProbe, ProbeError, cast_mux::*};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

const MAX_CHUNK: usize = 188 * 128;
pub const MAX_RING_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct LiveOptions {
    /// Never less than ten seconds. Whole GOPs may extend this window.
    pub retention: Duration,
    /// Shared with T3's producer pacing and each reader's initial burst.
    pub burst: Duration,
    pub max_bytes: usize,
    pub max_client_lag_bytes: usize,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(10),
            burst: Duration::from_secs(5),
            max_bytes: 64 * 1024 * 1024,
            max_client_lag_bytes: 1024 * 1024,
        }
    }
}

impl LiveOptions {
    pub fn pacing(&self) -> CastPacing {
        CastPacing::Realtime { burst: self.burst }
    }

    fn validate(&self) -> Result<(), LiveError> {
        if self.retention < Duration::from_secs(10)
            || self.retention > Duration::from_secs(120)
            || self.burst > self.retention
            || self.max_bytes < MAX_CHUNK
            || self.max_bytes > MAX_RING_BYTES
            || self.max_client_lag_bytes < MAX_CHUNK
            || self.max_client_lag_bytes > self.max_bytes
        {
            return Err(LiveError::InvalidOptions);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveError {
    InvalidOptions,
    InvalidEvent,
    /// The ten-second window / current GOP cannot fit. Never silently retain
    /// less history or block the generator on one HTTP consumer.
    BufferLimit,
    ProducerFailed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct LiveSnapshot {
    pub start: Option<CastStart>,
    pub buffered_bytes: usize,
    pub retained_span: Duration,
    pub keyframes: usize,
    pub finished: bool,
    pub error: Option<LiveError>,
}

struct Chunk {
    index: u64,
    sequence: u64,
    time_us: i64,
    bytes: Arc<[u8]>,
}

#[derive(Default)]
struct Ring {
    start: Option<CastStart>,
    chunks: VecDeque<Chunk>,
    keys: VecDeque<(u64, i64, u64)>,
    bytes: usize,
    next: u64,
    next_index: u64,
    last_time: i64,
    finished: bool,
    error: Option<LiveError>,
}

pub struct LiveStream {
    pub(super) options: LiveOptions,
    ring: Mutex<Ring>,
    pub(super) wake: Notify,
    stop: AtomicBool,
}

pub(super) struct Cursor {
    sequence: Option<u64>,
    index: u64,
}

impl Cursor {
    pub(super) fn new() -> Self {
        Self {
            sequence: None,
            index: 0,
        }
    }
}

pub(super) enum Read {
    Data {
        bytes: Arc<[u8]>,
        time_us: i64,
        jumped: bool,
    },
    Wait,
    End(Result<(), LiveError>),
}

impl LiveStream {
    pub fn new(options: LiveOptions) -> Result<Self, LiveError> {
        options.validate()?;
        Ok(Self {
            options,
            ring: Mutex::new(Ring::default()),
            wake: Notify::new(),
            stop: AtomicBool::new(false),
        })
    }

    /// Use on a worker thread. Start metadata is visible before any Data.
    /// Reader disconnects never cancel this producer. Session end does.
    pub fn generate(
        &self,
        probe: &LibavMetadataProbe,
        inputs: &CastInputs<'_>,
        max_bytes: u64,
    ) -> Result<CastSummary, ProbeError> {
        let result = probe.cast_ts_with_events(
            inputs,
            self.options.pacing(),
            max_bytes,
            &self.stop,
            &mut |event| self.push(event).is_ok(),
        );
        self.finish(result.is_ok());
        result
    }

    /// Nonblocking T3 callback adapter. Bounded copies only, no client IO.
    pub fn push(&self, event: CastEvent<'_>) -> Result<(), LiveError> {
        if self.stop.load(Ordering::Acquire) {
            return Err(LiveError::Cancelled);
        }
        let mut r = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if self.stop.load(Ordering::Acquire) {
            return Err(LiveError::Cancelled);
        }
        if let Some(error) = r.error {
            return Err(error);
        }
        let result = if r.finished {
            Err(LiveError::InvalidEvent)
        } else {
            match event {
                CastEvent::Start(start) if r.start.is_none() && r.chunks.is_empty() => {
                    r.start = Some(start);
                    Ok(())
                }
                CastEvent::Data {
                    bytes,
                    time_us,
                    key,
                } => self.append(&mut r, bytes, time_us, key),
                _ => Err(LiveError::InvalidEvent),
            }
        };
        if let Err(error) = result {
            r.error = Some(error);
            r.finished = true;
        }
        drop(r);
        self.wake.notify_waiters();
        result
    }

    fn append(&self, r: &mut Ring, bytes: &[u8], time_us: i64, key: bool) -> Result<(), LiveError> {
        if r.start.is_none()
            || (r.chunks.is_empty() && !key)
            || time_us < 0
            || time_us < r.last_time
            || bytes.len() > MAX_CHUNK
            || !continuity::valid(bytes)
        {
            return Err(LiveError::InvalidEvent);
        }
        let cutoff = time_us.saturating_sub(self.options.retention.as_micros() as i64);
        // Keep the entire GOP containing the beginning of the window.
        while let Some(&(sequence, _, _)) = r.keys.get(1).filter(|(_, time, _)| *time <= cutoff) {
            while r.chunks.front().is_some_and(|c| c.sequence < sequence) {
                r.bytes -= r.chunks.pop_front().expect("front exists").bytes.len();
            }
            r.keys.pop_front();
        }
        if bytes.len() > self.options.max_bytes.saturating_sub(r.bytes) {
            return Err(LiveError::BufferLimit);
        }
        let sequence = r.next;
        r.next = r
            .next
            .checked_add(bytes.len() as u64)
            .ok_or(LiveError::BufferLimit)?;
        r.bytes += bytes.len();
        r.last_time = time_us;
        let index = r.next_index;
        r.next_index = r.next_index.checked_add(1).ok_or(LiveError::BufferLimit)?;
        if key {
            r.keys.push_back((sequence, time_us, index));
        }
        r.chunks.push_back(Chunk {
            index,
            sequence,
            time_us,
            bytes: Arc::from(bytes),
        });
        Ok(())
    }

    /// External producers call this after EOF or failure; generate does it for
    /// callers automatically. Already reported ring errors are preserved.
    pub fn finish(&self, success: bool) {
        let mut r = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        r.finished = true;
        if !success && r.error.is_none() {
            r.error = Some(LiveError::ProducerFailed);
        }
        drop(r);
        self.wake.notify_waiters();
    }

    pub fn cancel(&self) {
        if !self.stop.swap(true, Ordering::AcqRel) {
            let mut r = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            r.chunks = VecDeque::new();
            r.keys = VecDeque::new();
            r.bytes = 0;
            r.finished = true;
            r.error = Some(LiveError::Cancelled);
        }
        self.wake.notify_waiters();
    }

    pub fn snapshot(&self) -> LiveSnapshot {
        let r = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        LiveSnapshot {
            start: r.start,
            buffered_bytes: r.bytes,
            retained_span: Duration::from_micros(
                r.chunks
                    .front()
                    .map_or(0, |c| r.last_time - c.time_us)
                    .max(0) as u64,
            ),
            keyframes: r.keys.len(),
            finished: r.finished,
            error: r.error,
        }
    }

    pub(super) fn read(&self, cursor: &mut Cursor) -> Read {
        if self.stop.load(Ordering::Acquire) {
            return Read::End(Err(LiveError::Cancelled));
        }
        let r = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = r.error {
            return Read::End(Err(error));
        }
        let latest = r.keys.back();
        let mut jumped = false;
        match cursor.sequence {
            None => {
                if let Some(&(sequence, _, index)) = latest {
                    cursor.sequence = Some(sequence);
                    cursor.index = index;
                }
            }
            Some(sequence) => {
                let expired = r.chunks.front().is_some_and(|c| sequence < c.sequence);
                let lagged =
                    r.next.saturating_sub(sequence) > self.options.max_client_lag_bytes as u64;
                if (expired || lagged)
                    && let Some(&(key_sequence, _, index)) =
                        latest.filter(|(key_sequence, _, _)| *key_sequence > sequence)
                {
                    cursor.sequence = Some(key_sequence);
                    cursor.index = index;
                    jumped = true;
                }
            }
        }
        let chunk = r
            .chunks
            .front()
            .and_then(|front| cursor.index.checked_sub(front.index))
            .and_then(|offset| usize::try_from(offset).ok())
            .and_then(|offset| r.chunks.get(offset));
        if let Some(chunk) = chunk.filter(|chunk| cursor.sequence == Some(chunk.sequence)) {
            cursor.sequence = Some(chunk.sequence + chunk.bytes.len() as u64);
            cursor.index += 1;
            Read::Data {
                bytes: chunk.bytes.clone(),
                time_us: chunk.time_us,
                jumped,
            }
        } else if r.finished {
            Read::End(Ok(()))
        } else {
            Read::Wait
        }
    }
}
