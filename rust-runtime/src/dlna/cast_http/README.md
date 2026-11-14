# Cast HTTP transport (T4)

`CastEndpoint` owns an independent random-port listener and a random 128-bit
capability. `bind(renderer_ipv4, device_context, server_identity)` uses T2's
`discovery::route_source`; it never binds the existing Host HTTP router or
`0.0.0.0`. The caller supplies the OS version through `ServerIdentity`.

After T3 has finalized a `CastFile`, call
`publish_file(generation, file, ts_mime)`. The endpoint takes the file lease.
For live delivery, create `Arc<LiveStream>` with `LiveOptions`, call
`publish_live(generation, stream.clone(), ts_mime)`, then run
`stream.generate(probe, inputs, max_generation_bytes)` on a worker thread.
Alternatively, forward `cast_ts_with_events` events to `stream.push`, using
`LiveOptions::pacing()` and `stream.finish(success)` on producer completion.
Do not cancel production when an individual reader disconnects.

Both publication methods return `PublishedMedia`, including the capability URI,
content type, mode and optional length for T2's transport metadata. Generation
numbers must strictly increase within an endpoint. A new publication cancels
old readers and makes the old URI return 404. Each generation uses a distinct
live stream; a cancelled stream is not reusable. `clear_generation` removes
the current resource while retaining the listener/token. `shutdown().await`
cancels and joins the listener and all accepted connections. Drop initiates the
same cancellation when an explicit join is unavailable. T3 retains ownership of
staging/recovery; HTTP readers keep its file lease until file handles close.

T5/T6 choose the delivery mode, initially File. `DeliveryMode` is re-exported
from T1 and HTTP content features use T1's policy. This module does not persist
a device preference, silently retry with a different mode,
modify AppState, or implement the mode control itself. Live MP4 is unsupported;
MP4 is a finalized file with the same HTTP range behavior as file TS.

## HTTP behavior

GET/HEAD `/cast/{32 hex token}/{generation}.ts` (or `.mp4` for a file) are the
only resource routes. The entire header terminator must arrive within 5 seconds
and 8192 bytes before any HTTP reply, including errors. Incomplete/oversized
requests close without replying. Private, shared (RFC 6598), link-local and loopback sources are
accepted regardless of the renderer description address. A differing source
produces a `http.peer_mismatch` diagnostic. Public sources receive 403 after
the header is complete. Invalid capabilities/generations receive 404.

File GET supports a single byte range, open-ended ranges and suffixes; responses
include 206 or 416 and Content-Range. Unsupported units/multiple ranges are
ignored with a full 200 response. HEAD computes the same headers without a body.
File delivery has no pacing. CI reflects T3's audio conversion result.

Live GET ignores every Range header, uses 200, no Content-Length/chunking,
`Accept-Ranges: none` and `Connection: close`. A reader joins at the latest
whole keyframe group. The default five-second burst is configurable and shared
with T3 pacing. Each reader has independent 1x pacing and continuity counters.
If its byte lag exceeds the configured bound, or its cursor was evicted, it
finishes the current TS chunk, emits adaptation-only discontinuity markers for
previously seen PIDs and resumes at the latest keyframe. PCR/PTS/DTS are
preserved; markers signal the clock jump. Long GOPs are accepted within the
memory bound. The per-reader algorithm follows Kirakara-Show; see [NOTICE.md](NOTICE.md).

Both bodies wait asynchronously on socket backpressure. Every successful write
resets the progress deadline; only 15 continuous seconds with no write progress
produces `write_stall`. There is no short SO_SNDTIMEO. Tokio explicitly manages
accepted nonblocking sockets with readiness waits on Windows and other targets.

## Bounds and diagnostics

The process allows four endpoint listeners. Each endpoint accepts at most 16
connection tasks (including incomplete headers), and four simultaneous GETs.
The fifth GET cancels the oldest GET and waits for its socket to close before
starting the new one; no 409 is returned. HEAD uses no GET slot. Backlog sockets
wait in the OS until an accepted task is available.

The default live ring has a 64 MiB byte cap and retains at least ten seconds of
whole GOPs; configurable caps are at most 128 MiB. Keys/chunks are indexed for
constant-time reader lookup. The ring never waits on an HTTP reader. If the
required history/current GOP cannot fit, production explicitly fails with
`LiveError::BufferLimit`, rather than retaining less history or growing memory.
Session cancellation clears buffered data even if the producer still holds its
`Arc`. Per-reader scratch storage is limited to one T3 chunk plus fixed PID
counter tables. T3's file/session quotas additionally bound disk use.

`subscribe()` returns bounded broadcast `HttpStageResult` events, carrying
`DeviceContext`, generation, connection ID, monotonic elapsed time, status,
result, written body bytes and disconnect reason. `http.first_byte` fires once
per generation, immediately after the first successful body write; headers and
HEAD do not count. T5 can apply this fact to AppState. Other stages cover GET,
HEAD, mismatched peer, rejection, live jump, terminal byte count and disconnect.
No capability, URI, raw address or OS error text is retained.

`diagnostics()` exposes a 256-stage bounded snapshot with `dropped_stages`,
aggregate byte/rejection/mismatch counters and current-generation
`first_get_written`. A lagged event consumer can recover that fact from the
snapshot. T7a owns persistence/export/redaction, outside T4.

## Targeted verification

From this checkout, using the native compiler environment:

```text
cargo fmt --manifest-path rust-runtime/Cargo.toml --check
cargo clippy --manifest-path rust-runtime/Cargo.toml --locked --all-targets -- -D warnings
cargo clippy --manifest-path rust-runtime/Cargo.toml --locked --all-targets --features native-host -- -D warnings
cargo test --manifest-path rust-runtime/Cargo.toml --locked --features native-host --lib dlna::cast_http::
```

The loopback tests exercise real socket backpressure, 3/10-second read pauses,
the real 15-second stall timeout, Windows accepted sockets, HTTP range/header
boundaries, concurrent readers, midstream joins/jumps, bounds and cancellation.

Build a fresh schema-2 test companion in a private output directory with
`xtask libav-companion --prefix <read-only native prefix> --out <owned output> --test`.
On Windows, copy the selected prefix's `avcodec-63.dll`, `avformat-63.dll`,
`avutil-61.dll` and `swresample-7.dll` beside the freshly built companion: the
trusted loader requires a self-contained vendor directory, not PATH resolution.
Set `BILIKARA_TEST_CAST_COMPANION` to that test DLL/library, then run:

```text
cargo test --manifest-path rust-runtime/Cargo.toml --locked --features native-host --lib dlna::cast_http::tests::companion:: -- --ignored --test-threads=1
```

This reuses the companion's fixture export to make three real GOPs, serves
T3-generated file TS, MP4 and live TS to loopback clients, and joins a second GET
while generation is running. It checks PSI/IDR/PCR/counters and verifies
lease/port cleanup. These tests do not qualify physical DLNA receivers or a
different operating system.
