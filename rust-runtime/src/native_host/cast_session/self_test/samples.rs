use super::*;
use crate::experimental_libav::cast_mux::{
    CastFile, CastFormat, CastInputs, CastSession, CastSummary,
};
use sha2::{Digest, Sha256};
mod transport;

pub(super) const SAMPLE_HASH: &str =
    "8624fbee0c7f4653e6f1c7c2f7a6de9aca5d2b3a3a729a218d7f740b9417aef3";
pub(super) const SAMPLE_ASSET: &str = "cast-samples/sample-b.ts";
const SAMPLE_BYTES: usize = 12_079_564;
pub(super) fn verified(bytes: &[u8]) -> bool {
    bytes.len() == SAMPLE_BYTES && format!("{:x}", Sha256::digest(bytes)) == SAMPLE_HASH
}

enum Source {
    Cached(media::Inputs),
    Uniform(Arc<[u8]>),
}
pub(super) struct Sample {
    source: Source,
    summary: CastSummary,
    pub facts: diag::MediaPreflight,
}
pub(super) async fn prepare(
    context: Arc<HostContext>,
    session: Arc<CastSession>,
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
                match session.mux(
                    &probe,
                    &CastInputs {
                        video: &input.video,
                        audio: &input.audio,
                        start_us: 0,
                    },
                    CastFormat::Ts,
                    &worker_stop,
                ) {
                    Ok(file) => {
                        chosen = Some((input, file.summary.clone()));
                        break;
                    }
                    Err(_) => failure = SkipReason::SamplePreparationFailed,
                }
            }
            let (input, summary) = chosen.ok_or(failure)?;
            (Source::Cached(input), summary, None)
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
        _=tokio::time::sleep(Duration::from_secs(30))=>{stop.store(true,Ordering::Release);let _=task.await;Err(SkipReason::SamplePreparationFailed)},
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
        Source::Cached(input) => {
            let probe = media::probe().map_err(|_| SkipReason::SamplePreparationFailed)?;
            session
                .mux(
                    &probe,
                    &CastInputs {
                        video: &input.video,
                        audio: &input.audio,
                        start_us: 0,
                    },
                    format,
                    &worker_stop,
                )
                .map_err(|_| SkipReason::SamplePreparationFailed)
        }
    });
    tokio::select! {biased;
        _=cancel.cancelled()=>{stop.store(true,Ordering::Release);let _=task.await;Err(SkipReason::Cancelled)},
        _=tokio::time::sleep(Duration::from_secs(30))=>{stop.store(true,Ordering::Release);let _=task.await;Err(SkipReason::SamplePreparationFailed)},
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
        Source::Cached(_) => tokio::task::spawn_blocking(move || {
            let Source::Cached(input) = &sample.source else {
                unreachable!()
            };
            let Ok(probe) = media::probe() else {
                producer.finish(false);
                return;
            };
            let _ = producer.generate(
                &probe,
                &CastInputs {
                    video: &input.video,
                    audio: &input.audio,
                    start_us: 0,
                },
                media::MAX_BYTES,
            );
        }),
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
