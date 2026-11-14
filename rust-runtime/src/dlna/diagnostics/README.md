# T7a session diagnostic contract (schema 1)

`SessionTrace` is an independent per-session service. It does not own playback,
run self-tests, change AppState, log inputs, or start background tasks. T5/T8
retain it and await their admitted workers before calling `finish(cancelled)`.
T8 supplies environment, samples, S0–S7 completion/skip status and user answers.
Unknown facts remain JSON `null`; missing measurements are never success.

## Collection

1. Construct `SessionTrace::new(platform, target_kind, app_version, os_version)`.
   Entropy failure is explicit. Platform/target are enums. Version inputs accept
   only one to three numeric components and optional known release channels
   (`preview`, `alpha`, `beta`, `rc` plus a number); no IP/build/user/path text.
2. `begin_connection()` returns the connection attempt ID. In discovery's live
   callback forward each `DiscoveryUpdate` to `discovery(id, &update)`.
   `StagesDropped(n)` is cumulative per scan; repeated counts are not summed.
   Live `Stage` records still arrive after T2's returned stage buffer fills.
   `discovery_facts` supplies scan duration and before/after target presence.
3. Forward T9 `PureKOperation.stages` via `record_purek_operation`, or each
   `PhaseResult` via `purek`. Parse, resolution, route, pre-auth description,
   registration, confirmation, announcement, health and revoke remain distinct.
   Pre/post service summaries retain known service kinds/version, and re-key
   unknown service digests. QR fields retain presence/count/class facts only.
4. Call `renderer(&renderer)` on discovered or QR-directed descriptions. This
   retains route/address-class/subnet facts and bounded metadata projections.
5. `begin_output(context, config)` returns a separate output ID. Forward T2
   control results using `record_operation(connection, Some(output), &op)`.
   Call `capabilities` with the query state and returned sink list; only MIME
   categories are retained. Report transport/position results through both the
   ordinary operation adapter (status/duration) and `transport` (typed values).
6. Capture `trace.elapsed()` immediately before endpoint bind. Feed its initial
   listener record once, then subscribe **before publication**. Await
   `receive_http(output, endpoint_origin, receiver, cancel)` in the owning worker,
   or forward events via `http_at(output, session_completion_offset, &event)`.
   The approximate pre-bind origin is for session ordering, not a first-byte
   latency benchmark. `HttpStageResult.elapsed` is endpoint uptime, not request
   duration; it is exported as `facts.endpoint_elapsed_ms`, with duration `null`.
   The receiver drains ready events on cancellation and counts broadcast lag.
   Do not replay an endpoint snapshot already ingested through the subscription.
7. Use `first_ts_packet`, `failure`, `sample`, `environment`, `self_test_step`,
   `observations` and `complete_output` for facts owned by the media/T8 worker.
   Complete a cell before starting the next. Five unique cells are retained
   independently of the current/previous ordinary output pair. Completed-cell
   timing is frozen; later cells cannot turn an earlier result into a timeout.
8. `report().json()` returns `{ "diagnostics": { "cast": ..., "cast_self_test": ... } }`.
   Both sections have `schema_version: 1`. `report().text()` returns the safe
   readable summary. `report().artifact(&DiagnosticRequest)` uses the existing
   artifact assembler and adds `diagnostics.cast.json` and `diagnostics.cast.txt`
   to the ZIP. The cast attachment can only be supplied as an immutable `Report`,
   never arbitrary frontend JSON. T5 will connect the active session later.

Batch operations and live callbacks are **alternative** ingestion paths. Never
ingest the same T2 operation twice. T2 lacks absolute timestamps: the batch
adapter reconstructs sequential completions from durations, while live callers
may use `record_stage_at` for measured completion offsets. `purek` records receipt
offsets and preserves each operation duration. Events sort by completion offset,
then a locked sequence number. Distinct `device_key`s stay distinct: callers must
not assume T9's separately salted keys equal T2's discovery keys for one device.

## Bounds and loss

The event buffer retains the first 512 events by default (configurable 0–4096),
counting omissions in `dropped_stages`, like T2's returned stage retention.
It also retains 128 device projections, six connection summaries, two output
summaries, five matrix cells, 24 actions/output, 16 disconnects/output, 32
transport states/output, 32 interface classes and eight unique S0–S7 results.
Known MIME classes are a fixed enum set. Counters saturate. Output aggregate
counts and final classification continue updating even when stage retention
fills. Source-buffer omissions and broadcast lag are distinct fields
`source_dropped_stages` and `delivery_lagged_stages`; they do not claim that live
events were lost when T2 merely omitted them from its return value.

## Privacy

All raw device keys (including URL-derived unsalted keys without USN), UDN
digests and unknown-service digests are re-keyed with HMAC-SHA256 and a fresh
32-byte OS-random session key. The first 16 bytes identify an entry. Domain
separation keeps device/UDN/service/metadata namespaces separate. The key and
original identifiers never enter retained state, JSON, summary, Debug or logs.
Repeated snapshots of one session correlate; fresh sessions do not correlate.
The HMAC primitive has an RFC 4231 test vector. No new crypto dependency is used.

Before entering retained diagnostic state, manufacturer/model/modelDescription
and SERVER discard Unicode control characters and truncate to 256 Unicode
characters; friendlyName uses the report's stricter 64-character limit. These
arbitrary fields can themselves be tokens, room names or usernames. Therefore
each exports `{digest, characters, truncated, controls_removed}` rather than
text. `vendor_class` remains readable. This intentionally exceeds simple regex
redaction: even an unlabeled opaque secret never escapes in device metadata.
T1 parsing is unchanged. No URL, port, raw error/body, QR payload, room/user,
MAC, local path or address enters the report. Only enums, numbers, booleans,
internally generated local-time timestamps and pseudonyms are serialized.

## Result interpretation and upstream gaps

Priority follows the plan/next trace: preparation → control → pipeline →
before-play/starting → not-requested → zero-delivery → stopped →
delivered-unverified. No GET produces `cast_not_requested` only 10 seconds after
a successful Play; GET without body delivery produces `cast_no_delivery` only
after 10 seconds. Rejections are `cast_request_rejected`. Stop failure is kept
with status/Fault/disposition, but never becomes an output control failure.
Ignored/retrying failures do not override successful fallback actions. Terminal
T4 write-stall/peer-close/read/write failure maps to `cast_interrupted`; normal
EOF, cancellation and publication replacement do not prove receiver failure.
`write_error` is **not** relabeled `reset`, because T4 exposes no reset subtype.

`media_time_us` is retained unchanged as a **DTS watermark**, with its full
`live_start` basis; it is not PTS and may lead presentation by about 66.7 ms.
No synthetic correction is added. T4 emits first-byte once per generation and
byte counts at connection completion. Before completion, `bytes_written` is a
lower bound using the first successful write; terminal counts replace that
bound without double-counting the first write. `bytes_observed_ms` is receipt of the
terminal byte-count event, **not** the last successful write. Range request
counts, peer /24 relationship, actual last-write time and maximum simultaneous
GETs/preemptions are `null` until T8 supplies measurements via `RequestObservations`.
T4's `peer_mismatch` supports only a mismatch count. `header_preempted` counts
header-slot eviction only. T4 currently cannot
distinguish GET preemption from other replacement using its exposed facts.
These gaps do not justify changing reviewed T4 code in T7a.

The §7.6 conclusion uses user answers for learned modes/variants; delivery alone
never proves a visible picture. Unanswered/uncertain picture questions cannot
produce “received but did not decode.” Results and mode are report suggestions,
not persistent device preferences. No physical receiver is qualified by the
loopback tests. No T8 runner, UI or AppState state is implemented here.

## Verification

`cargo test --manifest-path rust-runtime/Cargo.toml --locked --offline --features native-host --lib dlna::diagnostics::`
runs the real loopback discovery → registration/confirmation → Stop/Set/Play →
endpoint GET/body → revoke flow, ZIP/schema, privacy, HMAC correlation,
Unicode/control handling, stage priority, bounded/concurrent retention tests.
The three additive test-only module entries in T2/T4/T9 allow new fixtures to
use their private loopback policy without widening any production interface or
changing upstream logic/tests. No companion build or physical receiver is needed.
