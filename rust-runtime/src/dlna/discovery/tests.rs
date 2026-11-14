// Ported from VZRXS/bilikara PR #120 by FMPeach (MIT), commit 297ae1de,
// rust-runtime/src/show_dlna/discovery/tests.rs, with async cancellation regressions.
use super::*;
use crate::dlna::test_support::*;

#[test]
fn deeply_nested_description_from_notify_returns_structured_error() {
    let source = format!("<root>{}{}</root>", "<a>".repeat(5000), "</a>".repeat(5000));
    let fixture = Fixture::start(vec![Reply::raw(None, 200, source)]);
    let location = format!("http://{}/deep.xml", fixture.address);
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            run(async {
                let notify = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                let target = notify.local_addr().unwrap();
                let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                sender
                    .send_to(
                        format!(
                            "NOTIFY * HTTP/1.1\r\nNTS: ssdp:alive\r\nLOCATION: {location}\r\n\r\n"
                        )
                        .as_bytes(),
                        target,
                    )
                    .await
                    .unwrap();
                let mut stages = Vec::new();
                let result = collect_renderers(
                    &[Ipv4Addr::LOCALHOST],
                    NetworkPolicy::LoopbackFixture,
                    options(target, Duration::from_millis(300)),
                    Sockets {
                        search: Vec::new(),
                        notify: Some(notify),
                    },
                    &CancellationToken::new(),
                    &mut |_| {},
                    &mut stages,
                )
                .await
                .unwrap();
                assert!(result.is_empty());
                assert!(stages.iter().any(|stage| {
                    stage.stage == Stage::Description
                        && stage.result.as_ref().is_err_and(|error| {
                            error.code == ErrorCode::Policy(PolicyError::TooDeep)
                        })
                }));
            })
        })
        .unwrap()
        .join()
        .unwrap();
    fixture.join();
}

fn options(destination: SocketAddr, window: Duration) -> Options {
    Options {
        window,
        destination,
        notify_port: 0,
    }
}

#[test]
fn msearch_visits_each_interface_and_all_targets_without_vendor_filtering() {
    let fixture = Fixture::start(vec![Reply::raw(
        None,
        200,
        description(
            "Ordinary MediaRenderer",
            "<manufacturer>Example TV</manufacturer>",
        ),
    )]);
    let location = format!("http://{}/device.xml", fixture.address);
    run(async {
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let destination = udp.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut seen = HashSet::new();
            let mut buffer = [0; 4096];
            for _ in 0..SEARCH_TARGETS.len() * 2 {
                let (length, peer) =
                    tokio::time::timeout(Duration::from_secs(2), udp.recv_from(&mut buffer))
                        .await
                        .unwrap()
                        .unwrap();
                let request = std::str::from_utf8(&buffer[..length]).unwrap();
                assert!(request.starts_with("M-SEARCH * HTTP/1.1\r\n"));
                assert!(request.contains("\r\nMX: 2\r\n"));
                let target = request
                    .lines()
                    .find_map(|line| line.strip_prefix("ST: "))
                    .unwrap();
                seen.insert((peer.ip(), target.to_owned()));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nLOCATION: {location}\r\nUSN: uuid:fixture::upnp:rootdevice\r\nSERVER: Example/1.0\r\n\r\n"
                );
                udp.send_to(response.as_bytes(), peer).await.unwrap();
            }
            seen
        });
        let local = Ipv4Addr::LOCALHOST;
        let alias = Ipv4Addr::new(127, 0, 0, 2);
        let mut updates = Vec::new();
        let operation = discover_on(
            vec![local, alias],
            NetworkPolicy::LoopbackFixture,
            options(destination, Duration::from_millis(800)),
            &CancellationToken::new(),
            |update| updates.push(update),
        )
        .await;
        let renderers = operation.result.unwrap();
        assert_eq!(renderers.len(), 1);
        assert_eq!(renderers[0].description.name, "Ordinary MediaRenderer");
        assert_eq!(
            renderers[0].description.vendor_class,
            dlna_policy::VendorClass::Generic
        );
        assert_eq!(renderers[0].local_address, local);
        assert!(
            updates
                .iter()
                .any(|u| matches!(u, DiscoveryUpdate::Renderers(r) if r.len() == 1))
        );
        let seen = server.await.unwrap();
        for ip in [local, alias] {
            for target in &SEARCH_TARGETS[..3] {
                assert!(
                    seen.contains(&(IpAddr::V4(ip), target.to_string())),
                    "{ip} {target}"
                );
            }
        }
    });
    let requests = fixture.join();
    assert_eq!(requests.len(), 1); // Duplicate advertisements do not refetch.
    assert!(requests[0].headers.starts_with("GET /device.xml "));
    assert_eq!(requests[0].peer.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
}

#[test]
fn unsolicited_notify_loads_a_description_and_withdraws_before_search_ends() {
    let fixture = Fixture::start(vec![Reply::raw(None, 200, description("Notify Only", ""))]);
    let location = format!("http://{}/device.xml", fixture.address);
    run(async {
        let notify = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let target = notify.local_addr().unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let (published, mut snapshots) = tokio::sync::mpsc::unbounded_channel();
        let worker = tokio::spawn(async move {
            let mut stages = Vec::new();
            collect_renderers(
                &[Ipv4Addr::LOCALHOST],
                NetworkPolicy::LoopbackFixture,
                options(target, Duration::from_secs(4)),
                Sockets {
                    search: Vec::new(),
                    notify: Some(notify),
                },
                &trigger,
                &mut |update| {
                    if let DiscoveryUpdate::Renderers(renderers) = update {
                        published.send(renderers).unwrap();
                    }
                },
                &mut stages,
            )
            .await
        });
        let alive = format!(
            "NOTIFY * HTTP/1.1\r\nNTS: ssdp:alive\r\nUSN: UUID:Fixture::{}\r\nLOCATION: {location}\r\n\r\n",
            dlna_policy::MEDIA_RENDERER
        );
        sender.send_to(alive.as_bytes(), target).await.unwrap();
        sender.send_to(alive.as_bytes(), target).await.unwrap();
        let found = tokio::time::timeout(Duration::from_secs(2), snapshots.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description.name, "Notify Only");
        let byebye =
            b"NOTIFY * HTTP/1.1\r\nNTS: ssdp:byebye\r\nUSN: uuid:fixture::upnp:rootdevice\r\n\r\n";
        let spoof = UdpSocket::bind((Ipv4Addr::new(127, 0, 0, 2), 0))
            .await
            .unwrap();
        spoof.send_to(byebye, target).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(80), snapshots.recv())
                .await
                .is_err()
        );
        sender.send_to(byebye, target).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), snapshots.recv())
                .await
                .unwrap()
                .unwrap()
                .is_empty()
        );
        cancel.cancel();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), worker)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
    });
    fixture.join();
}

#[test]
fn no_interfaces_and_notify_port_conflicts_degrade_without_panicking() {
    run(async {
        let operation = discover_on(
            Vec::new(),
            NetworkPolicy::LoopbackFixture,
            Options::default(),
            &CancellationToken::new(),
            |_| {},
        )
        .await;
        assert_eq!(operation.result.unwrap_err().code, ErrorCode::NoInterfaces);
        assert_eq!(operation.stages.last().unwrap().stage, Stage::Discovery);
        let occupied = StdUdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let mut stages = Vec::new();
        let sockets = open_sockets(&[Ipv4Addr::LOCALHOST], port, &mut |stage| {
            stages.push(stage)
        });
        assert_eq!(sockets.search.len(), 1);
        assert!(sockets.notify.is_none());
        let notify_error = stages
            .iter()
            .find(|s| s.stage == Stage::NotifyListen)
            .unwrap();
        assert_eq!(
            notify_error.result.as_ref().unwrap_err().code,
            ErrorCode::SocketBind
        );
        assert_eq!(notify_error.disposition, Disposition::Ignored);
        let destination = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let operation = discover_on(
            vec![Ipv4Addr::LOCALHOST],
            NetworkPolicy::LoopbackFixture,
            Options {
                window: Duration::from_millis(60),
                destination: destination.local_addr().unwrap(),
                notify_port: port,
            },
            &CancellationToken::new(),
            |_| {},
        )
        .await;
        assert!(operation.result.unwrap().is_empty());
    });
}

#[test]
fn route_source_must_be_one_of_the_actual_interface_facts() {
    let source = route_source_on(
        Ipv4Addr::LOCALHOST,
        &[Ipv4Addr::LOCALHOST],
        NetworkPolicy::LoopbackFixture,
    )
    .unwrap();
    assert_eq!(source, Ipv4Addr::LOCALHOST);
    assert_eq!(
        route_source_on(Ipv4Addr::LOCALHOST, &[], NetworkPolicy::LoopbackFixture)
            .unwrap_err()
            .code,
        ErrorCode::Policy(PolicyError::RouteNotLocal)
    );
    assert!(route_source(Ipv4Addr::LOCALHOST).is_err());
    assert!(route_source(Ipv4Addr::new(1, 1, 1, 1)).is_err());
}

#[test]
fn manual_description_url_accepts_relaxed_descriptions_and_marks_lebo() {
    for source in [
        description("客厅乐播", "<manufacturer>HPPLAY</manufacturer>").replace("<device>", "<URLBase>http://192.168.2.20/</URLBase><device>"),
        description("LeBo TV", "").replace(">/control<", ">/control?first=1&amp;second=2<"),
        description("乐播", "").replace("</serviceList>", "<service><serviceType>urn:schemas-upnp-org:service:AVTransport:2</serviceType><controlURL>_urn:control?ok=1</controlURL></service></serviceList>"),
    ] {
        let fixture = Fixture::start(vec![Reply::raw(None, 200, source)]);
        let location = format!("http://{}/device.xml", fixture.address);
        let operation = run(load_on(&location, "", "", &[Ipv4Addr::LOCALHOST], NetworkPolicy::LoopbackFixture, HTTP_TIMEOUT, &CancellationToken::new()));
        assert_eq!(operation.result.unwrap().description.vendor_class, dlna_policy::VendorClass::Lebo);
        assert_eq!(operation.stages.len(), 2);
        assert_eq!(operation.stages[0].stage, Stage::Route);
        assert_eq!(operation.stages[1].stage, Stage::Description);
        fixture.join();
    }
    let source = description("External", "").replace(">/control<", ">http://127.0.0.2/control<");
    let fixture = Fixture::start(vec![Reply::raw(None, 200, source)]);
    let location = format!("http://{}/device.xml", fixture.address);
    assert_eq!(
        run(load_on(
            &location,
            "",
            "",
            &[Ipv4Addr::LOCALHOST],
            NetworkPolicy::LoopbackFixture,
            HTTP_TIMEOUT,
            &CancellationToken::new()
        ))
        .result
        .unwrap_err()
        .code,
        ErrorCode::Policy(PolicyError::ExternalControl)
    );
    fixture.join();
}

#[test]
fn cancelling_discovery_with_a_pending_description_returns_within_100ms() {
    let fixture = Fixture::start(vec![
        Reply::raw(None, 200, description("Delayed", "")).delayed(Duration::from_millis(350)),
    ]);
    let location = format!("http://{}/device.xml", fixture.address);
    run(async {
        let notify = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = notify.local_addr().unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let worker = tokio::spawn(async move {
            let mut stages = Vec::new();
            let result = collect_renderers(
                &[Ipv4Addr::LOCALHOST],
                NetworkPolicy::LoopbackFixture,
                options(address, SEARCH_WINDOW),
                Sockets {
                    search: Vec::new(),
                    notify: Some(notify),
                },
                &trigger,
                &mut |_| {},
                &mut stages,
            )
            .await;
            (result, stages)
        });
        sender
            .send_to(
                format!("NOTIFY * HTTP/1.1\r\nNTS: ssdp:alive\r\nLOCATION: {location}\r\n\r\n")
                    .as_bytes(),
                address,
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        let (result, stages) = tokio::time::timeout(Duration::from_millis(100), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().is_cancelled());
        assert!(stages.iter().any(|stage| stage.stage == Stage::Description
            && stage.result.as_ref().is_err_and(DlnaError::is_cancelled)));
        // The listener is released along with cancellation, not after HTTP timeout.
        StdUdpSocket::bind(address).unwrap();
    });
    fixture.join();
}

#[test]
fn failed_descriptions_retry_and_withdrawn_inflight_results_do_not_reappear() {
    let fixture = Fixture::start(vec![
        Reply::raw(None, 503, "busy"),
        Reply::raw(None, 200, description("Ready", "")),
    ]);
    let location = format!("http://{}/device.xml", fixture.address);
    run(async {
        let notify = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = notify.local_addr().unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut updates = Vec::new();
        sender
            .send_to(
                format!("NOTIFY * HTTP/1.1\r\nNTS: ssdp:alive\r\nLOCATION: {location}\r\n\r\n")
                    .as_bytes(),
                address,
            )
            .await
            .unwrap();
        let mut stages = Vec::new();
        let result = collect_renderers(
            &[Ipv4Addr::LOCALHOST],
            NetworkPolicy::LoopbackFixture,
            options(address, Duration::from_millis(1500)),
            Sockets {
                search: Vec::new(),
                notify: Some(notify),
            },
            &CancellationToken::new(),
            &mut |u| updates.push(u),
            &mut stages,
        )
        .await
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].description.name, "Ready");
        assert!(
            stages
                .iter()
                .any(|s| s.stage == Stage::Description && s.http_status == Some(503))
        );
    });
    fixture.join();

    let fixture = Fixture::start(vec![
        Reply::raw(None, 200, description("Too late", "")).delayed(Duration::from_millis(250)),
    ]);
    let location = format!("http://{}/device.xml", fixture.address);
    run(async {
        let notify = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = notify.local_addr().unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let server = tokio::spawn(async move {
            sender.send_to(format!("NOTIFY * HTTP/1.1\r\nNTS: ssdp:alive\r\nUSN: uuid:fixture\r\nLOCATION: {location}\r\n\r\n").as_bytes(), address).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            sender
                .send_to(
                    b"NOTIFY * HTTP/1.1\r\nNTS: ssdp:byebye\r\nUSN: uuid:fixture\r\n\r\n",
                    address,
                )
                .await
                .unwrap();
        });
        let result = collect_renderers(
            &[Ipv4Addr::LOCALHOST],
            NetworkPolicy::LoopbackFixture,
            options(address, Duration::from_millis(500)),
            Sockets {
                search: Vec::new(),
                notify: Some(notify),
            },
            &CancellationToken::new(),
            &mut |_| {},
            &mut Vec::new(),
        )
        .await
        .unwrap();
        assert!(result.is_empty());
        server.await.unwrap();
    });
    fixture.join();
}

#[test]
fn description_size_limits_and_redirects_are_recorded_without_following() {
    for chunked in [false, true] {
        let mut reply = Reply::raw(
            None,
            200,
            "x".repeat(dlna_policy::MAX_DESCRIPTION_BYTES + 1),
        );
        reply.chunked = chunked;
        let fixture = Fixture::start(vec![reply]);
        let location = format!("http://{}/device.xml", fixture.address);
        let error = run(load_on(
            &location,
            "",
            "",
            &[Ipv4Addr::LOCALHOST],
            NetworkPolicy::LoopbackFixture,
            HTTP_TIMEOUT,
            &CancellationToken::new(),
        ))
        .result
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Policy(PolicyError::TooLarge));
        fixture.join();
    }
    let mut reply = Reply::raw(None, 302, "moved");
    reply.headers = "Location: http://127.0.0.1:1/admin\r\n".into();
    let fixture = Fixture::start(vec![reply]);
    let location = format!("http://{}/device.xml", fixture.address);
    let error = run(load_on(
        &location,
        "",
        "",
        &[Ipv4Addr::LOCALHOST],
        NetworkPolicy::LoopbackFixture,
        HTTP_TIMEOUT,
        &CancellationToken::new(),
    ))
    .result
    .unwrap_err();
    assert_eq!(error.http_status, Some(302));
    fixture.join();
}
