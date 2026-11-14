use super::*;
use crate::dlna::dlna_policy;
use std::net::IpAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) const HEADER_LIMIT: usize = 8192;
pub(super) const HEADER_DEADLINE: Duration = Duration::from_secs(5);
pub const WRITE_STALL: Duration = Duration::from_secs(15);

pub(super) struct Request {
    pub method: String,
    pub target: String,
    pub range: Option<String>,
}

/// Denials must not race the request sender. An incomplete, expired or oversized
/// header is closed without an HTTP response; all HTTP replies follow CRLFCRLF.
pub(super) async fn read_header(
    socket: &mut TcpStream,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Reason> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Reason::Cancelled),
        result = tokio::time::timeout(HEADER_DEADLINE, async {
            let mut bytes = Vec::with_capacity(1024);
            loop {
                let mut block = [0; 1024];
                let remaining = HEADER_LIMIT - bytes.len();
                if remaining == 0 {
                    return Err(Reason::HeaderTooLarge);
                }
                let size = socket.read(&mut block[..remaining.min(1024)]).await
                    .map_err(|_| Reason::PeerClosed)?;
                if size == 0 {
                    return Err(Reason::IncompleteHeader);
                }
                bytes.extend_from_slice(&block[..size]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    bytes.truncate(end + 4);
                    return Ok(bytes);
                }
            }
        }) => result.map_err(|_| Reason::HeaderTimeout)?,
    }
}

pub(super) fn parse(bytes: &[u8]) -> Result<Request, Reason> {
    let text = std::str::from_utf8(bytes).map_err(|_| Reason::MalformedRequest)?;
    let mut lines = text.split("\r\n");
    let first: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if first.len() != 3 || !matches!(first[2], "HTTP/1.0" | "HTTP/1.1") {
        return Err(Reason::MalformedRequest);
    }
    let mut request = Request {
        method: first[0].into(),
        target: first[1].into(),
        range: None,
    };
    for line in lines.take_while(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(Reason::MalformedRequest);
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
        {
            return Err(Reason::MalformedRequest);
        }
        if name.eq_ignore_ascii_case("range") {
            if request.range.is_some() {
                // A multiple-range request is legal; this server can ignore it.
                request.range = Some("multiple".into());
            } else {
                request.range = Some(value.trim().into());
            }
        }
    }
    Ok(request)
}

pub(super) fn same_target(actual: &str, expected: &str) -> bool {
    actual.len() == expected.len()
        && actual
            .bytes()
            .zip(expected.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// LAN aliases, loopback, link-local and RFC1918 peers are accepted. This never
/// compares the peer with the renderer's description address for authorization.
pub(super) fn private_peer(peer: IpAddr) -> bool {
    match peer {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || (octets[0] == 100 && octets[1] & 0xc0 == 0x40) // shared (RFC 6598)
        }
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or_else(
            || ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
            |ip| private_peer(IpAddr::V4(ip)),
        ),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ByteRange {
    Full,
    Partial { start: u64, length: u64 },
    Unsatisfiable,
}

pub(super) fn byte_range(value: Option<&str>, length: u64) -> ByteRange {
    let Some(value) = value else {
        return ByteRange::Full;
    };
    let Some(value) = value.strip_prefix("bytes=") else {
        return ByteRange::Full;
    };
    // RFC 9110 allows ignoring unsupported multiple ranges.
    if value.contains(',') {
        return ByteRange::Full;
    }
    let Some((start, end)) = value.trim().split_once('-') else {
        return ByteRange::Unsatisfiable;
    };
    if length == 0 {
        return ByteRange::Unsatisfiable;
    }
    if start.is_empty() {
        return match unsigned(end) {
            Some(0) | None => ByteRange::Unsatisfiable,
            Some(suffix) => {
                let size = suffix.min(length);
                ByteRange::Partial {
                    start: length - size,
                    length: size,
                }
            }
        };
    }
    let Some(start) = unsigned(start).filter(|start| *start < length) else {
        return ByteRange::Unsatisfiable;
    };
    let end = if end.is_empty() {
        Some(length - 1)
    } else {
        unsigned(end).map(|end| end.min(length - 1))
    };
    match end.filter(|end| *end >= start) {
        Some(end) => ByteRange::Partial {
            start,
            length: end - start + 1,
        },
        None => ByteRange::Unsatisfiable,
    }
}

fn unsigned(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

pub(super) fn headers(
    shared: &Shared,
    generation: Option<&Generation>,
    status: u16,
    length: Option<u64>,
    range: Option<(u64, u64, u64)>,
) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        _ => "Service Unavailable",
    };
    let mime = generation.map_or("application/octet-stream", |g| g.mime());
    let file = generation.is_some_and(|g| matches!(g.body, Body::File(_)));
    let converted = generation.is_some_and(|g| match &g.body {
        Body::File(file) => file.converted,
        Body::Live(_) => false,
    });
    let mode = if file {
        DeliveryMode::File
    } else {
        DeliveryMode::Live
    };
    let features = dlna_policy::content_features(mode, converted);
    let ranges = if file { "bytes" } else { "none" };
    let mut header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\ntransferMode.dlna.org: Streaming\r\nEXT:\r\nServer: {}\r\nCache-Control: no-store\r\nConnection: close\r\nAccept-Ranges: {ranges}\r\ncontentFeatures.dlna.org: {features}\r\n",
        shared.server,
    );
    if let Some(length) = length {
        header.push_str(&format!("Content-Length: {length}\r\n"));
    }
    if let Some((start, count, total)) = range {
        if status == 416 {
            header.push_str(&format!("Content-Range: bytes */{total}\r\n"));
        } else {
            header.push_str(&format!(
                "Content-Range: bytes {start}-{}/{total}\r\n",
                start + count - 1
            ));
        }
    }
    if status == 405 {
        header.push_str("Allow: GET, HEAD\r\n");
    }
    header.push_str("\r\n");
    header.into_bytes()
}

pub(super) struct Writer<'a> {
    pub socket: &'a mut TcpStream,
    pub shared: &'a Shared,
    pub generation: Option<&'a Generation>,
    pub connection: &'a Client,
    pub status: Option<u16>,
    pub bytes: u64,
}

impl Writer<'_> {
    pub(super) async fn wait<T>(
        &self,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, Reason> {
        let generation_cancel = self
            .generation
            .map(|g| g.cancel.clone())
            .unwrap_or_default();
        tokio::select! {
            biased;
            _ = self.shared.stop.cancelled() => Err(Reason::Cancelled),
            _ = generation_cancel.cancelled() => Err(Reason::GenerationEnded),
            _ = self.connection.cancel.cancelled() => Err(Reason::Replaced),
            result = future => Ok(result),
        }
    }

    pub(super) fn event(&self, stage: HttpStage, result: Result<(), Reason>) {
        self.shared.event(
            stage,
            self.generation.map(|g| g.id),
            self.connection.id,
            self.status,
            result,
            self.bytes,
        );
    }

    pub(super) async fn send(&mut self, mut data: &[u8], body: bool) -> Result<(), Reason> {
        let mut deadline = tokio::time::Instant::now() + WRITE_STALL;
        while !data.is_empty() {
            let generation_cancel = self
                .generation
                .map(|g| g.cancel.clone())
                .unwrap_or_default();
            let result = tokio::select! {
                biased;
                _ = self.shared.stop.cancelled() => return Err(Reason::Cancelled),
                _ = generation_cancel.cancelled() => return Err(Reason::GenerationEnded),
                _ = self.connection.cancel.cancelled() => return Err(Reason::Replaced),
                result = tokio::time::timeout_at(deadline, self.socket.write(data)) => result,
            };
            let size = match result {
                Err(_) => return Err(Reason::WriteStall),
                Ok(Ok(0)) => return Err(Reason::PeerClosed),
                Ok(Ok(size)) => size,
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Ok(Err(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return Err(Reason::PeerClosed);
                }
                Ok(Err(_)) => return Err(Reason::WriteError),
            };
            data = &data[size..];
            deadline = tokio::time::Instant::now() + WRITE_STALL;
            if body {
                self.bytes += size as u64;
                if let Some(g) = self.generation {
                    self.shared.diag.written(size, g.id);
                    if !g.first.swap(true, Ordering::AcqRel) {
                        self.event(HttpStage::FirstByte, Ok(()));
                    }
                }
            }
        }
        Ok(())
    }
}
