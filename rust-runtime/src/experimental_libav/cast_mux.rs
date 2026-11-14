//! Cast generator adapted from bilikara-next experimental_libav/live_ts.rs
//! (MIT, Copyright (c) 2026 bilikara contributors). No HTTP or AppState owner.
use super::{LibavMetadataProbe, ProbeError};
use std::{path::Path, sync::atomic::AtomicBool, time::Duration};
mod files;
pub use files::{CastFile, CastSession};

pub struct CastInputs<'a> {
    pub video: &'a Path,
    pub audio: &'a Path,
    /// Seek backward to a keyframe; summary reports the actual presentation start.
    pub start_us: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastFormat {
    Ts,
    Mp4,
}
#[derive(Clone, Copy, Debug)]
pub enum CastPacing {
    FullSpeed,
    Realtime { burst: Duration },
}
impl Default for CastPacing {
    fn default() -> Self {
        Self::Realtime {
            burst: Duration::from_secs(5),
        }
    }
}
#[derive(Debug, Clone, serde::Serialize)]
pub struct CastSummary {
    pub packets: u64,
    pub bytes: u64,
    pub profile: i32,
    pub level: i32,
    pub width: u32,
    pub height: u32,
    pub has_b_frames: bool,
    pub audio_transcoded: bool,
    pub source_start_us: i64,
    pub duration_us: i64,
    /// Maximum observed keyframe interval; None if only one keyframe was seen.
    pub gop_us: Option<i64>,
}
impl LibavMetadataProbe {
    pub fn cast_mux_available(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            native::Capability::load(&self._library).is_ok()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }
    /// Full read-only packet preflight; reports source video characteristics for T7a.
    pub fn cast_preflight(
        &self,
        video: &Path,
        stop: &AtomicBool,
    ) -> Result<CastSummary, ProbeError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            native::preflight(self, video, stop)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = (video, stop);
            Err(ProbeError::Unavailable(
                "cast companion not packaged".into(),
            ))
        }
    }
    /// Synchronous worker-thread generator. Callback receives <=24064 borrowed
    /// TS bytes and monotonic decode time. `key` starts an independently joinable
    /// PAT/PMT + IDR group; T4 retains whole groups and rewrites continuity on join.
    /// Return false to cancel. No ring or HTTP client state is owned here.
    pub fn cast_ts(
        &self,
        inputs: &CastInputs<'_>,
        pacing: CastPacing,
        max_bytes: u64,
        stop: &AtomicBool,
        callback: &mut dyn FnMut(&[u8], i64, bool) -> bool,
    ) -> Result<CastSummary, ProbeError> {
        self.cast_run(inputs, None, pacing, max_bytes, stop, callback)
    }
    fn cast_run(
        &self,
        inputs: &CastInputs<'_>,
        output: Option<&Path>,
        pacing: CastPacing,
        max_bytes: u64,
        stop: &AtomicBool,
        callback: &mut dyn FnMut(&[u8], i64, bool) -> bool,
    ) -> Result<CastSummary, ProbeError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            native::run(self, inputs, output, pacing, max_bytes, stop, callback)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = (inputs, output, pacing, max_bytes, stop, callback);
            Err(ProbeError::Unavailable(
                "cast companion not packaged on this platform".into(),
            ))
        }
    }
}
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod native {
    use super::*;
    use crate::experimental_libav::{backend_error, open_input, status_error, wire};
    use std::{
        ffi::c_void,
        mem::{size_of, transmute},
        sync::atomic::Ordering,
    };
    #[repr(C)]
    struct Info {
        schema: u32,
        request_size: u32,
        result_size: u32,
    }
    #[repr(C)]
    struct Request {
        video: wire::Request,
        audio: wire::Request,
        start_us: i64,
        visit: extern "C" fn(*mut c_void, *const u8, u32, i64, u32) -> i32,
        opaque: *mut c_void,
        output_kind: u32,
        staging_path: *const std::ffi::c_char,
        max_bytes: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ResultData {
        status: u32,
        complete: u32,
        packets: u64,
        bytes: u64,
        profile: i32,
        level: i32,
        width: u32,
        height: u32,
        has_b_frames: u32,
        audio_transcoded: u32,
        source_start_us: i64,
        duration_us: i64,
        gop_us: i64,
    }
    pub(super) struct Capability {
        run: unsafe extern "C" fn(*const Request, *mut ResultData) -> u32,
    }
    impl Capability {
        pub(super) fn load(library: &wire::Library) -> Result<Self, ProbeError> {
            // SAFETY: the enclosing probe has negotiated the trusted companion.
            unsafe {
                let info: unsafe extern "C" fn(u32, *mut Info) -> u32 =
                    transmute(library.symbol(c"bm_cast_info_v1")?);
                let mut raw = Info {
                    schema: 0,
                    request_size: 0,
                    result_size: 0,
                };
                if info(size_of::<Info>() as u32, &mut raw) != 0
                    || raw.schema != 1
                    || raw.request_size as usize != size_of::<Request>()
                    || raw.result_size as usize != size_of::<ResultData>()
                {
                    return Err(ProbeError::Unavailable(
                        "cast mux capability unavailable".into(),
                    ));
                }
                Ok(Self {
                    run: transmute::<
                        *mut c_void,
                        unsafe extern "C" fn(*const Request, *mut ResultData) -> u32,
                    >(library.symbol(c"bm_cast_mux_v1")?),
                })
            }
        }
    }
    struct Visitor<'a> {
        cancelled: &'a AtomicBool,
        callback: &'a mut dyn FnMut(&[u8], i64, bool) -> bool,
        failed: bool,
        stopped: bool,
        pacing: CastPacing,
        began: std::time::Instant,
        last_us: i64,
        bytes: u64,
        limit: u64,
    }
    extern "C" fn cancelled(opaque: *mut c_void) -> i32 {
        // SAFETY: synchronous call borrows this visitor for its full duration.
        let visitor = unsafe { &*(opaque as *const Visitor<'_>) };
        i32::from(visitor.cancelled.load(Ordering::Acquire) || visitor.failed || visitor.stopped)
    }
    extern "C" fn visit(
        opaque: *mut c_void,
        bytes: *const u8,
        size: u32,
        time: i64,
        key: u32,
    ) -> i32 {
        // SAFETY: callback context belongs to this synchronous invocation.
        let visitor = unsafe { &mut *(opaque as *mut Visitor<'_>) };
        if bytes.is_null()
            || size == 0
            || size > 24064
            || !size.is_multiple_of(188)
            || time < 0
            || key > 1
            || time < visitor.last_us
            || u64::from(size) > visitor.limit.saturating_sub(visitor.bytes)
        {
            visitor.failed = true;
            return 1;
        }
        // SAFETY: companion borrows exactly size bytes until this call returns.
        let bytes = unsafe { std::slice::from_raw_parts(bytes, size as usize) };
        if bytes.chunks_exact(188).any(|p| p[0] != 0x47) {
            visitor.failed = true;
            return 1;
        }
        visitor.last_us = time;
        visitor.bytes += u64::from(size);
        if let CastPacing::Realtime { burst } = visitor.pacing {
            let target = std::time::Duration::from_micros(time as u64).saturating_sub(burst);
            while visitor.began.elapsed() < target {
                if visitor.cancelled.load(Ordering::Acquire) {
                    visitor.stopped = true;
                    return 1;
                }
                std::thread::sleep(
                    (target - visitor.began.elapsed().min(target))
                        .min(std::time::Duration::from_millis(10)),
                );
            }
        }
        if visitor.cancelled.load(Ordering::Acquire) {
            visitor.stopped = true;
            return 1;
        }
        let accepted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (visitor.callback)(bytes, time, key == 1)
        }));
        match accepted {
            Ok(true) => 0,
            Ok(false) => {
                visitor.stopped = true;
                1
            }
            Err(_) => {
                visitor.failed = true;
                1
            }
        }
    }
    pub(super) fn preflight(
        probe: &LibavMetadataProbe,
        path: &Path,
        stop: &AtomicBool,
    ) -> Result<CastSummary, ProbeError> {
        let _capability = Capability::load(&probe._library)?;
        type Preflight = unsafe extern "C" fn(*const wire::Request, *mut ResultData) -> u32;
        // SAFETY: negotiated result layout and trusted companion.
        let check: Preflight =
            unsafe { transmute(probe._library.symbol(c"bm_cast_preflight_v1")?) };
        let file = open_input(path)?;
        let fd = probe._library.descriptor(&file)?;
        let mut callback = super::super::Callback::new(stop);
        let request = wire::Request {
            fd: fd.fd,
            cancelled: super::super::cancellation_callback,
            opaque: (&mut callback as *mut super::super::Callback<'_>).cast(),
        };
        let mut result = ResultData::default();
        // SAFETY: all borrows remain alive until the synchronous call returns.
        let status = unsafe { check(&request, &mut result) };
        if status != 0 {
            return Err(status_error(
                status,
                "cast preflight invocation failed".into(),
            ));
        }
        if result.status != 0 {
            return Err(status_error(result.status, "cast preflight failed".into()));
        }
        if result.complete != 1
            || result.width == 0
            || result.height == 0
            || result.duration_us <= 0
            || result.has_b_frames > 1
            || result.gop_us < 0
            || result.packets == 0
        {
            return Err(backend_error("invalid cast preflight result"));
        }
        Ok(CastSummary {
            packets: result.packets,
            bytes: result.bytes,
            profile: result.profile,
            level: result.level,
            width: result.width,
            height: result.height,
            has_b_frames: result.has_b_frames != 0,
            audio_transcoded: false,
            source_start_us: result.source_start_us,
            duration_us: result.duration_us,
            gop_us: (result.gop_us > 0).then_some(result.gop_us),
        })
    }
    pub(super) fn run(
        probe: &LibavMetadataProbe,
        inputs: &CastInputs<'_>,
        output: Option<&Path>,
        pacing: CastPacing,
        limit: u64,
        stop: &AtomicBool,
        callback: &mut dyn FnMut(&[u8], i64, bool) -> bool,
    ) -> Result<CastSummary, ProbeError> {
        let start_us = inputs.start_us;
        if !(0..=21_600_000_000).contains(&start_us) || limit == 0 || limit > i64::MAX as u64 {
            return Err(backend_error("invalid cast request bounds"));
        }
        let staging = output
            .map(|p| {
                p.to_str()
                    .ok_or_else(|| backend_error("staging path must be UTF-8"))
                    .and_then(|p| {
                        std::ffi::CString::new(p)
                            .map_err(|_| backend_error("staging path contains NUL"))
                    })
            })
            .transpose()?;
        let capability = Capability::load(&probe._library)?;
        let video = open_input(inputs.video)?;
        let audio = open_input(inputs.audio)?;
        let video_fd = probe._library.descriptor(&video)?;
        let audio_fd = probe._library.descriptor(&audio)?;
        let mut visitor = Visitor {
            cancelled: stop,
            callback,
            failed: false,
            stopped: false,
            pacing,
            began: std::time::Instant::now(),
            last_us: 0,
            bytes: 0,
            limit,
        };
        let opaque = (&mut visitor as *mut Visitor<'_>).cast();
        let request = Request {
            video: wire::Request {
                fd: video_fd.fd,
                cancelled,
                opaque,
            },
            audio: wire::Request {
                fd: audio_fd.fd,
                cancelled,
                opaque,
            },
            start_us,
            visit,
            opaque,
            output_kind: u32::from(staging.is_some()),
            staging_path: staging.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            max_bytes: limit,
        };
        let mut result = ResultData::default();
        // SAFETY: descriptors/library/context remain live for call and cleanup.
        let status = unsafe { (capability.run)(&request, &mut result) };
        if visitor.failed {
            return Err(backend_error("invalid cast mux visitor output"));
        }
        if visitor.stopped || stop.load(Ordering::Acquire) {
            return Err(ProbeError::Cancelled);
        }
        if status != 0 {
            return Err(status_error(status, "cast mux invocation failed".into()));
        }
        if result.status != 0 {
            return Err(status_error(
                result.status,
                "cast mux pipeline failed".into(),
            ));
        }
        if result.complete != 1
            || result.packets == 0
            || result.bytes == 0
            || result.bytes > limit
            || result.width == 0
            || result.height == 0
            || result.duration_us <= 0
            || result.has_b_frames > 1
            || result.audio_transcoded > 1
            || result.gop_us < 0
            || (output.is_none() && result.bytes != visitor.bytes)
        {
            return Err(backend_error("incomplete cast mux output"));
        }
        Ok(CastSummary {
            packets: result.packets,
            bytes: result.bytes,
            profile: result.profile,
            level: result.level,
            width: result.width,
            height: result.height,
            has_b_frames: result.has_b_frames != 0,
            audio_transcoded: result.audio_transcoded != 0,
            source_start_us: result.source_start_us,
            duration_us: result.duration_us,
            gop_us: (result.gop_us > 0).then_some(result.gop_us),
        })
    }
}

#[cfg(all(
    test,
    any(target_os = "windows", target_os = "linux", target_os = "macos")
))]
mod tests;
