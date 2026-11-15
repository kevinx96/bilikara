//! End-to-end self-test through actual T3/T4/T2/T9, with event-synchronized answers.
use super::*;
use crate::dlna::diagnostics::{Answer, UserAnswers, VideoAnswer};
use crate::native_host::cast_session::self_test;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn self_test_discovery_links_udn_evidence_not_unrelated_salted_keys() {
    let receiver = Receiver::new();
    let target = receiver.renderer();
    let mut found = target.clone();
    found.context.device_key = "independent-t2-salt".into();
    let devices = [found];
    let matched = self_test::runner::identity_match(&target, &devices).unwrap();
    let trace = crate::dlna::diagnostics::SessionTrace::new(
        crate::dlna::diagnostics::Platform::Windows,
        crate::dlna::diagnostics::TargetKind::Purek,
        "0.8.0",
        "11",
    )
    .unwrap();
    let connection = trace.begin_connection();
    trace.discovery_facts(connection, Duration::from_secs(15), Some(false), Some(true));
    trace.discovery_target_facts(connection, &target.context, None, Some(matched));
    let report = trace.report();
    let c = &report.diagnostics()["cast_self_test"]["discovery"]["connections"][0];
    assert_eq!(c["target_before_auth"], false);
    assert_eq!(c["target_after_auth"], true);
    assert!(c["matched_before_device_key"].is_null());
    assert!(c["matched_after_device_key"].is_string());
    assert_ne!(c["target_device_key"], c["matched_after_device_key"]);
    assert_eq!(
        report.diagnostics()["cast_self_test"]["conclusion"]["decision_row"],
        11
    );
    let mut other = target.clone();
    other.description.id = "uuid:another-device".into();
    assert!(self_test::runner::identity_match(&other, &devices).is_none());
    let mut missing = target;
    missing.description.id.clear();
    assert!(self_test::runner::identity_match(&missing, &[missing.clone()]).is_none());
    privacy(&report.json());
}

fn scenario(name: &str) -> Value {
    eprintln!("self-test scenario: {name}");
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    assert!(
        std::env::var_os("BILIKARA_TEST_CAST_COMPANION").is_some(),
        "Set BILIKARA_TEST_CAST_COMPANION to the same-build test companion"
    );
    let mut host = Host::new();
    if name == "long_a" {
        host.use_long_media();
    }
    if name == "bad_b" {
        let root = host.context.directory.join("bad-sample");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("sample-b.ts"), b"corrupt").unwrap();
        Arc::get_mut(&mut host.context).unwrap().sample_root = Some(root);
    }
    if matches!(name, "bad_a" | "bad_audio" | "no_cache") {
        let snap = with_app(|app| app.native_core_snapshot()).unwrap();
        for item in snap.current_item.iter().chain(&snap.playlist) {
            let path = host
                .context
                .cache_root
                .join(item.video_media_url.strip_prefix("/media/").unwrap());
            if name == "bad_audio" {
                std::fs::write(path.with_file_name("audio.m4a"), b"not audio").unwrap();
            } else if name == "bad_a" {
                std::fs::write(path, b"not a video").unwrap();
            } else {
                std::fs::remove_file(path).unwrap();
            }
        }
    }
    let receiver = Receiver::new();
    {
        let mut state = receiver.state.lock().unwrap();
        state.fault_on = match name {
            "stop_fault" => Some("Stop"),
            "set_fault" => Some("SetAVTransportURI"),
            "play_fault" => Some("Play"),
            "registration_rejected" => Some("SetDeviceAuthorization"),
            _ => None,
        };
        state.no_pull = name == "no_pull";
        state.clients = if name == "concurrent" { 5 } else { 1 };
        state.different_peer = name == "different_peer";
        state.reject_files = name == "live_only";
    }
    let renderer = if name == "empty" {
        None
    } else {
        let mut renderer = receiver.renderer();
        if name == "no_udn" {
            renderer.description.id.clear();
        }
        Some(renderer)
    };
    let qr = matches!(
        name,
        "registration_rejected" | "cancel" | "handoff" | "finish"
    )
    .then(|| receiver.qr());
    let cancel = CancellationToken::new();
    let (send, receive) = tokio::sync::mpsc::channel(2);
    let report = runtime().block_on(async {
        let run = self_test::tests::run(
            host.context.clone(),
            renderer,
            qr,
            cancel.clone(),
            send,
            receive,
            self_test::runner::Timing {
                scan_before: Duration::ZERO,
                scan_after: Duration::ZERO,
                playback: if name == "no_pull" {
                    Duration::from_millis(10_050)
                } else {
                    Duration::from_millis(250)
                },
                question_after: Duration::from_millis(100),
                answer_timeout: if name == "unanswered" {
                    Duration::from_millis(100)
                } else {
                    Duration::from_secs(30)
                },
                between: Duration::ZERO,
                poll: Duration::from_millis(50),
                handoff_timeout: Duration::from_secs(5),
            },
        );
        let answer = async {
            if name == "unanswered" {
                return;
            }
            loop {
                let state = self_test::tests::question().await;
                if state["running"] == false {
                    break;
                }
                if state["handoff_available"] == true {
                    self_test::route(&host.context, &host.identity, &Method::POST, "/api/cast/self-test/handoff", &json!({"generation":1,"start_casting":name=="handoff"})).unwrap();
                    if name == "handoff" {
                        tokio::time::timeout(Duration::from_secs(10), async {
                            loop {
                                if host.snapshot()["cast"]["phase"] == "active" { break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.unwrap();
                        assert_eq!(receiver.state.lock().unwrap().releases, 0);
                        assert_eq!(receiver.state.lock().unwrap().actions.iter().filter(|a| a.as_str()=="SetDeviceAuthorization").count(), 1);
                        receiver.release_hold.store(true, Ordering::Release);
                        host.post("/api/cast/disconnect", json!({})).unwrap();
                        tokio::time::timeout(Duration::from_secs(3), async {
                            loop {
                                if receiver.release_ready.load(Ordering::Acquire) { break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.unwrap();
                        let owns = host.snapshot()["cast"]["owns_playback"].clone();
                        receiver.release_hold.store(false, Ordering::Release);
                        assert_eq!(owns, true, "ordinary worker must await handed-off room cleanup");
                        break;
                    }
                    continue;
                }
                if name == "cancel" {
                    let start = Instant::now();
                    cancel.cancel();
                    loop {
                        let state = self_test::tests::question().await;
                        if state["running"] == false {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                    assert!(
                        start.elapsed() < Duration::from_secs(3),
                        "cancellation exceeded 3 seconds"
                    );
                    break;
                }
                let cell = state["cell"].as_str().unwrap();
                let yes = match name {
                    "no_pull" | "decode" => false,
                    "uniform_file" => cell == "file_b",
                    "uniform_live" => cell == "live_b",
                    "mp4" => cell == "file_mp4_a",
                    _ => true,
                };
                let answer = UserAnswers {
                    video: if yes {
                        VideoAnswer::Normal
                    } else {
                        VideoAnswer::Black
                    },
                    audio: if yes { Answer::Yes } else { Answer::No },
                    synchronized: Answer::Yes,
                };
                let body = json!({"generation":1,"cell":cell,"question_id":state["question_id"],"answers":answer});
                if name == "success" {
                    let mut stale = body.clone(); stale["question_id"] = json!(0);
                    assert_eq!(self_test::route(&host.context, &host.identity, &Method::POST, "/api/cast/self-test/answer", &stale).unwrap_err().code, "cast_self_test_stale");
                    self_test::tests::expired_answer(&host.context, &host.identity, &body);
                }
                self_test::route(
                    &host.context,
                    &host.identity,
                    &Method::POST,
                    "/api/cast/self-test/answer",
                    &body,
                )
                .unwrap();
            }
        };
        let (report, ()) = tokio::join!(run, answer);
        report
    });
    let value = report.diagnostics().clone();
    let result = &value["cast_self_test"];
    if name == "bad_audio" {
        assert_eq!(
            self_test::tests::mux_count(),
            1,
            "failed merge must not retry every cached song"
        );
    }
    if name == "long_a" {
        assert!(
            result["preflight"]["sample_a"]["synthesis_ms"]
                .as_u64()
                .unwrap()
                < 20_000
        );
        assert_eq!(self_test::tests::mux_count(), 1);
        assert!((19_000_000..20_000_000).contains(&self_test::tests::recorded_end_us()));
    }
    if matches!(
        name,
        "success" | "uniform_file" | "uniform_live" | "mp4" | "live_only"
    ) {
        let mp4 = result["cells"][4]["skip_reason"].is_null();
        assert_eq!(
            self_test::tests::mux_count(),
            if mp4 { 2 } else { 1 },
            "{name}: original inputs are merged at most twice"
        );
    }
    if name == "mp4" {
        assert!(
            with_app(|app| Ok(app
                .cast_preferences()?
                .devices
                .values()
                .any(|d| d.learned_variant == Some(Variant::FileMp4))))
            .unwrap()
        );
    }
    assert_eq!(
        result["environment"]["companion_aac"], true,
        "test companion must execute"
    );
    if !matches!(name, "bad_a" | "bad_audio" | "no_cache") {
        assert!(
            result["preflight"]["sample_a"].is_object(),
            "{name}: A must actually synthesize"
        );
    }
    if name != "bad_b" {
        assert!(
            result["preflight"]["sample_b"].is_object(),
            "{name}: B must actually prepare"
        );
    }
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["steps"].as_array().unwrap().len(), 8);
    assert_eq!(result["cells"].as_array().unwrap().len(), 5);
    assert!(
        result["cells"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["terminal"] == true && c["stage"].is_string())
    );
    privacy(&report.json());
    privacy(report.text());
    assert!(
        report.text().lines().count() < 30,
        "self-test summary must stay one page"
    );
    if matches!(name, "success" | "no_udn") {
        let prefs = with_app(|app| app.cast_preferences().cloned()).unwrap();
        let state = host.snapshot();
        if name == "no_udn" {
            assert!(prefs.devices.is_empty());
            assert_eq!(state["cast"]["self_test"]["preferences_persisted"], false);
        } else {
            assert!(
                prefs
                    .devices
                    .values()
                    .any(|d| d.learned_mode == Some(LearnedMode::File)
                        && d.learned_variant == Some(Variant::Original))
            );
        }
        let read = self_test::route(
            &host.context,
            &host.identity,
            &Method::GET,
            "/api/cast/self-test/report",
            &json!({}),
        )
        .unwrap();
        assert_eq!(
            read["diagnostics"]["cast_self_test"]["conclusion"]["decision_row"],
            1
        );
        assert!(
            self_test::route(
                &host.context,
                &host.identity,
                &Method::POST,
                "/api/cast/self-test/answer",
                &json!({"generation":1,"cell":"file_a"})
            )
            .is_err()
        );
        let forbidden = Identity {
            token: "remote".into(),
            loopback: false,
            client: "remote".into(),
        };
        assert!(
            self_test::route(
                &host.context,
                &forbidden,
                &Method::GET,
                "/api/cast/self-test/report",
                &json!({})
            )
            .is_err()
        );
        assert!(self_test::download(&forbidden).is_err());
        let saved = runtime()
            .block_on(axum::body::to_bytes(
                self_test::download(&host.identity).unwrap().into_body(),
                1024 * 1024,
            ))
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&saved).unwrap()["diagnostics"]["cast_self_test"]["conclusion"]
                ["decision_row"],
            1
        );
        let zip =
            crate::native_host::diagnostics::package(&host.context, &host.identity, &json!({}))
                .unwrap();
        let zip = runtime()
            .block_on(axum::body::to_bytes(zip.into_body(), 16 * 1024 * 1024))
            .unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
        let mut found = false;
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).unwrap();
            if f.name().ends_with(".json") {
                let mut text = String::new();
                f.read_to_string(&mut text).unwrap();
                if text.contains("cast_self_test") {
                    found = true;
                    privacy(&text);
                }
            }
        }
        assert!(found, "diagnostic ZIP must include self-test JSON");
    }
    if matches!(name, "cancel" | "handoff" | "finish") {
        assert!(receiver.state.lock().unwrap().releases > 0);
        assert_eq!(result["metadata"]["cancelled"], name == "cancel");
    }
    value
}

fn privacy(text: &str) {
    use base64::{
        Engine,
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    };
    for secret in [
        "127.0.0.1",
        "127.0.0.2",
        "uuid:cast-fixture",
        "test-secret-token",
        "test-place",
        "test-bill",
        "Test Room",
        "projection_url",
        "conn.dolphinstar.cn",
    ] {
        for encoded in [
            secret.to_owned(),
            STANDARD.encode(secret),
            URL_SAFE_NO_PAD.encode(secret),
            url::form_urlencoded::byte_serialize(secret.as_bytes()).collect(),
        ] {
            assert!(!text.contains(&encoded), "report leaked {secret}");
        }
    }
}

#[test]
fn self_test_success_and_decision_rows_one_to_five() {
    for (name, row) in [
        ("success", 1),
        ("no_udn", 1),
        ("live_only", 2),
        ("uniform_file", 3),
        ("uniform_live", 4),
        ("mp4", 5),
        ("decode", 6),
    ] {
        let report = scenario(name);
        assert_eq!(
            report["cast_self_test"]["conclusion"]["decision_row"], row,
            "{name}: {report}"
        );
    }
}
#[test]
fn self_test_control_failures_are_independent_and_reports_complete() {
    for (name, row) in [
        ("stop_fault", 1),
        ("set_fault", 9),
        ("play_fault", 9),
        ("empty", 12),
        ("registration_rejected", 1),
    ] {
        let report = scenario(name);
        assert_eq!(
            report["cast_self_test"]["conclusion"]["decision_row"], row,
            "{name}: {report}"
        );
        if name == "registration_rejected" {
            assert!(
                report
                    .to_string()
                    .contains("cast_purek_authorization_rejected")
            );
        }
    }
}
#[test]
fn self_test_missing_bad_samples_and_unanswered_continue() {
    for name in ["bad_a", "bad_audio", "no_cache", "bad_b", "unanswered"] {
        let report = scenario(name);
        let cells = report["cast_self_test"]["cells"].as_array().unwrap();
        if name == "bad_b" {
            assert_eq!(cells[1]["skip_reason"], "sample_integrity_failed");
            assert_eq!(cells[3]["skip_reason"], "sample_integrity_failed");
        }
        if matches!(name, "bad_a" | "bad_audio" | "no_cache") {
            assert!(!cells[0]["skip_reason"].is_null());
            assert!(!cells[2]["skip_reason"].is_null());
        }
        if name == "unanswered" {
            assert!(cells.iter().all(|c| c["skip_reason"].is_null()));
            assert!(
                cells
                    .iter()
                    .all(|c| c["user_answers"]["video"] == "unanswered")
            );
            assert!(report["cast_self_test"]["conclusion"]["learned_mode"].is_null());
        }
    }
}
#[test]
fn self_test_pull_observations_preserve_peer_and_concurrency_facts() {
    for name in ["no_pull", "different_peer", "concurrent"] {
        let report = scenario(name);
        let cells = report["cast_self_test"]["cells"].as_array().unwrap();
        if name == "no_pull" {
            assert!(cells.iter().all(|c| c["http"]["get"] == 0));
            assert_eq!(report["cast_self_test"]["conclusion"]["decision_row"], 7);
        }
        if name == "different_peer" {
            assert_eq!(
                cells[1]["observations"]["source_matches_description"],
                false
            );
            assert!(cells[1]["observations"]["source_same_subnet_24"].is_null());
        }
        if name == "concurrent" {
            assert!(
                cells[1]["observations"]["over_client_limit"]
                    .as_u64()
                    .unwrap()
                    >= 1
            );
            assert!(cells[1]["observations"]["last_byte_ms"].as_u64().unwrap() > 0);
            assert!(
                cells[1]["observations"]["maximum_concurrent_get"]
                    .as_u64()
                    .unwrap()
                    >= 2
            );
        }
    }
}
#[test]
fn self_test_cancel_awaits_revocation_within_three_seconds() {
    scenario("cancel");
}

#[test]
fn self_test_handoff_keeps_registration_until_normal_session_ends() {
    scenario("handoff");
    scenario("finish");
}

#[test]
fn self_test_long_original_is_recorded_once_with_twenty_second_bound() {
    scenario("long_a");
}
