use super::*;
use crate::experimental_libav::{open_input, wire};
use std::{
    ffi::c_void,
    mem::{size_of, transmute},
    path::PathBuf,
    sync::atomic::Ordering,
};
#[repr(C)]
#[derive(Default, Debug)]
struct Inspection {
    status: u32,
    streams: u32,
    first_video_key: u32,
    audio_rate: u32,
    audio_channels: u32,
    duration_us: i64,
    packets: [u64; 2],
}
fn inspect(probe: &LibavMetadataProbe, path: &Path) -> Inspection {
    type Check = unsafe extern "C" fn(*const wire::Request, u32, *mut Inspection) -> u32;
    // SAFETY: explicitly selected test companion; C verifies the private size.
    let check: Check =
        unsafe { transmute(probe._library.symbol(c"bm_test_cast_check_v1").unwrap()) };
    let file = open_input(path).unwrap();
    let fd = probe._library.descriptor(&file).unwrap();
    extern "C" fn never(_: *mut c_void) -> i32 {
        0
    }
    let request = wire::Request {
        fd: fd.fd,
        cancelled: never,
        opaque: std::ptr::null_mut(),
    };
    let mut result = Inspection::default();
    // SAFETY: descriptor/library/result outlive this synchronous invocation.
    assert_eq!(
        unsafe { check(&request, size_of::<Inspection>() as u32, &mut result) },
        0
    );
    assert_eq!(result.status, 0, "{result:?}");
    assert_eq!(result.streams, 2);
    assert_eq!(result.first_video_key, 1);
    assert!(result.packets.iter().all(|n| *n > 0));
    result
}
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/bbdown")
}
fn load() -> LibavMetadataProbe {
    let path = std::env::var_os("BILIKARA_TEST_CAST_COMPANION")
        .expect("actual freshly built test companion");
    // SAFETY: only the explicit build/test environment selects this artifact.
    unsafe { LibavMetadataProbe::load(Path::new(&path)) }.unwrap()
}
fn temporary() -> PathBuf {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).unwrap();
    let name = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let path = std::env::temp_dir().join(format!("bilikara-cast-{name}"));
    std::fs::create_dir(&path).unwrap();
    path
}
fn repeated_video(probe: &LibavMetadataProbe, source: &Path, path: &Path) {
    type Fixture = unsafe extern "C" fn(*const wire::Request, *const std::ffi::c_char) -> u32;
    // SAFETY: explicitly supplied test companion; private test-only signature.
    let create: Fixture =
        unsafe { transmute(probe._library.symbol(c"bm_test_cast_fixture_v1").unwrap()) };
    let file = open_input(source).unwrap();
    let fd = probe._library.descriptor(&file).unwrap();
    extern "C" fn never(_: *mut c_void) -> i32 {
        0
    }
    let request = wire::Request {
        fd: fd.fd,
        cancelled: never,
        opaque: std::ptr::null_mut(),
    };
    let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: descriptor and output string live through this synchronous call.
    assert_eq!(unsafe { create(&request, path.as_ptr()) }, 0);
}
// Parse actual PSI, not just FFmpeg's reported codec names.
fn tables(bytes: &[u8]) {
    let first_pes = bytes.chunks_exact(188).find_map(|p| {
        if p[1] & 0x40 == 0 || p[3] & 0x10 == 0 {
            return None;
        }
        let at = 4 + if p[3] & 0x20 != 0 {
            1 + usize::from(p[4])
        } else {
            0
        };
        if at + 4 <= 188 && p[at..at + 3] == [0, 0, 1] {
            Some(p[at + 3])
        } else {
            None
        }
    });
    assert!(
        first_pes.is_some_and(|id| (0xe0..=0xef).contains(&id)),
        "first media PES must be video"
    );
    let mut pmt = None;
    let mut checked = false;
    for packet in bytes.chunks_exact(188) {
        assert_eq!(packet[0], 0x47);
        let pid = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
        if packet[1] & 0x40 == 0 || packet[3] & 0x10 == 0 {
            continue;
        }
        let mut at = 4;
        if packet[3] & 0x20 != 0 {
            at += 1 + usize::from(packet[4]);
        }
        if at >= 188 {
            continue;
        }
        at += 1 + usize::from(packet[at]);
        if at + 12 > 188 {
            continue;
        }
        let s = &packet[at..];
        if pid == 0 && s[0] == 0 {
            pmt = Some((u16::from(s[10] & 31) << 8) | u16::from(s[11]));
        }
        if Some(pid) == pmt && s[0] == 2 {
            let pcr = (u16::from(s[8] & 31) << 8) | u16::from(s[9]);
            let end = 3 + ((usize::from(s[1] & 15) << 8) | usize::from(s[2])) - 4;
            let mut offset = 12 + ((usize::from(s[10] & 15) << 8) | usize::from(s[11]));
            let mut kinds = Vec::new();
            while offset + 5 <= end {
                let stream = (u16::from(s[offset + 1] & 31) << 8) | u16::from(s[offset + 2]);
                kinds.push(s[offset]);
                if s[offset] == 0x1b {
                    assert_eq!(stream, pcr);
                }
                offset += 5 + ((usize::from(s[offset + 3] & 15) << 8) | usize::from(s[offset + 4]));
            }
            assert_eq!(kinds, [0x1b, 0x0f]);
            checked = true;
        }
    }
    assert!(checked, "PAT/PMT must precede a joinable group");
}
fn clocks(bytes: &[u8]) {
    const WRAP: u64 = (1u64 << 33) * 300;
    let mut pcrs = Vec::new();
    let mut pats = Vec::new();
    for (index, p) in bytes.chunks_exact(188).enumerate() {
        let pid = (u16::from(p[1] & 31) << 8) | u16::from(p[2]);
        if p[3] & 0x20 != 0 && p[4] >= 7 && p[5] & 0x10 != 0 {
            let base = (u64::from(p[6]) << 25)
                | (u64::from(p[7]) << 17)
                | (u64::from(p[8]) << 9)
                | (u64::from(p[9]) << 1)
                | (u64::from(p[10]) >> 7);
            let ext = (u64::from(p[10] & 1) << 8) | u64::from(p[11]);
            pcrs.push((index as u64, base * 300 + ext));
        }
        if pid == 0 && p[1] & 0x40 != 0 {
            pats.push(index as u64);
        }
    }
    assert!(pcrs.len() > 10);
    assert!(pats.len() > 2);
    let mut packet_ticks = 0;
    for pair in pcrs.windows(2) {
        let delta = (pair[1].1 + WRAP - pair[0].1) % WRAP;
        let tick = delta.div_ceil(pair[1].0 - pair[0].0);
        // libav checks PCR around PSI insertion. Bound the configured 20 ms
        // plus at most four TS packets (PAT/PMT/SDT and packet quantization).
        assert!(
            delta > 0 && delta <= 20 * 27_000 + 4 * tick,
            "PCR interval {} us",
            delta / 27
        );
        packet_ticks = packet_ticks.max(tick);
    }
    for pair in pats.windows(2) {
        let delta = (pair[1] - pair[0]) * packet_ticks;
        assert!(
            delta <= 50 * 27_000 + 4 * packet_ticks,
            "PAT interval {} us",
            delta / 27
        );
    }
}
#[test]
#[ignore = "requires actual cast test companion; run through test:libav-prerequisites"]
fn real_cast_mux_contract() {
    let probe = load();
    assert!(probe.cast_mux_available());
    let root = temporary();
    let cache = root.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let fixture = fixtures();
    let stop = AtomicBool::new(false);
    let session = CastSession::new(&cache, 32 * 1024 * 1024).unwrap();
    let repeated = root.join("three-gops.mp4");
    repeated_video(
        &probe,
        &fixture.join("video-reordered-start.mp4"),
        &repeated,
    );
    let preflight = probe.cast_preflight(&repeated, &stop).unwrap();
    assert_eq!(preflight.packets, 72);
    assert!(preflight.has_b_frames);
    assert!((preflight.duration_us - 3_003_000).abs() < 1000);
    assert!((preflight.gop_us.unwrap() - 1_001_000).abs() < 1000);
    for video in [fixture.join("video-reordered-start.mp4"), repeated.clone()] {
        for (audio, transcoded) in [
            ("audio.m4a", false),
            ("audio-flac.mp4", true),
            ("audio-eac3.m4a", true),
        ] {
            for format in [CastFormat::Ts, CastFormat::Mp4] {
                let inputs = CastInputs {
                    video: &video,
                    audio: &fixture.join(audio),
                    start_us: 0,
                };
                let output = session
                    .mux(&probe, &inputs, format, &stop)
                    .unwrap_or_else(|e| panic!("{video:?}/{audio}/{format:?}: {e}"));
                let checked = inspect(&probe, output.path());
                let source = probe.cast_preflight(&video, &stop).unwrap();
                let expected = source.duration_us.max(if audio == "audio-eac3.m4a" {
                    2_048_000 - source.source_start_us
                } else {
                    1_000_000 - source.source_start_us
                });
                assert!(
                    (checked.duration_us - expected).abs() <= 100_000,
                    "input/output duration: {checked:?} expected {expected}"
                );
                assert_eq!(output.summary.audio_transcoded, transcoded);
                if transcoded {
                    assert_eq!(checked.audio_rate, 48000);
                    assert_eq!(checked.audio_channels, 2);
                }
                assert!(
                    (checked.duration_us - output.summary.duration_us).abs() <= 100_000,
                    "{video:?}/{audio}/{format:?}: {checked:?} {:?}",
                    output.summary
                );
                let bytes = std::fs::read(output.path()).unwrap();
                if format == CastFormat::Ts {
                    tables(&bytes);
                    clocks(&bytes);
                } else {
                    let mut offset = 0;
                    let mut atoms = Vec::new();
                    while offset + 8 <= bytes.len() {
                        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
                            as usize;
                        assert!(size >= 8);
                        atoms.push(&bytes[offset + 4..offset + 8]);
                        offset += size;
                    }
                    assert!(
                        atoms.iter().position(|a| *a == b"moov")
                            < atoms.iter().position(|a| *a == b"mdat")
                    );
                }
                let path = output.path().to_owned();
                drop(output);
                assert!(!path.exists());
            }
        }
    }
    let video = fixture.join("video-reordered-start.mp4");
    let audio = fixture.join("audio.m4a");
    let inputs = CastInputs {
        video: &video,
        audio: &audio,
        start_us: 0,
    };
    let mut chunks = Vec::new();
    let mut keys = 0;
    probe
        .cast_ts(
            &inputs,
            CastPacing::FullSpeed,
            8 * 1024 * 1024,
            &stop,
            &mut |bytes, _, key| {
                keys += usize::from(key);
                chunks.extend_from_slice(bytes);
                true
            },
        )
        .unwrap();
    assert!(keys >= 1);
    let repeated_inputs = CastInputs {
        video: &repeated,
        audio: &fixture.join("audio-eac3.m4a"),
        start_us: 0,
    };
    let mut stream = Vec::new();
    let mut joins = Vec::new();
    probe
        .cast_ts(
            &repeated_inputs,
            CastPacing::FullSpeed,
            8 * 1024 * 1024,
            &stop,
            &mut |bytes, _, key| {
                if key {
                    joins.push(stream.len());
                }
                stream.extend_from_slice(bytes);
                true
            },
        )
        .unwrap();
    assert_eq!(joins.len(), 3);
    let joined = root.join("join.ts");
    std::fs::write(&joined, &stream[joins[1]..]).unwrap();
    tables(&stream[joins[1]..]);
    inspect(&probe, &joined);
    let seek_inputs = CastInputs {
        start_us: 1_500_000,
        ..repeated_inputs
    };
    let seek_file = session
        .mux(&probe, &seek_inputs, CastFormat::Ts, &stop)
        .unwrap();
    assert!((seek_file.summary.source_start_us - 1_001_000).abs() < 1000);
    inspect(&probe, seek_file.path());
    drop(seek_file);

    tables(&chunks);
    assert!(matches!(
        probe.cast_ts(
            &inputs,
            CastPacing::FullSpeed,
            8 * 1024 * 1024,
            &stop,
            &mut |_, _, _| false
        ),
        Err(ProbeError::Cancelled)
    ));
    assert!(matches!(
        probe.cast_ts(
            &inputs,
            CastPacing::FullSpeed,
            8 * 1024 * 1024,
            &stop,
            &mut |_, _, _| panic!("caught callback panic")
        ),
        Err(ProbeError::BackendFailure(_))
    ));
    let measured = session.mux(&probe, &inputs, CastFormat::Ts, &stop).unwrap();
    let quota = measured.summary.bytes;
    drop(measured);
    let bounded = CastSession::new(&cache, quota).unwrap();
    let held = bounded.mux(&probe, &inputs, CastFormat::Ts, &stop).unwrap();
    assert!(bounded.mux(&probe, &inputs, CastFormat::Ts, &stop).is_err());
    drop(held);
    let held = bounded.mux(&probe, &inputs, CastFormat::Ts, &stop).unwrap();
    let held_path = held.path().to_owned();
    drop(bounded);
    assert!(held_path.exists());
    drop(held);
    assert!(!held_path.exists());
    stop.store(true, Ordering::Release);
    for format in [CastFormat::Ts, CastFormat::Mp4] {
        assert!(matches!(
            session.mux(&probe, &inputs, format, &stop),
            Err(ProbeError::Cancelled)
        ));
    }
    stop.store(false, Ordering::Release);
    let tiny = CastSession::new(&cache, 188).unwrap();
    for format in [CastFormat::Ts, CastFormat::Mp4] {
        assert!(tiny.mux(&probe, &inputs, format, &stop).is_err());
    }
    drop(tiny);
    let hevc = fixture.join("video-hevc.mp4");
    let unsupported = CastInputs {
        video: &hevc,
        audio: &audio,
        start_us: 0,
    };
    assert!(matches!(
        session.mux(&probe, &unsupported, CastFormat::Ts, &stop),
        Err(ProbeError::Media(crate::MediaError {
            kind: crate::MediaErrorKind::UnsupportedCodec,
            ..
        }))
    ));
    let mut generated = 0;
    let began = std::time::Instant::now();
    let worker_stop = &stop;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(80));
            worker_stop.store(true, Ordering::Release);
        });
        assert!(matches!(
            probe.cast_ts(
                &inputs,
                CastPacing::Realtime {
                    burst: Duration::ZERO
                },
                8 * 1024 * 1024,
                &stop,
                &mut |_, _, _| {
                    generated += 1;
                    true
                }
            ),
            Err(ProbeError::Cancelled)
        ));
    });
    assert!(began.elapsed() < Duration::from_millis(500));
    assert!(generated > 0);
    drop(session);
    assert_eq!(std::fs::read_dir(cache.join(".cast")).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}
