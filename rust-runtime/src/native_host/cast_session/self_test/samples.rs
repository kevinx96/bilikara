use super::*;
use crate::experimental_libav::cast_mux::{
    CastEvent, CastFile, CastFormat, CastInputs, CastPacing, CastSession, CastStart, CastSummary,
};
use sha2::{Digest, Sha256};
mod transport;

pub(super) const SAMPLE_HASH: &str =
    "8624fbee0c7f4653e6f1c7c2f7a6de9aca5d2b3a3a729a218d7f740b9417aef3";
const SAMPLE_BYTES: usize = 12_079_564;
pub(super) fn read_uniform(context: &HostContext) -> Result<Vec<u8>, SkipReason> {
    if cfg!(target_os = "android") {
        return Err(SkipReason::PlatformUnavailable);
    }
    let path = context
        .sample_root
        .as_ref()
        .ok_or(SkipReason::SampleIntegrityFailed)?
        .join("sample-b.ts");
    let file = std::fs::File::open(path).map_err(|_| SkipReason::SampleIntegrityFailed)?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(SAMPLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SkipReason::SampleIntegrityFailed)?;
    if !verified(&bytes) {
        return Err(SkipReason::SampleIntegrityFailed);
    }
    Ok(bytes)
}
pub(super) fn verified(bytes: &[u8]) -> bool {
    bytes.len() == SAMPLE_BYTES && format!("{:x}", Sha256::digest(bytes)) == SAMPLE_HASH
}

enum Source {
    Cached {
        input: media::Inputs,
        transport: Box<RecordedTs>,
    },
    Uniform(Arc<[u8]>),
}
struct RecordedTs {
    start: CastStart,
    bytes: Vec<u8>,
    chunks: Vec<(std::ops::Range<usize>, i64, bool)>,
    summary: CastSummary,
}
#[cfg(test)]
pub(super) static MUX_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) static RECORDED_END_US: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(0);
const PREPARE_LIMIT: Duration = Duration::from_secs(20);
const SAMPLE_DURATION_US: i64 = 20_000_000;
const MAX_SAMPLE_BYTES: usize = 128 * 1024 * 1024;

// T3 supplies authenticated packet boundaries and decode-time watermarks. Keep
// these once; neither file nor live playback reads/encodes the original again.
fn record_ts(
    probe: &crate::experimental_libav::LibavMetadataProbe,
    input: &media::Inputs,
    stop: &AtomicBool,
) -> Result<RecordedTs, SkipReason> {
    let mut start = None;
    let mut bytes = Vec::new();
    let mut chunks = Vec::new();
    let mut clipped = false;
    #[cfg(test)]
    MUX_COUNT.fetch_add(1, Ordering::Relaxed);
    let result = probe.cast_ts_with_events(
        &CastInputs {
            video: &input.video,
            audio: &input.audio,
            start_us: 0,
        },
        CastPacing::FullSpeed,
        MAX_SAMPLE_BYTES as u64,
        stop,
        &mut |event| {
            match event {
                CastEvent::Start(value) => start = Some(value),
                CastEvent::Data {
                    bytes: data,
                    time_us,
                    key,
                } => {
                    if time_us >= SAMPLE_DURATION_US {
                        clipped = true;
                        return false;
                    }
                    if bytes.len().saturating_add(data.len()) > MAX_SAMPLE_BYTES {
                        return false;
                    }
                    let begin = bytes.len();
                    bytes.extend_from_slice(data);
                    chunks.push((begin..bytes.len(), time_us, key));
                }
            }
            !stop.load(Ordering::Acquire)
        },
    );
    if stop.load(Ordering::Acquire) || (result.is_err() && !clipped) || bytes.is_empty() {
        return Err(SkipReason::SamplePreparationFailed);
    }
    let start = start.ok_or(SkipReason::SamplePreparationFailed)?;
    #[cfg(test)]
    RECORDED_END_US.store(chunks.last().unwrap().1, Ordering::Relaxed);
    let keys: Vec<_> = chunks
        .iter()
        .filter_map(|(_, time, key)| key.then_some(*time))
        .collect();
    let mut summary = result.unwrap_or(CastSummary {
        packets: (bytes.len() / 188) as u64,
        bytes: bytes.len() as u64,
        profile: start.profile,
        level: start.level,
        width: start.width,
        height: start.height,
        has_b_frames: start.has_b_frames,
        audio_transcoded: start.audio_transcoded,
        source_start_us: start.source_start_us,
        duration_us: SAMPLE_DURATION_US,
        gop_us: keys.windows(2).map(|pair| pair[1] - pair[0]).max(),
    });
    summary.duration_us = summary.duration_us.min(SAMPLE_DURATION_US);
    Ok(RecordedTs {
        start,
        bytes,
        chunks,
        summary,
    })
}
pub(super) struct Sample {
    source: Source,
    summary: CastSummary,
    pub facts: diag::MediaPreflight,
}
pub(super) async fn prepare(
    context: Arc<HostContext>,
    _session: Arc<CastSession>,
    uniform: Option<Vec<u8>>,
    cancel: &CancellationToken,
) -> Result<Arc<Sample>, SkipReason> {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let mut task = tokio::task::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let (source, summary, fps) = if let Some(bytes) = uniform {
            if !verified(&bytes) || transport::index(&bytes).is_err() {
                return Err(SkipReason::SampleIntegrityFailed);
            }
            // Facts are bound to the exact SHA-256 in SOURCE.md. No encoding or
            // media decoder is needed to serve the preencoded comparison clip.
            (
                Source::Uniform(bytes.into()),
                CastSummary {
                    packets: 64253,
                    bytes: SAMPLE_BYTES as u64,
                    profile: 77,
                    level: 40,
                    width: 1920,
                    height: 1080,
                    has_b_frames: false,
                    audio_transcoded: false,
                    source_start_us: 0,
                    duration_us: 20_021_000,
                    gop_us: Some(500_000),
                },
                Some(30_000),
            )
        } else {
            let items = with_app(|app| {
                let s = app.native_core_snapshot()?;
                Ok(s.current_item
                    .into_iter()
                    .chain(s.playlist)
                    .collect::<Vec<_>>())
            })
            .map_err(|_| SkipReason::NoCachedSong)?;
            let mut chosen = None;
            let mut failure = SkipReason::NoCachedSong;
            for item in items {
                if worker_stop.load(Ordering::Acquire) {
                    return Err(SkipReason::Cancelled);
                }
                let Ok(input) = media::inputs(&context, &item) else {
                    continue;
                };
                let probe = media::probe().map_err(|_| SkipReason::SamplePreparationFailed)?;
                let metadata = match probe.probe_metadata(&input.video, &worker_stop) {
                    Ok(m) => m,
                    Err(_) => {
                        failure = SkipReason::SamplePreparationFailed;
                        continue;
                    }
                };
                if !metadata
                    .streams
                    .iter()
                    .any(|s| s.codec_name.as_deref() == Some("h264"))
                {
                    continue;
                }
                match record_ts(&probe, &input, &worker_stop) {
                    Ok(transport) => {
                        let summary = transport.summary.clone();
                        chosen = Some((
                            Source::Cached {
                                input,
                                transport: Box::new(transport),
                            },
                            summary,
                        ));
                        break;
                    }
                    // Once an eligible source enters the muxer, do not try the
                    // rest of the queue: one TS merge plus optional MP4 is the
                    // entire test's generation budget, even on failure.
                    Err(reason) => return Err(reason),
                }
            }
            let (source, summary) = chosen.ok_or(failure)?;
            (source, summary, None)
        };
        let facts = diag::MediaPreflight {
            video_codec: Some(diag::Codec::H264),
            profile: Some(summary.profile),
            level: Some(summary.level),
            width: Some(summary.width),
            height: Some(summary.height),
            has_b_frames: Some(summary.has_b_frames),
            gop_us: summary.gop_us,
            audio_codec: Some(diag::Codec::Aac),
            synthesis_ms: Some(started.elapsed().as_millis() as u64),
            file_size: Some(summary.bytes),
            fps_milli: fps,
        };
        Ok(Arc::new(Sample {
            source,
            summary,
            facts,
        }))
    });
    tokio::select! {biased;
        _=cancel.cancelled()=>{stop.store(true,Ordering::Release);let _=task.await;Err(SkipReason::Cancelled)},
        _=tokio::time::sleep(PREPARE_LIMIT)=>{stop.store(true,Ordering::Release);let _=task.await;Err(SkipReason::SamplePreparationFailed)},
        result=&mut task=>result.unwrap_or(Err(SkipReason::SamplePreparationFailed)),
    }
}
pub(super) async fn remux(
    session: Arc<CastSession>,
    sample: Arc<Sample>,
    format: CastFormat,
    cancel: &CancellationToken,
) -> Result<CastFile, SkipReason> {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let mut task = tokio::task::spawn_blocking(move || match &sample.source {
        Source::Uniform(bytes) if format == CastFormat::Ts => session
            .retain_ts(bytes, SAMPLE_HASH, sample.summary.clone())
            .map_err(|_| SkipReason::SamplePreparationFailed),
        Source::Uniform(_) => Err(SkipReason::SamplePreparationFailed),
        Source::Cached { input, transport } => match format {
            CastFormat::Mp4 => {
                #[cfg(test)]
                MUX_COUNT.fetch_add(1, Ordering::Relaxed);
                let probe = media::probe().map_err(|_| SkipReason::SamplePreparationFailed)?;
                session
                    .mux(
                        &probe,
                        &CastInputs {
                            video: &input.video,
                            audio: &input.audio,
                            start_us: 0,
                        },
                        CastFormat::Mp4,
                        &worker_stop,
                    )
                    .map_err(|_| SkipReason::SamplePreparationFailed)
            }
            CastFormat::Ts => session
                .retain_ts(
                    &transport.bytes,
                    &format!("{:x}", Sha256::digest(&transport.bytes)),
                    sample.summary.clone(),
                )
                .map_err(|_| SkipReason::SamplePreparationFailed),
        },
    });
    tokio::select! {biased;
        _=cancel.cancelled()=>{stop.store(true,Ordering::Release);let _=task.await;Err(SkipReason::Cancelled)},
        _=tokio::time::sleep(PREPARE_LIMIT)=>{stop.store(true,Ordering::Release);let _=task.await;Err(SkipReason::SamplePreparationFailed)},
        result=&mut task=>result.unwrap_or(Err(SkipReason::SamplePreparationFailed)),
    }
}
pub(super) struct Live {
    pub stream: Arc<crate::dlna::cast_http::LiveStream>,
    task: tokio::task::JoinHandle<()>,
    stop: CancellationToken,
}
impl Live {
    pub async fn stop(self) {
        self.stop.cancel();
        self.stream.cancel();
        let _ = self.task.await;
    }
}
pub(super) async fn live(
    sample: Arc<Sample>,
    cancel: &CancellationToken,
) -> Result<Live, SkipReason> {
    use crate::dlna::cast_http::{LiveOptions, LiveStream};
    let stream = Arc::new(
        LiveStream::new(LiveOptions::default()).map_err(|_| SkipReason::SamplePreparationFailed)?,
    );
    let producer = stream.clone();
    let stop = cancel.child_token();
    let task = match &sample.source {
        Source::Uniform(bytes) => {
            let bytes = bytes.clone();
            let stopping = stop.clone();
            tokio::spawn(async move {
                let ok = transport::produce(&producer, &bytes, &stopping)
                    .await
                    .is_ok();
                producer.finish(ok);
            })
        }
        Source::Cached { .. } => {
            let stopping = stop.clone();
            tokio::spawn(async move {
                let Source::Cached { transport, .. } = &sample.source else {
                    unreachable!()
                };
                let ok: Result<(), ()> = async {
                    producer.push(CastEvent::Start(transport.start)).map_err(|_| ())?;
                    let began = tokio::time::Instant::now();
                    for (range, time_us, key) in &transport.chunks {
                        let pace = Duration::from_micros((*time_us).max(0) as u64).saturating_sub(Duration::from_secs(5));
                        tokio::select! {biased; _=stopping.cancelled()=>return Err(()), _=tokio::time::sleep_until(began+pace)=>{}}
                        producer.push(CastEvent::Data { bytes: &transport.bytes[range.clone()], time_us: *time_us, key: *key }).map_err(|_| ())?;
                    }
                    Ok(())
                }.await;
                producer.finish(ok.is_ok());
            })
        }
    };
    let live = Live { stream, task, stop };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if live.stream.snapshot().start.is_some() {
            return Ok(live);
        }
        if cancel.is_cancelled()
            || tokio::time::Instant::now() >= deadline
            || live.task.is_finished()
        {
            live.stop().await;
            return Err(SkipReason::SamplePreparationFailed);
        }
        tokio::select! {_=cancel.cancelled()=>{},_=tokio::time::sleep(Duration::from_millis(10))=>{}}
    }
}
