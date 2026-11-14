// Ported from VZRXS/bilikara PR #120 by FMPeach (MIT), commit 297ae1de,
// rust-runtime/src/show_dlna/discovery.rs. See THIRD_PARTY_NOTICES.md.
//! Per-interface M-SEARCH plus best-effort NOTIFY. Async description tasks are
//! aborted when discovery ends; cancellation never joins blocking HTTP workers.

use super::*;
use dlna_policy::{
    AdvertisementKind, SsdpChannel, SsdpMessage, accept_advertisement, canonical_usn, lan_url,
    parse_description, parse_ssdp, validate_route_source,
};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;

const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
pub const SEARCH_WINDOW: Duration = Duration::from_secs(40);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const SEND_INTERVAL: Duration = Duration::from_millis(80);
const DESCRIPTION_RETRY: Duration = Duration::from_secs(1);
const MAX_LOCATIONS: usize = 128;
const DESCRIPTION_WORKERS: usize = 8;
const MAX_STAGES: usize = 512;
const SEARCH_TARGETS: [&str; 6] = [
    dlna_policy::MEDIA_RENDERER,
    "urn:schemas-upnp-org:service:AVTransport:1",
    "ssdp:all",
    dlna_policy::MEDIA_RENDERER,
    "urn:schemas-upnp-org:service:AVTransport:1",
    "ssdp:all",
];

#[derive(Clone, Debug)]
pub enum DiscoveryUpdate {
    Renderers(Vec<Renderer>),
    Stage(StageResult),
}

/// A bounded 40-second scan; publish receives incremental snapshots and stage
/// facts. NOTIFY binding/join failures degrade to M-SEARCH and remain visible.
pub async fn discover(
    cancel: &CancellationToken,
    publish: impl FnMut(DiscoveryUpdate),
) -> Operation<Vec<Renderer>> {
    discover_on(
        local_ipv4(NetworkPolicy::Lan),
        NetworkPolicy::Lan,
        Options::default(),
        cancel,
        publish,
    )
    .await
}

pub fn route_source(target: Ipv4Addr) -> Result<Ipv4Addr, DlnaError> {
    if !NetworkPolicy::Lan.allows(target) {
        return Err(PolicyError::AddressNotAllowed.into());
    }
    route_source_on(target, &local_ipv4(NetworkPolicy::Lan), NetworkPolicy::Lan)
}

pub(super) fn route_source_on(
    target: Ipv4Addr,
    interfaces: &[Ipv4Addr],
    policy: NetworkPolicy,
) -> Result<Ipv4Addr, DlnaError> {
    // UDP connect consults the OS routing table without sending any packet.
    let socket = StdUdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|_| DlnaError::new(ErrorCode::RouteUnavailable))?;
    socket
        .connect((target, SSDP_PORT))
        .map_err(|_| DlnaError::new(ErrorCode::RouteUnavailable))?;
    match socket
        .local_addr()
        .map_err(|_| DlnaError::new(ErrorCode::RouteUnavailable))?
        .ip()
    {
        IpAddr::V4(address) => {
            validate_route_source(address, interfaces, policy).map_err(Into::into)
        }
        _ => Err(DlnaError::new(ErrorCode::RouteUnavailable)),
    }
}

/// Manual description URL fallback uses the same policy and route validation.
pub async fn load_renderer(location: &str, cancel: &CancellationToken) -> Operation<Renderer> {
    load_on(
        location,
        "",
        "",
        &local_ipv4(NetworkPolicy::Lan),
        NetworkPolicy::Lan,
        HTTP_TIMEOUT,
        cancel,
    )
    .await
}

async fn load_on(
    location: &str,
    usn: &str,
    server: &str,
    interfaces: &[Ipv4Addr],
    policy: NetworkPolicy,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Operation<Renderer> {
    let since = Instant::now();
    let route = if cancel.is_cancelled() {
        Err(DlnaError::new(ErrorCode::Cancelled))
    } else {
        lan_url(location, policy)
            .map_err(DlnaError::from)
            .and_then(|(_, address)| route_source_on(address, interfaces, policy))
    };
    let stages = vec![StageResult::new(Stage::Route, since, None, &route)];
    let local = match route {
        Ok(local) => local,
        Err(error) => {
            return Operation {
                result: Err(error),
                stages,
            };
        }
    };
    let mut operation =
        load_description(location, usn, server, local, policy, timeout, cancel).await;
    operation.stages.splice(0..0, stages);
    operation
}

async fn load_description(
    location: &str,
    usn: &str,
    server: &str,
    local: Ipv4Addr,
    policy: NetworkPolicy,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Operation<Renderer> {
    let since = Instant::now();
    let mut status = None;
    let result = async {
        let client = http_client(local, timeout)?;
        let (url, _) = lan_url(location, policy)?;
        let reply = http_request(
            &client,
            url,
            None,
            dlna_policy::MAX_DESCRIPTION_BYTES,
            cancel,
        )
        .await?;
        status = Some(reply.status);
        if !(200..300).contains(&reply.status) {
            return Err(DlnaError::new(ErrorCode::HttpStatus).status(reply.status));
        }
        let description = parse_description(&reply.body, location, usn, server, policy)
            .map_err(|error| DlnaError::from(error).status(reply.status))?;
        Ok(Renderer {
            description,
            local_address: local,
        })
    }
    .await;
    let stage = StageResult::new(Stage::Description, since, status, &result);
    Operation {
        result,
        stages: vec![stage],
    }
}

struct Options {
    window: Duration,
    destination: SocketAddr,
    notify_port: u16,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            window: SEARCH_WINDOW,
            destination: (MULTICAST, SSDP_PORT).into(),
            notify_port: SSDP_PORT,
        }
    }
}

struct SearchChannel {
    socket: UdpSocket,
    interface: Ipv4Addr,
}
struct Sockets {
    search: Vec<SearchChannel>,
    notify: Option<UdpSocket>,
}

fn search_socket(interface: Ipv4Addr) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.bind(&SocketAddr::from((interface, 0)).into())?;
    socket.set_multicast_if_v4(&interface)?;
    socket.set_multicast_ttl_v4(4)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

fn open_sockets(
    interfaces: &[Ipv4Addr],
    port: u16,
    record: &mut impl FnMut(StageResult),
) -> Sockets {
    let mut search = Vec::new();
    for &interface in interfaces {
        let since = Instant::now();
        let result = search_socket(interface).map_err(|_| DlnaError::new(ErrorCode::SocketBind));
        record(StageResult::new(Stage::SearchBind, since, None, &result));
        if let Ok(socket) = result {
            search.push(SearchChannel { socket, interface });
        }
    }
    let since = Instant::now();
    let notify = (|| {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
        socket.set_multicast_loop_v4(true)?;
        socket.set_nonblocking(true)?;
        Ok::<_, io::Error>(socket)
    })()
    .map_err(|_| DlnaError::new(ErrorCode::SocketBind));
    let mut stage = StageResult::new(Stage::NotifyListen, since, None, &notify);
    if stage.result.is_err() {
        stage.disposition = Disposition::Ignored;
    }
    record(stage);
    let notify = notify.ok().and_then(|socket| {
        for interface in interfaces {
            let since = Instant::now();
            let result = socket
                .join_multicast_v4(&MULTICAST, interface)
                .map_err(|_| DlnaError::new(ErrorCode::MulticastJoin));
            let mut stage = StageResult::new(Stage::MulticastJoin, since, None, &result);
            if stage.result.is_err() {
                stage.disposition = Disposition::Ignored;
            }
            record(stage);
        }
        match UdpSocket::from_std(socket.into()) {
            Ok(socket) => Some(socket),
            Err(_) => {
                let result: Result<(), _> = Err(DlnaError::new(ErrorCode::SocketBind));
                let mut stage =
                    StageResult::new(Stage::NotifyListen, Instant::now(), None, &result);
                stage.disposition = Disposition::Ignored;
                record(stage);
                None
            }
        }
    });
    Sockets { search, notify }
}

fn search_request(target: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\nUSER-AGENT: UPnP/1.1 bilikara/0.8\r\n\r\n"
    )
}

async fn discover_on(
    interfaces: Vec<Ipv4Addr>,
    policy: NetworkPolicy,
    options: Options,
    cancel: &CancellationToken,
    mut publish: impl FnMut(DiscoveryUpdate),
) -> Operation<Vec<Renderer>> {
    let since = Instant::now();
    let mut stages = Vec::new();
    let mut record = |stage: StageResult| {
        publish(DiscoveryUpdate::Stage(stage.clone()));
        if stages.len() < MAX_STAGES {
            stages.push(stage);
        }
    };
    let result = if cancel.is_cancelled() {
        Err(DlnaError::new(ErrorCode::Cancelled))
    } else if interfaces.is_empty() {
        Err(DlnaError::new(ErrorCode::NoInterfaces))
    } else {
        let sockets = open_sockets(&interfaces, options.notify_port, &mut record);
        if sockets.search.is_empty() && sockets.notify.is_none() {
            Err(DlnaError::new(ErrorCode::DiscoveryUnavailable))
        } else {
            // End the record borrow before collection uses the same publisher.
            collect_renderers(
                &interfaces,
                policy,
                options,
                sockets,
                cancel,
                &mut publish,
                &mut stages,
            )
            .await
        }
    };
    let stage = StageResult::new(Stage::Discovery, since, None, &result);
    publish(DiscoveryUpdate::Stage(stage.clone()));
    stages.push(stage);
    Operation { result, stages }
}

struct Candidate {
    load_id: u64,
    usn: String,
    server: String,
    helper: Option<Ipv4Addr>,
    local: Ipv4Addr,
    renderer: Option<Renderer>,
    retry_at: Option<Instant>,
    pending: bool,
    loading_since: Option<Instant>,
}

struct Seen {
    message: SsdpMessage,
    sender: Ipv4Addr,
    reply_on: Option<Ipv4Addr>,
}

fn receive_packets(socket: &UdpSocket, reply_on: Option<Ipv4Addr>, events: &mut Vec<Seen>) {
    let mut buffer = [0; dlna_policy::MAX_SSDP_BYTES + 1];
    for _ in 0..64 {
        let Ok((length, sender)) = socket.try_recv_from(&mut buffer) else {
            break;
        };
        let channel = if reply_on.is_some() {
            SsdpChannel::SearchResponse
        } else {
            SsdpChannel::Notify
        };
        if let IpAddr::V4(sender) = sender.ip()
            && let Some(message) = parse_ssdp(&buffer[..length], channel)
        {
            events.push(Seen {
                message,
                sender,
                reply_on,
            });
        }
    }
}

fn snapshot(candidates: &HashMap<String, Candidate>) -> Vec<Renderer> {
    let mut renderers: Vec<_> = candidates
        .values()
        .filter_map(|c| c.renderer.clone())
        .collect();
    renderers.sort_by_cached_key(|r| {
        (
            r.description.name.to_lowercase(),
            canonical_usn(&r.description.id),
            r.description.location.clone(),
        )
    });
    let mut seen = HashSet::new();
    renderers.retain(|r| seen.insert(canonical_usn(&r.description.id)));
    renderers
}

fn record(
    stage: StageResult,
    publish: &mut impl FnMut(DiscoveryUpdate),
    stages: &mut Vec<StageResult>,
) {
    // Bound diagnostic work as well as storage on noisy networks.
    if stages.len() < MAX_STAGES {
        publish(DiscoveryUpdate::Stage(stage.clone()));
        stages.push(stage);
    }
}

fn interrupted_descriptions(
    candidates: &HashMap<String, Candidate>,
    code: ErrorCode,
    publish: &mut impl FnMut(DiscoveryUpdate),
    stages: &mut Vec<StageResult>,
) {
    for candidate in candidates.values() {
        if let Some(since) = candidate.loading_since {
            let result: Result<(), _> = Err(DlnaError::new(code));
            record(
                StageResult::new(Stage::Description, since, None, &result),
                publish,
                stages,
            );
        }
    }
}

async fn collect_renderers(
    interfaces: &[Ipv4Addr],
    policy: NetworkPolicy,
    options: Options,
    sockets: Sockets,
    cancel: &CancellationToken,
    publish: &mut impl FnMut(DiscoveryUpdate),
    stages: &mut Vec<StageResult>,
) -> Result<Vec<Renderer>, DlnaError> {
    let deadline = Instant::now() + options.window;
    let mut send_at = Instant::now();
    let mut targets = SEARCH_TARGETS.into_iter();
    let mut candidates = HashMap::<String, Candidate>::new();
    let mut jobs = JoinSet::new();
    let mut events = Vec::new();
    let mut load_id = 0;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                interrupted_descriptions(&candidates, ErrorCode::Cancelled, publish, stages);
                return Err(DlnaError::new(ErrorCode::Cancelled));
            },
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        if Instant::now() >= deadline {
            interrupted_descriptions(&candidates, ErrorCode::Timeout, publish, stages);
            break;
        }
        if Instant::now() >= send_at
            && let Some(target) = targets.next()
        {
            let request = search_request(target);
            for channel in &sockets.search {
                let since = Instant::now();
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(DlnaError::new(ErrorCode::Cancelled)),
                    result = channel.socket.send_to(request.as_bytes(), options.destination) => result.map(|_| ()).map_err(|_| DlnaError::new(ErrorCode::SearchSend)),
                };
                record(
                    StageResult::new(Stage::SearchSend, since, None, &result),
                    publish,
                    stages,
                );
            }
            send_at = Instant::now() + SEND_INTERVAL;
        }
        if let Some(notify) = &sockets.notify {
            receive_packets(notify, None, &mut events);
        }
        for channel in &sockets.search {
            receive_packets(&channel.socket, Some(channel.interface), &mut events);
        }
        let mut changed = false;
        for Seen {
            message,
            sender,
            reply_on,
        } in events.drain(..)
        {
            if message.kind == AdvertisementKind::Byebye {
                candidates.retain(|location, candidate| {
                    let matches_id = candidate.usn == message.usn
                        || candidate
                            .renderer
                            .as_ref()
                            .is_some_and(|r| canonical_usn(&r.description.id) == message.usn);
                    let matches_sender = lan_url(location, policy)
                        .is_ok_and(|(_, address)| address == sender)
                        || candidate.helper == Some(sender);
                    let remove = matches_id && matches_sender;
                    changed |= remove && candidate.renderer.is_some();
                    !remove
                });
                continue;
            }
            let Some(location) = message.location else {
                continue;
            };
            let since = Instant::now();
            let accepted = accept_advertisement(&location, sender, reply_on, interfaces, policy)
                .map_err(DlnaError::from);
            let accepted = match accepted {
                Ok(value) => value,
                Err(error) => {
                    record(
                        StageResult::new(Stage::Advertisement, since, None, &Err::<(), _>(error)),
                        publish,
                        stages,
                    );
                    continue;
                }
            };
            let location = accepted.url.to_string();
            if let Some(candidate) = candidates.get_mut(&location) {
                candidate.helper = accepted.helper.or(candidate.helper);
                if candidate.usn.is_empty() {
                    candidate.usn = message.usn;
                }
                if !message.server.is_empty() {
                    candidate.server = message.server;
                    if let Some(renderer) = candidate.renderer.as_mut() {
                        let description = &mut renderer.description;
                        let vendor = dlna_policy::vendor_class(&[
                            &description.manufacturer,
                            &description.model_name,
                            &description.model_description,
                            &description.name,
                            &candidate.server,
                        ]);
                        changed |= vendor != description.vendor_class;
                        description.vendor_class = vendor;
                    }
                }
                continue;
            }
            if candidates.len() >= MAX_LOCATIONS {
                continue;
            }
            let since = Instant::now();
            let route = route_source_on(accepted.address, interfaces, policy);
            record(
                StageResult::new(Stage::Route, since, None, &route),
                publish,
                stages,
            );
            let Ok(local) = route else { continue };
            load_id += 1;
            candidates.insert(
                location,
                Candidate {
                    load_id,
                    usn: message.usn,
                    server: message.server,
                    helper: accepted.helper,
                    local,
                    renderer: None,
                    retry_at: None,
                    pending: true,
                    loading_since: None,
                },
            );
        }
        while let Some(Ok((location, id, operation))) = jobs.try_join_next() {
            let operation: Operation<Renderer> = operation;
            for stage in operation.stages {
                record(stage, publish, stages);
            }
            if let Some(candidate) = candidates.get_mut(&location)
                && candidate.load_id == id
            {
                candidate.pending = false;
                candidate.loading_since = None;
                changed |= operation.result.is_ok();
                candidate.retry_at = operation
                    .result
                    .is_err()
                    .then(|| Instant::now() + DESCRIPTION_RETRY);
                candidate.renderer = operation.result.ok();
            }
        }
        for (location, candidate) in &mut candidates {
            if jobs.len() >= DESCRIPTION_WORKERS {
                break;
            }
            if candidate.pending && candidate.retry_at.is_none()
                || candidate.retry_at.is_some_and(|at| Instant::now() >= at)
            {
                let location = location.clone();
                let usn = candidate.usn.clone();
                let server = candidate.server.clone();
                let local = candidate.local;
                let id = candidate.load_id;
                let cancel = cancel.clone();
                let timeout = deadline
                    .saturating_duration_since(Instant::now())
                    .min(HTTP_TIMEOUT);
                candidate.pending = false;
                candidate.retry_at = None;
                candidate.loading_since = Some(Instant::now());
                jobs.spawn(async move {
                    let operation =
                        load_description(&location, &usn, &server, local, policy, timeout, &cancel)
                            .await;
                    (location, id, operation)
                });
            }
        }
        if changed {
            publish(DiscoveryUpdate::Renderers(snapshot(&candidates)));
        }
    }
    // JoinSet drop aborts requests, including header/body stalls, without waiting.
    Ok(snapshot(&candidates))
}

#[cfg(test)]
mod tests;
