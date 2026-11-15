//! Test-only access to the existing private loopback transport; no production API change.
use super::*;

pub(crate) async fn discover(
    location: String,
    trace: &super::super::diagnostics::SessionTrace,
    connection: u64,
) -> Renderer {
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let destination = udp.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let mut buffer = [0; 4096];
        for _ in 0..SEARCH_TARGETS.len() {
            let (_, peer) =
                tokio::time::timeout(Duration::from_secs(2), udp.recv_from(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nLOCATION: {location}\r\nUSN: uuid:fixture::upnp:rootdevice\r\nSERVER: fixture/1\r\n\r\n"
            );
            udp.send_to(response.as_bytes(), peer).await.unwrap();
        }
    });
    let operation = discover_on(
        vec![Ipv4Addr::LOCALHOST],
        NetworkPolicy::LoopbackFixture,
        Options {
            window: Duration::from_millis(900),
            destination,
            notify_port: 0,
            route_lookup: |_| Ok(Ipv4Addr::LOCALHOST),
        },
        &CancellationToken::new(),
        |update| trace.discovery(connection, &update),
    )
    .await;
    worker.await.unwrap();
    operation.result.unwrap().pop().unwrap()
}
