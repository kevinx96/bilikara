//! One joined worker owns network resources; all observable state lives in AppState.
use super::*;
use crate::app_state::cast::{LearnedMode, Mode, Variant};
use crate::app_state::native_session::text;
use crate::dlna::{CancellationToken, Renderer, diagnostics::SessionTrace};
use std::sync::Arc;

mod media;
pub(super) mod self_test;
mod worker;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    #[default]
    Idle,
    Discovering,
    Connecting,
    Active,
    Recovering,
}

pub(crate) enum Request {
    Discover(Option<String>),
    Connect {
        output: crate::cast_platform::OutputLease,
        renderer: Option<Box<Renderer>>,
        qr: Option<String>,
        confirmed: bool,
    },
    Disconnect,
}
#[derive(Clone, Debug)]
pub(crate) enum Control {
    Pause,
    Play,
    Next,
    Seek(f64),
    Restart,
    ProgramChanged,
}

#[derive(Default)]
pub(crate) struct State {
    pub network_facts: crate::cast_platform::NetworkFacts,
    pub platform_output_id: Option<u64>,
    pub platform_stop_error: Option<&'static str>,
    pub self_test: self_test::State,
    pub phase: Phase,
    pub owns_playback: bool,
    pub operation: u64,
    pub generation: u64,
    pub playback_generation: u64,
    pub renderers: Vec<Renderer>,
    pub selected: Option<String>,
    pub session_devices: std::collections::BTreeMap<String, crate::app_state::cast::Device>,
    pub receiver_state: Option<&'static str>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub error_code: Option<String>,
    pub resume: Option<Value>,
    pub mode: Mode,
    pub pending: Option<Request>,
    pub control: Option<Control>,
    pub control_busy: bool,
    pub cancel: CancellationToken,
    pub mux_stop: Arc<AtomicBool>,
    pub trace: Option<Arc<SessionTrace>>,
    pub worker_started: bool,
    pub self_test_handoff_active: bool,
    pub worker_mutation: bool,
}
impl State {
    pub fn visible(&self) -> bool {
        self.operation != 0 || !self.session_devices.is_empty() || self.self_test.generation != 0
    }
    pub fn snapshot(&self, preferences: &crate::app_state::cast::Preferences) -> Value {
        let preference = self
            .selected
            .as_ref()
            .and_then(|k| {
                preferences
                    .devices
                    .get(k)
                    .or_else(|| self.session_devices.get(k))
            })
            .cloned()
            .unwrap_or_default();
        json!({"phase":self.phase,"owns_playback":self.owns_playback,"self_test":self.self_test.snapshot(),
            "network_hint":self.network_facts.network_hint(),"network_facts":self.network_facts,
            "operation_id":self.operation,"generation":self.generation,
            "renderers":self.renderers.iter().map(|r| json!({"device_key":renderer_key(r),"preferences_persisted":preference_key(r).is_some(),"label":r.description.name.chars().take(100).collect::<String>()})).collect::<Vec<_>>(),
            "devices":preferences.devices.iter().chain(self.session_devices.iter()).map(|(key,d)|(key.clone(),json!({"mode":d.mode,"learned_mode":d.learned_mode}))).collect::<serde_json::Map<_,_>>(),
            "selected":self.selected,"receiver_state":self.receiver_state,"position":self.position,
            "duration":self.duration,"is_paused":self.paused,"error_code":self.error_code,"resume":self.resume,
            "mode":preference.mode,"learned_mode":preference.learned_mode,"effective_mode":self.mode,
            "busy":self.control_busy || self.pending.is_some(),"local_media":if self.owns_playback {"retain_paused_muted"} else {"host"}})
    }
    fn replace(&mut self, request: Request, phase: Phase) {
        self.cancel.cancel();
        self.mux_stop.store(true, Ordering::Release);
        self.operation += 1;
        self.pending = Some(request);
        self.phase = phase;
        self.control = None;
        self.control_busy = false;
        self.error_code = None;
        self.resume = None;
    }
}

/// Persistence identity is independent of T2 URL fallbacks and T9 trace salts.
fn preference_key(renderer: &Renderer) -> Option<String> {
    use sha2::{Digest, Sha256};
    let udn = crate::dlna::dlna_policy::canonical_usn(&renderer.description.id);
    if !udn.starts_with("uuid:")
        || udn.len() <= 5
        || udn == renderer.description.location.to_ascii_lowercase()
        || udn.starts_with("http:")
        || udn.starts_with("https:")
    {
        return None;
    }
    Some(
        Sha256::digest(udn.as_bytes())[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}
fn renderer_key(renderer: &Renderer) -> String {
    preference_key(renderer).unwrap_or_else(|| renderer.context.device_key.clone())
}

pub(super) fn route(
    context: &Arc<HostContext>,
    identity: &Identity,
    method: &Method,
    path: &str,
    body: &Value,
) -> Result<Value, ApiError> {
    if path.starts_with("/api/cast/self-test/") {
        return self_test::route(context, identity, method, path, body);
    }
    with_app(|app| {
        app.native_authorize(identity, path != "/api/cast/command")?;
        if path == "/api/cast/report" && method == Method::GET {
            return app
                .native()
                .cast
                .trace
                .as_ref()
                .map(|t| {
                    let report = t.report();
                    json!({"diagnostics":report.diagnostics(),"text":report.text()})
                })
                .ok_or_else(|| {
                    ApiError::new(404, "cast_report_unavailable", "No cast report yet")
                });
        }
        if method != Method::POST || !body.is_object() {
            return Err(ApiError::invalid("Cast requires POST JSON"));
        }
        if app.native().cast.self_test.running {
            return Err(ApiError::new(
                409,
                "cast_busy",
                "Wait for compatibility testing to finish",
            ));
        }
        if app.native_session_choice_pending() {
            return Err(ApiError::new(
                409,
                "session_choice_pending",
                "Choose a session first",
            ));
        }
        if context.stop.load(Ordering::Acquire) {
            return Err(ApiError::new(503, "stopped", "Host stopped"));
        }
        if path == "/api/cast/command"
            && matches!(
                body["command"].as_str(),
                Some("cast.set_device_mode" | "cast.set_device_learning")
            )
        {
            app.native_authorize(identity, true)?;
            let key = text(body, "device_key")?;
            if !crate::app_state::cast::valid_key(&key) {
                return Err(ApiError::invalid("Invalid device key"));
            }
            let persistent = app.cast_preferences()?.devices.contains_key(&key)
                || app
                    .native()
                    .cast
                    .renderers
                    .iter()
                    .any(|r| preference_key(r).as_deref() == Some(&key));
            if !persistent
                && !app
                    .native()
                    .cast
                    .renderers
                    .iter()
                    .any(|r| renderer_key(r) == key)
            {
                return Err(ApiError::new(
                    409,
                    "cast_device_preferences_unavailable",
                    "Discover the receiver first",
                ));
            }
            let revision = body["revision"]
                .as_u64()
                .ok_or_else(|| ApiError::invalid("Missing revision"))?;
            let mut device = app
                .cast_preferences()?
                .devices
                .get(&key)
                .cloned()
                .or_else(|| app.native().cast.session_devices.get(&key).cloned())
                .unwrap_or_default();
            if body["command"] == "cast.set_device_mode" {
                device.mode = serde_json::from_value(body["mode"].clone())
                    .map_err(|_| ApiError::invalid("Invalid mode"))?;
            } else {
                device.learned_mode =
                    serde_json::from_value::<Option<LearnedMode>>(body["learned_mode"].clone())
                        .map_err(|_| ApiError::invalid("Invalid learned mode"))?;
                device.learned_variant =
                    serde_json::from_value::<Option<Variant>>(body["learned_variant"].clone())
                        .map_err(|_| ApiError::invalid("Invalid learned variant"))?;
                let learned = device.learned_mode.is_some() || device.learned_variant.is_some();
                device.learned_at = learned.then(|| chrono::Utc::now().to_rfc3339());
                device.self_test_report_version = learned.then_some(1);
            }
            device.last_used = chrono::Utc::now().timestamp_millis().max(0) as u64;
            let effective = device.effective_mode();
            if app.native().cast.owns_playback
                && app.native().cast.selected.as_deref() == Some(&key)
                && app.native().cast.control_busy
            {
                return Err(ApiError::new(
                    409,
                    "cast_busy",
                    "Wait for the current cast operation",
                ));
            }
            if persistent {
                app.cast_set_device(&key, revision, device)?;
            } else {
                app.cast_set_session_device(&key, revision, device)?;
            }
            let state = &mut app.native().cast;
            if state.owns_playback
                && state.selected.as_deref() == Some(&key)
                && state.mode != effective
            {
                state.mode = effective;
                state.control = Some(Control::Restart);
                state.control_busy = true;
            }
            return app.native_snapshot(true);
        }
        match path {
            "/api/cast/discover" => {
                if app.native().cast.owns_playback {
                    return Err(ApiError::new(
                        409,
                        "cast_busy",
                        "Disconnect before discovery",
                    ));
                }
                let location = body
                    .get("description_url")
                    .map(|_| text(body, "description_url"))
                    .transpose()?;
                if let Some(location) = &location {
                    crate::dlna::dlna_policy::lan_url(
                        location,
                        crate::dlna::dlna_policy::NetworkPolicy::Lan,
                    )
                    .map_err(|_| ApiError::invalid("Discovery requires a LAN IPv4 URL"))?;
                }
                app.native()
                    .cast
                    .replace(Request::Discover(location), Phase::Discovering);
                app.native().cast.network_facts = crate::cast_platform::network_facts();
            }
            "/api/cast/connect" => {
                let qr = body
                    .get("purek_qr")
                    .map(|_| text(body, "purek_qr"))
                    .transpose()?;
                let renderer = if qr.is_some() {
                    None
                } else {
                    let key = text(body, "device_key")?;
                    Some(Box::new(
                        app.native()
                            .cast
                            .renderers
                            .iter()
                            .find(|r| renderer_key(r) == key)
                            .cloned()
                            .ok_or_else(|| {
                                ApiError::new(
                                    409,
                                    "cast_renderer_unavailable",
                                    "Discover the receiver again",
                                )
                            })?,
                    ))
                };
                if app.native_core_snapshot()?.current_item.is_none() {
                    return Err(ApiError::new(
                        409,
                        "cast_no_media",
                        "Choose a cached song first",
                    ));
                }
                let snapshot = app.native_snapshot(true)?;
                let output = crate::cast_platform::OutputLease::acquire()
                    .map_err(|error| ApiError::new(409, error.code(), error.code()))?;
                app.native().cast.platform_output_id = Some(output.id());
                app.native().cast.platform_stop_error = None;
                app.native().cast.network_facts = crate::cast_platform::network_facts();
                if !app.native().cast.owns_playback {
                    app.native().cast.position = snapshot["player_status"]["current_time"]
                        .as_f64()
                        .unwrap_or(0.0);
                    app.native().cast.paused = snapshot["player_status"]["is_paused"]
                        .as_bool()
                        .unwrap_or(false);
                }
                app.native().cast.playback_generation =
                    snapshot["playback_generation"].as_u64().unwrap_or(0);
                app.native().cast.replace(
                    Request::Connect {
                        output,
                        renderer,
                        qr,
                        confirmed: body["address_confirmed"] == true,
                    },
                    Phase::Connecting,
                );
                app.native().cast.owns_playback = true;
            }
            "/api/cast/disconnect" => {
                app.native()
                    .cast
                    .replace(Request::Disconnect, Phase::Recovering);
            }
            "/api/cast/command" => {
                enqueue(app, body)?;
            }
            _ => return Err(ApiError::new(404, "not_found", "Unknown cast route")),
        }
        app.native().revision += 1;
        if !app.native().cast.worker_started {
            let ctx = context.clone();
            if context
                .spawn("dlna-session", move || worker::run(ctx))
                .is_err()
            {
                let input = app.native_cast_input()?;
                let state = &mut app.native().cast;
                state.pending = None;
                state.phase = Phase::Idle;
                state.owns_playback = false;
                state.platform_output_id = None;
                state.error_code = Some("cast_worker_unavailable".into());
                state.resume = Some(
                    json!({"item_id":input.current_item.as_ref().map(|i|&i.id),"playback_generation":input.playback_generation,"position":state.position,"is_paused":state.paused,"operation_id":state.operation}),
                );
                return Err(ApiError::new(
                    503,
                    "cast_worker_unavailable",
                    "Cannot start cast worker",
                ));
            }
            app.native().cast.worker_started = true;
        }
        Ok(json!({"accepted":true,"operation_id":app.native().cast.operation}))
    })
}

/// Fixed native callback. Never trust a stale service/notification to stop a
/// newer output. Both paths retain the existing awaited Pure K cleanup.
pub fn platform_stopped(lease_id: u64, reason: i32) {
    if !matches!(reason, 1..=3) {
        return;
    }
    let _ = with_app(|app| {
        let cast = &mut app.native().cast;
        if cast.platform_output_id != Some(lease_id) {
            return Ok(());
        }
        cast.platform_output_id = None;
        cast.platform_stop_error = (reason == 3).then_some("cast_platform_unavailable");
        if cast.self_test.running {
            cast.self_test.cancel.cancel();
        } else {
            cast.replace(Request::Disconnect, Phase::Recovering);
        }
        app.native().revision += 1;
        Ok(())
    });
}

pub(crate) fn enqueue(app: &mut crate::app_state::AppState, body: &Value) -> Result<(), ApiError> {
    let snapshot = app.native_core_snapshot()?;
    if body["playback_generation"].as_u64() != Some(snapshot.playback_generation) {
        return Err(ApiError::new(409, "stale_player", "Playback changed"));
    }
    let state = &mut app.native().cast;
    if !state.owns_playback || state.phase != Phase::Active {
        return Err(ApiError::new(409, "cast_not_active", "Cast is not active"));
    }
    if state.control_busy {
        return Err(ApiError::new(409, "cast_busy", "Cast command is pending"));
    }
    let action = body["action"]
        .as_str()
        .ok_or_else(|| ApiError::invalid("Missing action"))?;
    state.control = Some(match action {
        "pause" => Control::Pause,
        "play" | "resume" => Control::Play,
        "toggle-pause" => {
            if state.paused {
                Control::Play
            } else {
                Control::Pause
            }
        }
        "next" | "next-track" => Control::Next,
        "seek-absolute" | "seek-relative" => {
            if state.mode == Mode::Live {
                return Err(ApiError::new(
                    409,
                    "cast_seek_unsupported",
                    "Seeking live delivery is not supported in this version",
                ));
            }
            let v = if action == "seek-relative" {
                body["delta_seconds"].as_f64().map(|d| state.position + d)
            } else {
                body["target_seconds"].as_f64()
            };
            Control::Seek(
                v.filter(|v| v.is_finite() && v.abs() <= 86400.0)
                    .ok_or_else(|| ApiError::invalid("Invalid seek"))?
                    .max(0.0)
                    .min(state.duration.max(0.0)),
            )
        }
        _ => {
            return Err(ApiError::new(
                400,
                "cast_command_unsupported",
                "Unsupported cast command",
            ));
        }
    });
    state.control_busy = true;
    app.native().revision += 1;
    Ok(())
}

#[cfg(test)]
mod tests;
