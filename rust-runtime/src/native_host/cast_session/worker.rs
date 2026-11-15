use super::*;
use crate::app_state::{AppStateRequest, PlaylistItem};
use crate::dlna::{
    Operation,
    cast_http::{CastEndpoint, HttpStageResult, ServerIdentity, TsMime},
    control::{CapabilityQuery, ControlClient, MonitorDecision, StartRequest, TransportMonitor},
    diagnostics::{self, OutputConfig},
    discovery,
    dlna_policy::{FileMetadata, TransportState},
    purek,
};
use crate::experimental_libav::cast_mux::{CastFile, CastSession};
use std::time::Instant;

fn update<T>(operation: u64, action: impl FnOnce(&mut State) -> T) -> Option<T> {
    with_app(|app| {
        if app.native().cast.operation != operation {
            return Ok(None);
        }
        let result = action(&mut app.native().cast);
        app.native().revision += 1;
        Ok(Some(result))
    })
    .ok()
    .flatten()
}
fn platform() -> diagnostics::Platform {
    if cfg!(windows) {
        diagnostics::Platform::Windows
    } else if cfg!(target_os = "macos") {
        diagnostics::Platform::Macos
    } else if cfg!(target_os = "android") {
        diagnostics::Platform::Android
    } else {
        diagnostics::Platform::Linux
    }
}
fn error(code: &str) -> ApiError {
    ApiError::new(409, code, code)
}
fn record<T>(
    trace: &SessionTrace,
    connection: u64,
    output: Option<u64>,
    operation: Operation<T>,
) -> Result<T, ApiError> {
    trace.record_operation(Some(connection), output, &operation);
    operation.result.map_err(|e| error(e.code.as_str()))
}

pub(super) fn run(context: Arc<HostContext>) {
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()
    else {
        let operation = with_app(|app| {
            app.native().cast.worker_started = false;
            app.native().cast.pending = None;
            app.native().revision += 1;
            Ok(app.native().cast.operation)
        });
        if let Ok(operation) = operation {
            restore(operation, Some("cast_worker_unavailable".into()));
        }
        return;
    };
    runtime.block_on(async {
        while !context.stop.load(Ordering::Acquire) {
            let pending = with_app(|app| {
                let s = &mut app.native().cast;
                Ok(s.pending.take().map(|r| {
                    s.cancel = CancellationToken::new();
                    s.mux_stop = Arc::new(AtomicBool::new(false));
                    (s.operation, r, s.cancel.clone(), s.mux_stop.clone())
                }))
            })
            .ok()
            .flatten();
            let Some((id, request, cancel, stop)) = pending else {
                tokio::time::sleep(Duration::from_millis(30)).await;
                continue;
            };
            let watching = context.clone();
            let watch_cancel = cancel.clone();
            let watch_stop = stop.clone();
            let watch = tokio::spawn(async move {
                while !watch_cancel.is_cancelled() && !watching.stop.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                watch_cancel.cancel();
                watch_stop.store(true, Ordering::Release);
            });
            match request {
                Request::Disconnect => {
                    restore(id, None);
                }
                Request::Discover(location) => {
                    if let Ok(trace) = SessionTrace::new(
                        platform(),
                        diagnostics::TargetKind::Generic,
                        env!("CARGO_PKG_VERSION"),
                        std::env::consts::OS,
                    ) {
                        let trace = Arc::new(trace);
                        let connection = trace.begin_connection();
                        update(id, |s| s.trace = Some(trace.clone()));
                        let result = if let Some(location) = location {
                            record(
                                &trace,
                                connection,
                                None,
                                discovery::load_renderer(&location, &cancel).await,
                            )
                            .map(|r| vec![r])
                        } else {
                            let result = discovery::discover(&cancel, |event| {
                                trace.discovery(connection, &event);
                                if let discovery::DiscoveryUpdate::Renderers(renderers) = event {
                                    update(id, |s| s.renderers = renderers);
                                }
                            })
                            .await;
                            result.result.map_err(|e| error(e.code.as_str()))
                        };
                        trace.finish(cancel.is_cancelled());
                        update(id, |s| {
                            s.phase = Phase::Idle;
                            match result {
                                Ok(r) => s.renderers = r,
                                Err(e) => s.error_code = Some(e.code),
                            }
                        });
                    }
                }
                Request::Connect {
                    renderer,
                    qr,
                    confirmed,
                } => {
                    let result = connect(
                        &context,
                        id,
                        renderer.map(|r| *r),
                        qr,
                        confirmed,
                        &cancel,
                        &stop,
                    )
                    .await;
                    restore(id, result.err().map(|e| e.code));
                }
            }
            cancel.cancel();
            stop.store(true, Ordering::Release);
            let _ = watch.await;
        }
    });
}

fn restore(id: u64, failure: Option<String>) {
    let _ = with_app(|app| {
        let snap = app.native_cast_input()?;
        let state = &mut app.native().cast;
        // Even a superseding disconnect must receive the last receiver position.
        // A new connection will capture this resume before taking ownership.
        if state.owns_playback || state.resume.is_some() {
            if state.playback_generation != snap.playback_generation {
                state.position = 0.0;
                state.paused = false;
            }
            state.resume = Some(
                json!({"playback_generation":snap.playback_generation,"item_id":snap.current_item.as_ref().map(|i|&i.id),"position":state.position,"is_paused":state.paused,"operation_id":id}),
            );
        }
        if state.operation == id || matches!(state.pending, Some(Request::Disconnect)) {
            state.owns_playback = false;
            state.phase = Phase::Idle;
            state.error_code = failure;
            state.control_busy = false;
            state.receiver_state = None;
        }
        app.native().revision += 1;
        Ok(())
    });
}

async fn connect(
    context: &Arc<HostContext>,
    id: u64,
    renderer: Option<Renderer>,
    qr: Option<String>,
    confirmed: bool,
    cancel: &CancellationToken,
    stop: &Arc<AtomicBool>,
) -> Result<(), ApiError> {
    let trace = Arc::new(
        SessionTrace::new(
            platform(),
            if qr.is_some() {
                diagnostics::TargetKind::Purek
            } else {
                diagnostics::TargetKind::Generic
            },
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
        )
        .map_err(|_| error("cast_diagnostics_unavailable"))?,
    );
    let connection = trace.begin_connection();
    update(id, |s| s.trace = Some(trace.clone()));
    let mut room = None;
    let renderer = if let Some(qr) = qr {
        let operation = connect_room(&qr, confirmed, cancel).await;
        trace.record_purek_operation(connection, &operation);
        let session = operation.result.map_err(|e| error(e.code.as_str()))?;
        let renderer = session.renderer();
        room = Some(session);
        renderer.map_err(|e| error(e.code.as_str()))
    } else {
        renderer.ok_or_else(|| error("cast_renderer_unavailable"))
    };
    // Never use `?` after acquiring the room without passing this cleanup.
    let result = match renderer {
        Ok(renderer) => {
            play_session(
                context,
                id,
                renderer,
                (&trace, connection),
                cancel,
                stop,
                &mut room,
            )
            .await
        }
        Err(e) => Err(e),
    };
    stop.store(true, Ordering::Release);
    if let Some(room) = &mut room {
        let release = room.release().await;
        trace.record_purek_operation(connection, &release);
    }
    trace.finish(cancel.is_cancelled());
    result
}

struct Output {
    id: u64,
    generation: u64,
    item: PlaylistItem,
    playback_generation: u64,
    live: Option<media::Live>,
    began: Instant,
    start: f64,
    receiver_origin: f64,
    duration: f64,
    paused: bool,
    seen_playing: bool,
}
type Prefetch = (
    String,
    Arc<AtomicBool>,
    tokio::task::JoinHandle<Result<CastFile, ApiError>>,
);

async fn play_session(
    context: &Arc<HostContext>,
    id: u64,
    renderer: Renderer,
    diagnostic: (&SessionTrace, u64),
    cancel: &CancellationToken,
    stop: &Arc<AtomicBool>,
    room: &mut Option<purek::Session>,
) -> Result<(), ApiError> {
    let (trace, connection) = diagnostic;
    trace.renderer(&renderer);
    let mode = with_app(|app| {
        if app.native().cast.operation != id {
            return Err(error("cancelled"));
        }
        let key = &renderer.context.device_key;
        let mut device = app
            .cast_preferences()?
            .devices
            .get(key)
            .cloned()
            .unwrap_or_default();
        let before = device.clone();
        device.label = renderer
            .description
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect();
        device.vendor_class = match renderer.description.vendor_class {
            crate::dlna::dlna_policy::VendorClass::Generic => {
                crate::app_state::cast::Vendor::Generic
            }
            crate::dlna::dlna_policy::VendorClass::Lebo => crate::app_state::cast::Vendor::Lebo,
            crate::dlna::dlna_policy::VendorClass::Dolphinstar => {
                crate::app_state::cast::Vendor::Dolphinstar
            }
        };
        if before != device || !app.cast_preferences()?.devices.contains_key(key) {
            let revision = app.native_core_snapshot()?.revision;
            app.cast_set_device(key, revision, device)?;
        }
        let mode = app
            .cast_preferences()?
            .devices
            .get(&renderer.context.device_key)
            .cloned()
            .unwrap_or_default()
            .effective_mode();
        let state = &mut app.native().cast;
        if state.operation != id {
            return Err(error("cancelled"));
        }
        state.selected = Some(renderer.context.device_key.clone());
        state.mode = mode;
        state.resume = None;
        Ok(mode)
    })?;
    let client = make_control(renderer.clone()).map_err(|e| error(e.code.as_str()))?;
    let address = url::Url::parse(&renderer.description.location)
        .ok()
        .and_then(|u| u.host_str()?.parse().ok())
        .ok_or_else(|| error("cast_renderer_unavailable"))?;
    #[cfg(test)]
    let test_endpoint = if renderer.local_address.is_loopback() {
        Some(crate::dlna::cast_http::diagnostic_fixture::bind(renderer.context.clone()).await)
    } else {
        None
    };
    #[cfg(not(test))]
    let test_endpoint: Option<CastEndpoint> = None;
    let endpoint = match test_endpoint {
        Some(e) => e,
        None => CastEndpoint::bind(
            address,
            renderer.context.clone(),
            ServerIdentity::new(std::env::consts::OS, env!("CARGO_PKG_VERSION"))
                .map_err(|_| error("cast_endpoint"))?,
        )
        .await
        .map_err(|_| error("cast_endpoint"))?,
    };
    let files = match CastSession::new(&context.cache_root, media::MAX_BYTES) {
        Ok(files) => Arc::new(files),
        Err(_) => {
            endpoint.shutdown().await;
            return Err(error("cast_prepare_failed"));
        }
    };
    let mut events = endpoint.subscribe();
    let mut output: Option<Output> = None;
    let mut prefetched: Option<Prefetch> = None;
    let result = async {
        let position = with_app(|app| Ok(app.native().cast.position))?;
        output = Some(
            start_output(
                OutputContext {
                    context,
                    id,
                    client: &client,
                    endpoint: &endpoint,
                    files: files.clone(),
                    trace,
                    connection,
                    cancel,
                    stop,
                },
                mode,
                position,
                None,
            )
            .await?,
        );
        let mut monitor = TransportMonitor::default();
        let mut position_enabled = true;
        let mut position_failures = 0u8;
        let mut next_poll = Instant::now();
        let mut recovery: Option<Instant> = None;
        let mut stopped = 0u8;
        let mut health_at = Instant::now();
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let out = output.as_mut().unwrap();
            if let Some(live) = &mut out.live {
                live.check().await?;
            }
            while let Ok(event) = events.try_recv() {
                if event.generation == Some(out.generation) {
                    trace.http_at(out.id, trace.elapsed(), &event);
                    if !position_enabled {
                        live_progress(id, out, &event);
                    }
                }
            }
            let (snap, control, mode) = with_app(|app| {
                Ok((
                    app.native_cast_input()?,
                    app.native().cast.control.take(),
                    app.native().cast.mode,
                ))
            })?;
            if snap.current_item.as_ref().is_none_or(|i| {
                i.item_incarnation_id != out.item.item_incarnation_id
                    || i.selected_audio_variant_id != out.item.selected_audio_variant_id
            }) || snap.playback_generation != out.playback_generation
            {
                return Err(error("cast_program_changed"));
            }
            if let Some(control) = control {
                match control {
                    Control::Pause if out.live.is_some() => {
                        let _ = record(trace, connection, Some(out.id), client.stop(cancel).await);
                        if let Some(live) = out.live.take() {
                            live.stop().await;
                        }
                        endpoint.clear_generation();
                        out.paused = true;
                        update(id, |s| {
                            s.paused = true;
                            s.receiver_state = Some("paused");
                        });
                    }
                    Control::Pause | Control::Play if mode == Mode::File => {
                        let pause = matches!(control, Control::Pause);
                        let action = if pause {
                            client.pause(cancel).await
                        } else {
                            client.play(cancel).await
                        };
                        match record(trace, connection, Some(out.id), action) {
                            Ok(()) => {
                                out.paused = pause;
                                out.start = with_app(|app| Ok(app.native().cast.position))?;
                                out.began = Instant::now();
                                update(id, |s| {
                                    s.paused = pause;
                                    s.receiver_state =
                                        Some(if pause { "paused" } else { "playing" });
                                });
                            }
                            Err(e) => {
                                update(id, |s| s.error_code = Some(e.code));
                            }
                        }
                    }
                    Control::Seek(position) => {
                        match record(
                            trace,
                            connection,
                            Some(out.id),
                            client.seek(Duration::from_secs_f64(position), cancel).await,
                        ) {
                            Ok(()) => {
                                out.start = position;
                                out.began = Instant::now();
                                update(id, |s| s.position = position);
                            }
                            Err(_) => {
                                update(id, |s| s.error_code = Some("cast_seek_unsupported".into()));
                            }
                        }
                    }
                    Control::Next | Control::Restart | Control::Play | Control::Pause => {
                        let next = matches!(control, Control::Next);
                        let retain_pause = matches!(control, Control::Restart) && out.paused;
                        let position = if next {
                            advance(id, out.playback_generation)?;
                            0.0
                        } else {
                            with_app(|app| Ok(app.native().cast.position))?
                        };
                        if let Some(live) = out.live.take() {
                            live.stop().await;
                        }
                        trace.complete_output(out.id, Default::default());
                        endpoint.clear_generation();
                        if !next && let Some((_, stopping, _)) = &prefetched {
                            stopping.store(true, Ordering::Release);
                        }
                        let cached = take_prefetch(&mut prefetched, cancel).await;
                        output = Some(
                            start_output(
                                OutputContext {
                                    context,
                                    id,
                                    client: &client,
                                    endpoint: &endpoint,
                                    files: files.clone(),
                                    trace,
                                    connection,
                                    cancel,
                                    stop,
                                },
                                mode,
                                position,
                                if next { cached } else { None },
                            )
                            .await?,
                        );
                        if retain_pause {
                            update(id, |s| {
                                s.control = Some(Control::Pause);
                                s.control_busy = true;
                            });
                        }
                        monitor = TransportMonitor::default();
                        position_enabled = true;
                        position_failures = 0;
                        stopped = 0;
                        recovery = None;
                    }
                }
                update(id, |s| s.control_busy = s.control.is_some());
                continue;
            }
            if prefetched.is_none()
                && mode == Mode::File
                && let Some(item) = snap
                    .next_item
                    .as_ref()
                    .filter(|i| i.cache_status == "ready")
            {
                let pre_stop = Arc::new(AtomicBool::new(false));
                prefetched = Some((
                    media_key(item),
                    pre_stop.clone(),
                    media::file(context.clone(), files.clone(), item.clone(), 0.0, pre_stop),
                ));
            }
            if Instant::now() >= next_poll {
                next_poll = Instant::now() + Duration::from_secs(2);
                let sample = monitor.poll(&client, cancel).await;
                if let Some(op) = sample.operation {
                    trace.record_operation(Some(connection), Some(out.id), &op);
                }
                let old_position = with_app(|app| Ok(app.native().cast.position))?;
                if position_enabled {
                    let op = client.get_position_info(cancel).await;
                    trace.record_operation(Some(connection), Some(out.id), &op);
                    match op.result {
                        Ok(info) => {
                            position_failures = 0;
                            update(id, |s| {
                                if let Some(d) = info.duration.filter(|d| !d.is_zero()) {
                                    s.duration = d.as_secs_f64();
                                }
                                if let Some(p) = info.position.filter(|p| {
                                    !p.is_zero()
                                        || !matches!(
                                            sample.state,
                                            Some(TransportState::Stopped | TransportState::NoMedia)
                                        )
                                }) {
                                    s.position = p.as_secs_f64()
                                        + if mode == Mode::Live {
                                            out.receiver_origin
                                        } else {
                                            0.0
                                        };
                                }
                            });
                        }
                        Err(e) if e.is_network() => {
                            position_failures = position_failures.saturating_add(1)
                        }
                        Err(e) if !e.is_cancelled() => {
                            position_enabled = false;
                            position_failures = 0;
                        }
                        _ => {}
                    }
                }
                if !position_enabled && !out.paused {
                    update(id, |s| {
                        s.position = bounded_position(
                            out.start + out.began.elapsed().as_secs_f64(),
                            s.duration,
                        );
                    });
                }
                if let Some(state) = sample.state {
                    // Hardware controls are observations too: the receiver, not
                    // the last submitted API command, owns the pause state.
                    if matches!(state, TransportState::Playing | TransportState::Paused)
                        && out.paused != (state == TransportState::Paused)
                    {
                        out.paused = state == TransportState::Paused;
                        out.start = with_app(|app| Ok(app.native().cast.position))?;
                        out.began = Instant::now();
                        update(id, |s| s.paused = out.paused);
                    }
                    trace.transport(
                        out.id,
                        state,
                        Some(with_app(|app| Ok(app.native().cast.position))? > old_position),
                    );
                    update(id, |s| {
                        s.receiver_state = Some(match state {
                            TransportState::Playing => "playing",
                            TransportState::Paused => "paused",
                            TransportState::Stopped => "stopped",
                            TransportState::NoMedia => "no_media",
                            TransportState::Transitioning => "transitioning",
                            TransportState::Unknown => "unknown",
                        })
                    });
                    if state == TransportState::Playing {
                        out.seen_playing = true;
                        stopped = 0;
                    }
                    if !out.paused
                        && matches!(state, TransportState::Stopped | TransportState::NoMedia)
                    {
                        let (position, duration) = with_app(|app| {
                            Ok((app.native().cast.position, app.native().cast.duration))
                        })?;
                        if out.seen_playing
                            && duration > 0.0
                            && position.max(old_position) >= duration - 3.0
                        {
                            update(id, |s| {
                                s.control = Some(Control::Next);
                                s.control_busy = true;
                            });
                            continue;
                        }
                        stopped = stopped.saturating_add(1);
                    }
                }
                let failed = sample.decision == MonitorDecision::Disconnected
                    || stopped >= 3
                    || position_failures >= 3;
                if failed {
                    let since = recovery.get_or_insert_with(Instant::now);
                    update(id, |s| {
                        s.phase = Phase::Recovering;
                        s.error_code = Some("cast_receiver_unreachable".into());
                    });
                    if since.elapsed() >= Duration::from_secs(4) {
                        return Err(error("cast_receiver_unreachable"));
                    }
                } else if sample.decision != MonitorDecision::Disconnected
                    && stopped == 0
                    && position_failures == 0
                {
                    recovery = None;
                    update(id, |s| {
                        s.phase = Phase::Active;
                        s.error_code = None;
                    });
                }
                with_app(|app| app.native_cast_observation(id, out.playback_generation))?;
                if endpoint.diagnostics().bytes_written == 0
                    && out.began.elapsed() > Duration::from_secs(10)
                {
                    update(id, |s| {
                        s.error_code = Some(
                            if mode == Mode::File {
                                "cast_retry_live"
                            } else {
                                "cast_not_requested"
                            }
                            .into(),
                        )
                    });
                }
            }
            if let Some(room) = room.as_mut() {
                room.record_delivery(endpoint.diagnostics().bytes_written);
                if Instant::now() >= health_at {
                    if let Some(stage) = room.renew_announcement(cancel).await {
                        trace.purek(connection, &stage);
                    }
                    let health = room.poll_health(cancel).await;
                    trace.record_purek_operation(connection, &health);
                    health_at = Instant::now() + room.health_interval();
                    if !matches!(
                        health.result,
                        Ok(purek::HealthState::Online | purek::HealthState::Suspect)
                    ) {
                        return Err(error("cast_purek_unavailable"));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        Ok(())
    }
    .await;
    stop.store(true, Ordering::Release);
    if let Some((_, stop, _)) = &prefetched {
        stop.store(true, Ordering::Release);
    }
    let cleanup_resources = async {
        let _ = take_prefetch(&mut prefetched, cancel).await;
        if let Some(out) = &mut output {
            if let Some(live) = out.live.take() {
                live.stop().await;
            }
            trace.complete_output(out.id, Default::default());
        }
        endpoint.clear_generation();
        // A cancelled operation cannot leave a long SOAP Stop behind. Release has
        // its own bounded protocol timeout and runs concurrently below.
        let cleanup = CancellationToken::new();
        let timeout = cleanup.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            timeout.cancel();
        });
        let _ = record(trace, connection, None, client.stop(&cleanup).await);
        timer.abort();
        let _ = timer.await;
        let _ = endpoint.shutdown().await;
    };
    // Revoke immediately, concurrently with HTTP/producer cleanup. Do not stack
    // our Stop timeout in front of T9's independent three-second release budget.
    let release_room = async {
        if let Some(room) = room.as_mut() {
            let released = room.release().await;
            trace.record_purek_operation(connection, &released);
        }
    };
    tokio::join!(cleanup_resources, release_room);
    result
}

async fn take_prefetch(
    prefetch: &mut Option<Prefetch>,
    cancel: &CancellationToken,
) -> Option<(String, CastFile)> {
    let (key, stop, mut task) = prefetch.take()?;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            stop.store(true, Ordering::Release);
            task.await
        },
        result = &mut task => result,
    };
    result.ok()?.ok().map(|file| (key, file))
}
fn advance(id: u64, generation: u64) -> Result<(), ApiError> {
    with_app(|app| {
        if app.native().cast.operation != id {
            return Err(error("cancelled"));
        }
        app.native().cast.worker_mutation = true;
        let result = app.native_execute(AppStateRequest::AdvanceToNext {
            schema_version: 1,
            expected_playback_generation: generation,
            reset_av_delay: false,
            now: now(),
        });
        app.native().cast.worker_mutation = false;
        result.map(|_| ())
    })
}

struct OutputContext<'a> {
    context: &'a Arc<HostContext>,
    id: u64,
    client: &'a ControlClient,
    endpoint: &'a CastEndpoint,
    files: Arc<CastSession>,
    trace: &'a SessionTrace,
    connection: u64,
    cancel: &'a CancellationToken,
    stop: &'a Arc<AtomicBool>,
}
async fn start_output(
    resources: OutputContext<'_>,
    mode: Mode,
    position: f64,
    cached: Option<(String, CastFile)>,
) -> Result<Output, ApiError> {
    let OutputContext {
        context,
        id,
        client,
        endpoint,
        files,
        trace,
        connection,
        cancel,
        stop,
    } = resources;
    if cancel.is_cancelled() {
        return Err(error("cancelled"));
    }
    let snap = with_app(|app| app.native_cast_input())?;
    let item = snap
        .current_item
        .ok_or_else(|| error("cast_queue_finished"))?;
    let generation = update(id, |s| {
        s.generation += 1;
        s.playback_generation = snap.playback_generation;
        s.phase = Phase::Connecting;
        s.position = position;
        s.generation
    })
    .ok_or_else(|| error("cancelled"))?;
    let output_id = trace.begin_output(
        &client.renderer().context,
        OutputConfig {
            cell: None,
            mode: if mode == Mode::Live {
                diagnostics::Mode::Live
            } else {
                diagnostics::Mode::File
            },
            mime: diagnostics::Mime::Ts,
            preflight: Default::default(),
        },
    );
    for event in endpoint
        .diagnostics()
        .stages
        .iter()
        .filter(|e| e.stage == crate::dlna::cast_http::HttpStage::Listener)
    {
        trace.http_at(output_id, trace.elapsed(), event);
    }
    let mut live = None;
    let duration;
    let published = if mode == Mode::Live {
        let producer = media::live(context.clone(), item.clone(), position).await?;
        let publication = endpoint.publish_live(generation, producer.stream.clone(), TsMime::Mp2t);
        match publication {
            Ok(p) => {
                live = Some(producer);
                duration = item.selected_durations.first().copied().unwrap_or(0) as f64;
                p
            }
            Err(_) => {
                producer.stop().await;
                return Err(error("cast_publish_failed"));
            }
        }
    } else {
        let file = match cached.filter(|(key, _)| *key == media_key(&item) && position == 0.0) {
            Some((_, file)) => file,
            None => media::file(context.clone(), files, item.clone(), 0.0, stop.clone())
                .await
                .map_err(|_| error("cast_prepare_failed"))??,
        };
        duration = file.summary.duration_us as f64 / 1_000_000.0
            + file.summary.source_start_us as f64 / 1_000_000.0;
        endpoint
            .publish_file(generation, file, TsMime::Mp2t)
            .map_err(|_| error("cast_publish_failed"))?
    };
    let mut started = record(
        trace,
        connection,
        Some(output_id),
        client
            .start(
                StartRequest {
                    uri: &published.uri,
                    title: &item.display_title,
                    file: published.length.map(|size| FileMetadata {
                        size,
                        duration: Duration::from_secs_f64(duration.max(0.0)),
                    }),
                    capabilities: CapabilityQuery::Query,
                },
                cancel,
            )
            .await,
    );
    if started.is_ok()
        && mode == Mode::File
        && position > 0.0
        && let Err(e) = record(
            trace,
            connection,
            Some(output_id),
            client.seek(Duration::from_secs_f64(position), cancel).await,
        )
    {
        started = Err(e);
    }
    if let Err(e) = started {
        if let Some(producer) = live {
            producer.stop().await;
        }
        endpoint.clear_generation();
        return Err(if mode == Mode::File && !cancel.is_cancelled() {
            error("cast_retry_live")
        } else {
            e
        });
    }
    if cancel.is_cancelled() {
        if let Some(producer) = live {
            producer.stop().await;
        }
        return Err(error("cancelled"));
    }
    let committed = with_app(|app| {
        if app.native().cast.operation != id {
            return Err(error("cancelled"));
        }
        app.native_execute(AppStateRequest::MarkCurrentItemStarted {
            schema_version: 1,
            item_id: item.id.clone(),
            now: now(),
        })?;
        Ok(())
    });
    if let Err(e) = committed {
        if let Some(producer) = live {
            producer.stop().await;
        }
        endpoint.clear_generation();
        return Err(e);
    }
    update(id, |s| {
        s.phase = Phase::Active;
        s.owns_playback = true;
        s.paused = false;
        s.duration = duration;
        s.receiver_state = Some("playing");
        s.error_code = None;
    });
    Ok(Output {
        id: output_id,
        generation,
        item,
        playback_generation: snap.playback_generation,
        live,
        began: Instant::now(),
        start: position,
        receiver_origin: position,
        duration,
        paused: false,
        seen_playing: false,
    })
}

fn live_progress(id: u64, out: &Output, event: &HttpStageResult) {
    if out.live.is_none() || out.paused {
        return;
    }
    if let (Some(start), Some(dts)) = (event.live_start, event.media_time_us) {
        let position =
            out.start + presentation_elapsed(start, dts).min(out.began.elapsed().as_secs_f64());
        update(id, |s| {
            s.position = bounded_position(position, out.duration)
        });
    }
}

fn bounded_position(position: f64, duration: f64) -> f64 {
    if duration > 0.0 {
        position.clamp(0.0, duration)
    } else {
        position.max(0.0)
    }
}

fn presentation_elapsed(start: crate::experimental_libav::cast_mux::CastStart, dts: i64) -> f64 {
    // T4 media_time_us is a DTS watermark relative to timestamp_origin_us.
    // With B frames its PTS is about 66.7ms later (two 30fps frames). Convert
    // back to source PTS, then subtract the actual keyframe source start;
    // the caller also caps against wall-clock playback, excluding the 5s burst.
    let pts = start
        .timestamp_origin_us
        .saturating_add(dts)
        .saturating_add(if start.has_b_frames { 66_667 } else { 0 });
    pts.saturating_sub(start.source_start_us).max(0) as f64 / 1_000_000.0
}

fn make_control(renderer: Renderer) -> Result<ControlClient, crate::dlna::DlnaError> {
    #[cfg(test)]
    if renderer.local_address.is_loopback() {
        return crate::dlna::session_fixture::control(renderer);
    }
    ControlClient::new(renderer)
}

async fn connect_room(
    qr: &str,
    confirmed: bool,
    cancel: &CancellationToken,
) -> purek::PureKOperation<purek::Session> {
    #[cfg(test)]
    if std::env::var_os("BILIKARA_TEST_CAST_COMPANION").is_some() {
        return purek::diagnostic_fixture::connect(qr).await;
    }
    purek::PureKClient::new()
        .connect(
            qr,
            if confirmed {
                purek::AddressApproval::ConfirmHostname
            } else {
                purek::AddressApproval::LanLiteralOnly
            },
            cancel,
        )
        .await
}

fn media_key(item: &PlaylistItem) -> String {
    format!(
        "{}:{}:{}",
        item.item_incarnation_id, item.artifact_set_id, item.selected_audio_variant_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dts_watermark_uses_source_origin_b_frame_offset_and_duration_bound() {
        let mut start = crate::experimental_libav::cast_mux::CastStart {
            source_start_us: 10_000_000,
            timestamp_origin_us: 9_000_000,
            profile: 100,
            level: 40,
            width: 1920,
            height: 1080,
            has_b_frames: true,
            audio_transcoded: false,
        };
        assert!((presentation_elapsed(start, 1_933_333) - 1.0).abs() < 0.000_001);
        start.has_b_frames = false;
        assert!((presentation_elapsed(start, 1_933_333) - 0.933_333).abs() < 0.000_001);
        assert_eq!(presentation_elapsed(start, 0), 0.0);
        assert_eq!(bounded_position(125.0, 120.0), 120.0);
        assert_eq!(bounded_position(125.0, 0.0), 125.0);
    }
}
