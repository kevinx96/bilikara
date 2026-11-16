use super::*;
use crate::cast_platform::{
    self, InitializationFailure,
    test_support::{self, RecordingPlatform},
};
use crate::dlna::discovery;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn platform_initialization_failure_keeps_host_running_and_reports_unavailable_s0() {
    let shell = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../src-tauri/src/android.rs"
    ));
    assert!(
        shell.contains("android_cast::initialize(app);"),
        "Android must not propagate optional cast startup failure"
    );
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for reason in [
        InitializationFailure::InitializationFailed,
        InitializationFailure::InitializationTimedOut,
    ] {
        test_support::scoped(|| {
            // Same optional boundary used by Android bootstrap; the actual native
            // listener and unrelated playlist operations must still work afterward.
            cast_platform::initialize_optional(|| Err(reason));
            crate::execute_app_state(AppStateRequest::Shutdown { schema_version: 1 });
            let seed = serde_json::from_value(json!({"session_users":["Alice"],"session_started_at":1,"session_played_file":"session.json","updated_at":1})).unwrap();
            assert!(
                crate::execute_app_state(AppStateRequest::Initialize {
                    schema_version: 1,
                    state: Box::new(seed)
                })
                .error()
                .is_none()
            );
            let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../.tmp")
                .join(format!("cast-bootstrap-{}", token().unwrap()));
            std::fs::create_dir_all(&directory).unwrap();
            let native = NativeHost::start(&directory, Arc::new(|_| None)).unwrap();
            drop(TcpStream::connect(("127.0.0.1", native.local_port())).unwrap());
            let identity = Identity {
                token: with_app(|app| Ok(app.native().host_token.clone())).unwrap(),
                loopback: true,
                client: "fixture".into(),
            };
            let host = Host {
                context: native.context.clone(),
                identity: identity.clone(),
            };
            host.song("after-platform-failure", 30);
            assert_eq!(
                host.snapshot()["current_item"]["id"],
                "after-platform-failure"
            );
            let post = |path, body| {
                api::dispatch(
                    &native.context,
                    &identity,
                    true,
                    &Method::POST,
                    path,
                    "",
                    body,
                )
            };
            let error = post(
                "/api/cast/connect",
                json!({"purek_qr":"invalid-but-not-yet-parsed"}),
            )
            .unwrap_err();
            assert_eq!(
                (error.status, error.code.as_str()),
                (409, "cast_platform_unavailable")
            );
            let error = post("/api/cast/self-test/start", json!({})).unwrap_err();
            assert_eq!(
                (error.status, error.code.as_str()),
                (409, "cast_platform_unavailable")
            );
            let report = api::dispatch(
                &native.context,
                &identity,
                true,
                &Method::GET,
                "/api/cast/self-test/report",
                "",
                json!({}),
            )
            .unwrap();
            let report = &report["diagnostics"]["cast_self_test"];
            assert_eq!(
                report["environment"]["android_network"]["platform_unavailable_reason"],
                serde_json::to_value(reason).unwrap()
            );
            assert!(report["environment"]["android_network"]["multicast_acquired"].is_null());
            assert_eq!(
                report["steps"],
                json!([{"stage":"s0","outcome":"failed","elapsed_ms":0}])
            );
            assert_eq!(host.snapshot()["cast"]["self_test"]["running"], false);
            assert_eq!(host.snapshot()["cast"]["owns_playback"], false);
            assert!(with_app(|app| Ok(app.native().cast.pending.is_none())).unwrap());
            // A callback arriving after timeout cannot change the latched failure.
            assert_eq!(
                cast_platform::install(Arc::new(RecordingPlatform::default())),
                Err(cast_platform::PlatformError::AlreadyInstalled)
            );
            assert_eq!(cast_platform::initialization_failure(), Some(reason));
            drop(native);
        });
    }
}

#[test]
fn platform_self_test_acquires_only_in_s1_and_s3_not_s0() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::new();
    let receiver = Receiver::new();
    test_support::scoped(|| {
        discovery::lease_tests::loopback(|_| {
            let platform = Arc::new(RecordingPlatform::with_begin(|| {
                with_app(|app| {
                    let test = &mut app.native().cast.self_test;
                    let stage = test.snapshot()["stage"].clone();
                    assert!(
                        stage == "s1" || stage == "s3",
                        "unexpected multicast lease in {stage}"
                    );
                    if stage == "s3" {
                        test.cancel.cancel();
                    }
                    Ok(())
                })
                .unwrap();
            }));
            cast_platform::install(platform.clone()).unwrap();
            let (send, receive) = tokio::sync::mpsc::channel(2);
            let report = runtime().block_on(self_test::tests::run(
                host.context.clone(),
                Some(receiver.renderer()),
                Some(receiver.qr()),
                CancellationToken::new(),
                send,
                receive,
                self_test::runner::Timing {
                    scan_before: Duration::from_millis(50),
                    scan_after: Duration::from_millis(50),
                    ..Default::default()
                },
            ));
            platform.assert_finished(2);
            assert!(
                platform.facts_reads.load(Ordering::Relaxed) >= 3,
                "S0/S1/S3 must exercise the installed platform path"
            );
            let report = report.diagnostics();
            let steps = report["cast_self_test"]["steps"].as_array().unwrap();
            assert_eq!(steps[0]["stage"], "s0");
            assert_eq!(steps[0]["outcome"], "complete");
            assert!(receiver.state.lock().unwrap().releases > 0);
        })
    });
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION; actual active playback after discovery"]
fn platform_active_handoff_does_not_hold_discovery_lease() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::new();
    let receiver = Receiver::new();
    let renderer = receiver.renderer();
    test_support::scoped(|| {
        discovery::lease_tests::loopback(|_| {
            let platform = Arc::new(RecordingPlatform::default());
            cast_platform::install(platform.clone()).unwrap();
            runtime().block_on(async {
                assert!(
                    discovery::discover(&CancellationToken::new(), |_| {})
                        .await
                        .result
                        .is_ok()
                );
                platform.assert_finished(1);
                let cancel = CancellationToken::new();
                let output = cast_platform::OutputLease::acquire().unwrap();
                let id = with_app(|app| Ok(app.native().cast.operation)).unwrap();
                let active = async {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        loop {
                            if host.snapshot()["cast"]["phase"] == "active" {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    platform.assert_finished(1);
                    cancel.cancel();
                };
                tokio::join!(
                    worker::handoff(&host.context, id, renderer, None, &cancel),
                    active
                );
                drop(output);
                platform.assert_finished(1);
            });
        })
    });
}
