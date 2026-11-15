use super::*;

fn discover(_host: &Host, receiver: &Receiver) -> String {
    let renderer = receiver.renderer();
    let key = renderer_key(&renderer);
    with_app(|app| {
        app.native().cast.renderers.push(renderer);
        Ok(())
    })
    .unwrap();
    key
}
fn mode(host: &Host, key: &str, mode: &str) {
    host.post("/api/cast/command", json!({"command":"cast.set_device_mode", "device_key":key, "mode":mode, "revision":host.snapshot()["revision"]})).unwrap();
}

#[test]
fn udn_identity_ignores_diagnostic_salt_and_no_udn_preferences_are_transient() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::new();
    let receiver = Receiver::new();
    let mut renderer = receiver.renderer();
    let expected = preference_key(&renderer).unwrap();
    renderer.description.id =
        " UUID:CAST-FIXTURE::urn:schemas-upnp-org:device:MediaRenderer:1 ".into();
    renderer.context.device_key = "0123456789abcdef0123456789abcdef".into();
    assert_eq!(preference_key(&renderer).unwrap(), expected);
    renderer.description.id = renderer.description.location.clone();
    assert!(preference_key(&renderer).is_none());
    let key = renderer_key(&renderer);
    with_app(|app| {
        app.native().cast.renderers.push(renderer);
        Ok(())
    })
    .unwrap();
    let request = json!({"command":"cast.set_device_mode", "device_key":key, "mode":"live", "revision":host.snapshot()["revision"]});
    let result = host.post("/api/cast/command", request).unwrap();
    assert_eq!(result["cast"]["devices"][&key]["mode"], "live");
    assert!(with_app(|app| Ok(app.cast_preferences()?.is_empty())).unwrap());
    assert_eq!(
        host.post(
            "/api/cast/discover",
            json!({"description_url":"http://8.8.8.8/description"})
        )
        .unwrap_err()
        .status,
        400
    );
    assert!(with_app(|app| Ok(app.cast_preferences()?.is_empty())).unwrap());
}

#[test]
fn learning_clear_removes_metadata_and_unknown_device_cannot_consume_capacity() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::new();
    let receiver = Receiver::new();
    let key = discover(&host, &receiver);
    for learned in [Some("live"), None] {
        host.post("/api/cast/command", json!({"command":"cast.set_device_learning", "device_key":key, "learned_mode":learned, "learned_variant":null, "revision":host.snapshot()["revision"]})).unwrap();
        with_app(|app| {
            let saved = &app.cast_preferences()?.devices[&key];
            assert_eq!(saved.learned_at.is_some(), learned.is_some());
            assert_eq!(saved.self_test_report_version.is_some(), learned.is_some());
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(host.post("/api/cast/command", json!({"command":"cast.set_device_mode", "device_key":"0123456789abcdef0123456789abcdef", "mode":"file", "revision":host.snapshot()["revision"]})).unwrap_err().code, "cast_device_preferences_unavailable");
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn purek_reconnect_reuses_preference_and_full_store_does_not_block_connect() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::new();
    let receiver = Receiver::new();
    let qr = receiver.qr();
    host.post("/api/cast/connect", json!({"purek_qr":qr}))
        .unwrap();
    let first = host.wait(|s| s["cast"]["phase"] == "active");
    let key = first["cast"]["selected"].as_str().unwrap().to_owned();
    assert_eq!(key, preference_key(&receiver.renderer()).unwrap());
    mode(&host, &key, "live");
    host.wait(|s| s["cast"]["phase"] == "active" && s["cast"]["busy"] == false);
    host.post("/api/cast/connect", json!({"purek_qr":qr}))
        .unwrap();
    let second = host.wait(|s| s["cast"]["phase"] == "active" && s["cast"]["operation_id"] == 2);
    assert_eq!(second["cast"]["selected"], key);
    assert_eq!(second["cast"]["effective_mode"], "live");
    assert_eq!(second["cast"]["devices"].as_object().unwrap().len(), 1);
    host.post("/api/cast/disconnect", json!({})).unwrap();
    host.wait(|s| s["cast"]["phase"] == "idle" && s["cast"]["busy"] == false);
    with_app(|app| {
        for index in 0..256 {
            app.cast_set_device(
                &format!("{index:032x}"),
                app.native_core_snapshot()?.revision,
                Default::default(),
            )?;
        }
        Ok(())
    })
    .unwrap();
    let mut renderer = receiver.renderer();
    renderer.description.id = "uuid:new-receiver".into();
    host.connect(renderer);
    assert_eq!(host.snapshot()["cast"]["owns_playback"], true);
    assert_eq!(
        host.snapshot()["cast"]["devices"]
            .as_object()
            .unwrap()
            .len(),
        256
    );
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn live_zero_missing_and_stagnant_position_advance_and_pause_is_idempotent() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for reading in ["zero", "missing", "stagnant"] {
        let host = Host::with_duration(8);
        let receiver = Receiver::new();
        let key = discover(&host, &receiver);
        mode(&host, &key, "live");
        {
            let mut state = receiver.state.lock().unwrap();
            state.duration = 0;
            state.position = 0;
            state.no_position = reading == "missing";
        }
        host.connect(receiver.renderer());
        host.command("pause");
        let generation = host.snapshot()["cast"]["generation"].clone();
        host.command("pause");
        assert_eq!(host.snapshot()["cast"]["generation"], generation);
        assert_eq!(host.snapshot()["cast"]["is_paused"], true);
        host.command("play");
        if reading == "stagnant" {
            receiver.state.lock().unwrap().position = 1;
        }
        host.wait(|s| s["cast"]["position"].as_f64().unwrap() >= 7.0);
        receiver.state.lock().unwrap().state = "STOPPED";
        let next =
            host.wait(|s| s["current_item"]["id"] == "song-two" && s["cast"]["phase"] == "active");
        assert_eq!(next["cast"]["owns_playback"], true);
        assert!(next["cast"]["error_code"].is_null());
    }
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn stopped_mid_song_after_pause_and_during_seek_never_advances() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for scenario in ["middle", "paused", "seek"] {
        let host = Host::new();
        let receiver = Receiver::new();
        host.connect(receiver.renderer());
        receiver.state.lock().unwrap().position = 40;
        host.wait(|s| s["cast"]["position"] == 40.0);
        if scenario == "paused" {
            host.command("pause");
        }
        if scenario == "seek" {
            host.post("/api/cast/command", json!({"action":"seek-absolute", "target_seconds":100, "playback_generation":host.snapshot()["playback_generation"]})).unwrap();
            host.wait(|s| s["cast"]["busy"] == false);
        }
        {
            let mut state = receiver.state.lock().unwrap();
            state.state = "STOPPED";
            state.position = if scenario == "paused" { 119 } else { 0 };
        }
        if scenario == "seek" {
            host.wait(|s| s["cast"]["receiver_state"] == "stopped");
            assert_eq!(host.snapshot()["current_item"]["id"], "song-one");
            receiver.state.lock().unwrap().state = "PLAYING";
            host.wait(|s| s["cast"]["receiver_state"] == "playing");
            assert_eq!(host.snapshot()["cast"]["phase"], "active");
        } else {
            host.wait(|s| {
                assert_eq!(
                    s["current_item"]["id"], "song-one",
                    "unexpected natural advance: {scenario}"
                );
                s["cast"]["phase"] == "recovering"
            });
            let snap = host.wait(|s| s["cast"]["phase"] == "idle");
            assert_eq!(snap["current_item"]["id"], "song-one");
            assert_eq!(snap["cast"]["owns_playback"], false);
            assert_eq!(
                snap["cast"]["resume"]["position"],
                if scenario == "paused" { 119.0 } else { 40.0 },
                "STOPPED must freeze the last receiver position throughout recovery"
            );
        }
    }
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn hung_poll_is_interrupted_by_command_and_recovers_within_deadline() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for delay in [1500, 2500] {
        slow_receiver_keeps_session_and_changes_program(delay);
    }
    let host = Host::new();
    let receiver = Receiver::new();
    host.connect(receiver.renderer());
    host.wait(|s| s["cast"]["duration"] == 120.0);
    let polls = {
        let mut state = receiver.state.lock().unwrap();
        state.hang = true;
        state
            .actions
            .iter()
            .filter(|a| *a == "GetTransportInfo")
            .count()
    };
    let began = Instant::now();
    host.wait(|_| {
        receiver
            .state
            .lock()
            .unwrap()
            .actions
            .iter()
            .filter(|a| *a == "GetTransportInfo")
            .count()
            > polls
    });
    let command = Instant::now();
    host.command("pause");
    assert!(command.elapsed() < Duration::from_millis(2600));
    host.wait(|s| s["cast"]["phase"] == "recovering");
    assert!(began.elapsed() < Duration::from_secs(10));
    host.wait(|s| s["cast"]["phase"] == "idle");
    assert!(began.elapsed() < Duration::from_secs(15));
    assert_eq!(
        host.snapshot()["cast"]["error_code"],
        "cast_receiver_unreachable"
    );
    drop(host);
    // A hung monitor must not delay Next's AppState transition or URI work.
    let host = Host::new();
    receiver.state.lock().unwrap().hang = false;
    host.connect(receiver.renderer());
    host.wait(|s| s["cast"]["duration"] == 120.0);
    let polls = {
        let mut state = receiver.state.lock().unwrap();
        state.hang = true;
        state
            .actions
            .iter()
            .filter(|a| *a == "GetTransportInfo")
            .count()
    };
    host.wait(|_| {
        receiver
            .state
            .lock()
            .unwrap()
            .actions
            .iter()
            .filter(|a| *a == "GetTransportInfo")
            .count()
            > polls
    });
    let began = Instant::now();
    host.post(
        "/api/cast/command",
        json!({"action":"next", "playback_generation":host.snapshot()["playback_generation"]}),
    )
    .unwrap();
    host.wait(|s| s["current_item"]["id"] == "song-two");
    assert!(
        began.elapsed() < Duration::from_millis(600),
        "Next waited for the hung poll"
    );
    let cancel = Instant::now();
    host.post("/api/cast/disconnect", json!({})).unwrap();
    host.wait(|s| s["cast"]["phase"] == "idle" && s["cast"]["busy"] == false);
    assert!(cancel.elapsed() < Duration::from_secs(3));
}

fn slow_receiver_keeps_session_and_changes_program(delay_ms: u64) {
    let host = Host::new();
    let receiver = Receiver::new();
    receiver.state.lock().unwrap().response_delay = Duration::from_millis(delay_ms);
    host.connect(receiver.renderer());
    // Three complete real SOAP poll pairs, not a sleep followed by one lucky
    // snapshot. Every observed snapshot must retain the session and ownership.
    host.wait(|s| {
        assert_eq!(s["cast"]["phase"], "active", "{delay_ms}ms: {}", s["cast"]);
        assert_eq!(s["cast"]["owns_playback"], true);
        receiver
            .state
            .lock()
            .unwrap()
            .completed
            .iter()
            .filter(|a| *a == "GetPositionInfo")
            .count()
            >= 3
    });
    assert_eq!(host.command("next")["current_item"]["id"], "song-two");
    host.wait(|s| s["cast"]["duration"] == 120.0);
    {
        let mut state = receiver.state.lock().unwrap();
        state.position = 119;
        state.state = "STOPPED";
    }
    let next = host.wait(|s| {
        assert_ne!(s["cast"]["phase"], "recovering");
        assert_ne!(s["cast"]["phase"], "idle");
        s["current_item"]["id"] == "song-three" && s["cast"]["phase"] == "active"
    });
    assert_eq!(next["cast"]["operation_id"], 1);
    assert!(next["cast"]["error_code"].is_null());
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn playlist_program_changes_keep_receiver_session() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for action in ["defer", "set", "front", "remove", "reset"] {
        let host = Host::new();
        let receiver = Receiver::new();
        host.connect(receiver.renderer());
        let before = with_app(|app| app.native_core_snapshot()).unwrap();
        let request = match action {
            "defer" => AppStateRequest::DeferCurrentItem {
                schema_version: 1,
                item_id: "song-one".into(),
                expected_item_incarnation_id: before.current_item.unwrap().item_incarnation_id,
                expected_playback_generation: before.playback_generation,
                expected_playlist_item_ids: before.playlist.iter().map(|i| i.id.clone()).collect(),
                target_index: 1,
                reset_av_delay: false,
                now: 4.0,
            },
            "set" => AppStateRequest::SetCurrentItem {
                schema_version: 1,
                item_id: Some("song-two".into()),
                reset_av_delay: false,
                now: 4.0,
            },
            "front" => AppStateRequest::MoveToFront {
                schema_version: 1,
                item_id: "song-two".into(),
                reset_av_delay: false,
                now: 4.0,
            },
            "reset" => AppStateRequest::ResetPlayer {
                schema_version: 1,
                now: 4.0,
            },
            _ => AppStateRequest::RemoveItem {
                schema_version: 1,
                item_id: "song-one".into(),
                now: 4.0,
            },
        };
        assert!(crate::execute_app_state(request).error().is_none());
        if action == "remove" {
            let snap = host.wait(|s| s["cast"]["phase"] == "idle");
            assert!(snap["current_item"].is_null());
            assert_eq!(snap["cast"]["error_code"], "cast_no_current_item");
            continue;
        }
        let snap = host.wait(|s| {
            s["cast"]["generation"].as_u64().unwrap() >= 2 && s["cast"]["phase"] == "active"
        });
        assert_eq!(
            snap["current_item"]["id"],
            if action == "reset" {
                "song-one"
            } else {
                "song-two"
            },
            "{action}"
        );
        assert_eq!(snap["cast"]["operation_id"], 1);
        assert_eq!(snap["cast"]["owns_playback"], true);
        assert!(snap["cast"]["resume"].is_null());
        assert!(
            receiver
                .state
                .lock()
                .unwrap()
                .actions
                .iter()
                .filter(|a| *a == "SetAVTransportURI")
                .count()
                >= 2
        );
    }
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn corrupt_preferences_do_not_block_cast_and_emit_safe_diagnostic() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::with_storage(120, true);
    let receiver = Receiver::new();
    host.connect(receiver.renderer());
    assert_eq!(host.snapshot()["cast"]["owns_playback"], true);
    assert!(host.snapshot()["cast"]["error_code"].is_null());
    assert_eq!(
        std::fs::read(host.context.directory.join("cast-devices.json")).unwrap(),
        b"{broken"
    );
    let diagnostics = with_app(|app| Ok(app.native_diagnostics()))
        .unwrap()
        .to_string();
    assert!(diagnostics.contains("cast_preferences_write_failed"));
    assert!(!diagnostics.contains("127.0.0.1"));
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn live_seek_uses_actual_keyframe_for_receiver_position_and_local_resume() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::with_duration(30);
    let receiver = Receiver::new();
    // Replace only this newly created test cache, before it is pinned.
    let item = with_app(|app| Ok(app.native_core_snapshot()?.current_item.unwrap())).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/native_host/cast_session/fixtures/keyframes-30s.mp4");
    let video = host
        .context
        .cache_root
        .join(item.video_media_url.strip_prefix("/media/").unwrap());
    let audio = host.context.cache_root.join(
        item.audio_variants[0]["audio_url"]
            .as_str()
            .unwrap()
            .strip_prefix("/media/")
            .unwrap(),
    );
    std::fs::copy(&source, video).unwrap();
    std::fs::copy(source.with_file_name("audio-30s.m4a"), audio).unwrap();
    receiver.state.lock().unwrap().duration = 30;
    host.connect(receiver.renderer());
    host.post("/api/cast/command", json!({"action":"seek-absolute", "target_seconds":21, "playback_generation":host.snapshot()["playback_generation"]})).unwrap();
    host.wait(|s| s["cast"]["busy"] == false);
    let key = host.snapshot()["cast"]["selected"]
        .as_str()
        .unwrap()
        .to_owned();
    mode(&host, &key, "live");
    host.wait(|s| s["cast"]["busy"] == false && s["cast"]["phase"] == "active");
    receiver.state.lock().unwrap().position = 5;
    host.wait(|s| s["cast"]["position"] == 25.0);
    host.post("/api/cast/disconnect", json!({})).unwrap();
    let result = host.wait(|s| s["cast"]["phase"] == "idle" && s["cast"]["busy"] == false);
    assert_eq!(result["cast"]["resume"]["position"], 25.0);
}

#[test]
fn registered_remote_can_control_but_cannot_manage_cast() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::new();
    let token = with_app(|app| app.native_join_remote("", "registered-phone".into())).unwrap();
    let remote = Identity {
        token,
        loopback: false,
        client: "remote".into(),
    };
    for (path, body, method) in [
        ("/api/cast/discover", json!({}), Method::POST),
        ("/api/cast/connect", json!({}), Method::POST),
        ("/api/cast/disconnect", json!({}), Method::POST),
        ("/api/cast/report", json!({}), Method::GET),
        (
            "/api/cast/command",
            json!({"command":"cast.set_device_mode"}),
            Method::POST,
        ),
        (
            "/api/cast/command",
            json!({"command":"cast.set_device_learning"}),
            Method::POST,
        ),
    ] {
        assert_eq!(
            route(&host.context, &remote, &method, path, &body)
                .unwrap_err()
                .code,
            "forbidden"
        );
    }
    with_app(|app| {
        let state = &mut app.native().cast;
        state.owns_playback = true;
        state.phase = Phase::Active;
        state.worker_started = true;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        route(
            &host.context,
            &remote,
            &Method::POST,
            "/api/cast/command",
            &json!({"action":"pause", "playback_generation":host.snapshot()["playback_generation"]})
        )
        .unwrap()["accepted"],
        true
    );
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn destructive_session_changes_exit_with_explicit_reason_and_clear_queue_retains_cast() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    for action in [
        "clear_current",
        "begin",
        "discard",
        "reset_runtime",
        "restore",
        "clear_queue",
    ] {
        let host = Host::new();
        let receiver = Receiver::new();
        host.connect(receiver.renderer());
        let archive = crate::app_state::SessionArchiveSeed {
            file_name: "next.json".into(),
            session_started_at: 5.0,
            items: Vec::new(),
        };
        let request = match action {
            "clear_current" => AppStateRequest::SetCurrentItem {
                schema_version: 1,
                item_id: None,
                reset_av_delay: false,
                now: 5.0,
            },
            "begin" => AppStateRequest::BeginSession {
                schema_version: 1,
                new_session: archive,
                now: 5.0,
            },
            "discard" => AppStateRequest::DiscardBackup {
                schema_version: 1,
                new_session: archive,
                now: 5.0,
            },
            "reset_runtime" => AppStateRequest::ResetRuntime {
                schema_version: 1,
                new_session: archive,
                now: 5.0,
            },
            "restore" => AppStateRequest::RestoreBackup {
                schema_version: 1,
                reset_av_delay: false,
                now: 5.0,
            },
            _ => AppStateRequest::ClearPlaylist {
                schema_version: 1,
                now: 5.0,
            },
        };
        assert!(crate::execute_app_state(request).error().is_none());
        if action == "clear_queue" {
            thread::sleep(Duration::from_millis(150));
            assert_eq!(host.snapshot()["cast"]["phase"], "active");
            assert_eq!(host.snapshot()["current_item"]["id"], "song-one");
        } else {
            let snap = host.wait(|s| s["cast"]["phase"] == "idle");
            assert_eq!(snap["cast"]["owns_playback"], false);
            assert_eq!(
                snap["cast"]["error_code"],
                if action == "restore" {
                    "cast_media_unavailable"
                } else if action == "clear_current" {
                    "cast_no_current_item"
                } else {
                    "cast_queue_finished"
                }
            );
        }
    }
}

#[test]
#[ignore = "requires trusted BILIKARA_TEST_CAST_COMPANION"]
fn real_cast_and_mode_write_leave_checkpoint_readable_by_legacy_seed() {
    let _serial = GLOBAL_APP_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let host = Host::with_storage(120, false);
    let receiver = Receiver::new();
    host.connect(receiver.renderer());
    let key = host.snapshot()["cast"]["selected"]
        .as_str()
        .unwrap()
        .to_owned();
    mode(&host, &key, "file");
    let checkpoint = std::fs::read(host.context.directory.join("host-state.json")).unwrap();
    let saved: Value = serde_json::from_slice(&checkpoint).unwrap();
    let _: crate::app_state::AppStateSeed = serde_json::from_value(saved["state"].clone()).unwrap();
    assert!(!String::from_utf8_lossy(&checkpoint).contains("\"cast"));
    let prefs = std::fs::read_to_string(host.context.directory.join("cast-devices.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&prefs).unwrap()["devices"][&key]["mode"],
        "file"
    );
    for private in ["Fixture TV", "127.0.0.1", "uuid:cast-fixture"] {
        assert!(!prefs.contains(private));
    }
    host.post("/api/cast/disconnect", json!({})).unwrap();
    host.wait(|s| s["cast"]["phase"] == "idle" && s["cast"]["busy"] == false);
    assert!(
        crate::native_host_storage::NativeHostStorage::inspect(&host.context.directory)
            .unwrap()
            .is_some()
    );
}
