//! Test-only construction of the existing loopback policy, isolated from production.
use super::*;

pub(crate) async fn connect(qr: &str) -> PureKOperation<Session> {
    PureKClient {
        policy: NetworkPolicy::LoopbackFixture,
        interfaces: Some(vec![Ipv4Addr::LOCALHOST]),
        timeout: Duration::from_secs(2),
        retry_interval: Duration::from_millis(1),
        route_lookup: |_| Ok(Ipv4Addr::LOCALHOST),
        notify_destination: (Ipv4Addr::LOCALHOST, 9).into(),
    }
    .connect(
        qr,
        AddressApproval::LanLiteralOnly,
        &CancellationToken::new(),
    )
    .await
}
