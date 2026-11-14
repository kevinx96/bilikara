//! Real schema-2 companion, local fixtures, actual loopback HTTP delivery. This
//! is simulated receiver evidence, never DLNA hardware qualification.
use super::*;
use crate::experimental_libav::{
    LibavMetadataProbe,
    cast_mux::{CastFormat, CastInputs, CastSession},
};
use std::sync::atomic::AtomicBool;

fn probe() -> LibavMetadataProbe {
    let path = std::env::var_os("BILIKARA_TEST_CAST_COMPANION")
        .expect("explicit freshly built test companion");
    // SAFETY: the operator selects the locally built, trusted test library.
    unsafe { LibavMetadataProbe::load(Path::new(&path)) }.unwrap()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/bbdown")
}

// Private test-shim ABI from media-libav/probe.h and test_shim.c. Reuse the
// companion fixture export without changing T3 or adding an encoder/CLI.
#[repr(C)]
struct FixtureRequest {
    fd: i32,
    cancelled: extern "C" fn(*mut std::ffi::c_void) -> i32,
    opaque: *mut std::ffi::c_void,
}
extern "C" fn never_cancel(_: *mut std::ffi::c_void) -> i32 {
    0
}
type MakeFixture = unsafe extern "C" fn(*const FixtureRequest, *const std::ffi::c_char) -> u32;

fn repeated_video(_probe: &LibavMetadataProbe, source: &Path, destination: &Path) {
    let file = std::fs::File::open(source).unwrap();
    let output = std::ffi::CString::new(destination.to_str().unwrap()).unwrap();
    let path = std::env::var_os("BILIKARA_TEST_CAST_COMPANION").unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        let wide: Vec<u16> = path.encode_wide().chain(Some(0)).collect();
        // SAFETY: _probe keeps this already validated module alive. Resolve
        // only its known test/descriptor ABI. Its /MD UCRT duplicates the
        // borrowed handle; no Rust CRT fd crosses the boundary.
        let status = unsafe {
            let module = GetModuleHandleW(wide.as_ptr());
            assert!(!module.is_null());
            let make: MakeFixture = std::mem::transmute(
                GetProcAddress(module, c"bm_test_cast_fixture_v1".as_ptr().cast()).unwrap(),
            );
            let open: unsafe extern "C" fn(*mut std::ffi::c_void) -> i32 = std::mem::transmute(
                GetProcAddress(module, c"bm_fd_from_handle_v1".as_ptr().cast()).unwrap(),
            );
            let close: unsafe extern "C" fn(i32) = std::mem::transmute(
                GetProcAddress(module, c"bm_fd_close_v1".as_ptr().cast()).unwrap(),
            );
            let fd = open(file.as_raw_handle());
            assert!(fd >= 0);
            let status = make(
                &FixtureRequest {
                    fd,
                    cancelled: never_cancel,
                    opaque: std::ptr::null_mut(),
                },
                output.as_ptr(),
            );
            close(fd);
            status
        };
        assert_eq!(status, 0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::{ffi::OsStrExt, io::AsRawFd};
        let path = std::ffi::CString::new(path.as_bytes()).unwrap();
        // SAFETY: the trusted test ABI above; fd/strings live through this
        // synchronous call. Balance dlopen's extra module reference.
        let status = unsafe {
            let module = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            assert!(!module.is_null());
            let symbol = libc::dlsym(module, c"bm_test_cast_fixture_v1".as_ptr());
            assert!(!symbol.is_null());
            let make: MakeFixture = std::mem::transmute(symbol);
            let status = make(
                &FixtureRequest {
                    fd: file.as_raw_fd(),
                    cancelled: never_cancel,
                    opaque: std::ptr::null_mut(),
                },
                output.as_ptr(),
            );
            libc::dlclose(module);
            status
        };
        assert_eq!(status, 0);
    }
}

fn inspect_ts(data: &[u8]) -> u64 {
    assert!(continuity::valid(data));
    let mut counters = HashMap::new();
    let mut pmt = None;
    let mut codecs = Vec::new();
    let mut first_video = Vec::new();
    let mut video_pid = None;
    let mut video_complete = false;
    let mut pcrs = Vec::new();
    for p in data.chunks_exact(188) {
        let pid = (u16::from(p[1] & 31) << 8) | u16::from(p[2]);
        if pid == 8191 {
            continue;
        }
        let payload = p[3] & 16 != 0;
        if let Some(previous) = counters.get(&pid) {
            assert_eq!(
                p[3] & 15,
                if payload {
                    (previous + 1) & 15
                } else {
                    *previous
                },
                "PID {pid}"
            );
        }
        if payload {
            counters.insert(pid, p[3] & 15);
        }
        let mut at = 4;
        if p[3] & 32 != 0 {
            if p[4] >= 7 && p[5] & 16 != 0 {
                let base = (u64::from(p[6]) << 25)
                    | (u64::from(p[7]) << 17)
                    | (u64::from(p[8]) << 9)
                    | (u64::from(p[9]) << 1)
                    | (u64::from(p[10]) >> 7);
                let extension = (u64::from(p[10] & 1) << 8) | u64::from(p[11]);
                pcrs.push(base * 300 + extension);
            }
            at += 1 + usize::from(p[4]);
        }
        if !payload || at >= 188 {
            continue;
        }
        if p[1] & 64 != 0 && pid == 0 {
            let table = &p[at + 1 + usize::from(p[at])..];
            assert_eq!(table[0], 0);
            pmt = Some((u16::from(table[10] & 31) << 8) | u16::from(table[11]));
        } else if p[1] & 64 != 0 && Some(pid) == pmt {
            let table = &p[at + 1 + usize::from(p[at])..];
            assert_eq!(table[0], 2);
            let end = 3 + ((usize::from(table[1] & 15) << 8) | usize::from(table[2])) - 4;
            let mut offset = 12 + ((usize::from(table[10] & 15) << 8) | usize::from(table[11]));
            codecs.clear();
            while offset + 5 <= end {
                codecs.push(table[offset]);
                offset += 5
                    + ((usize::from(table[offset + 3] & 15) << 8) | usize::from(table[offset + 4]));
            }
        } else if !video_complete
            && p[1] & 64 != 0
            && p[at..].starts_with(&[0, 0, 1])
            && (0xe0..=0xef).contains(&p[at + 3])
        {
            if video_pid.is_none() {
                video_pid = Some(pid);
                first_video.extend_from_slice(&p[at..]);
            } else {
                video_complete = true;
            }
        } else if !video_complete && Some(pid) == video_pid {
            first_video.extend_from_slice(&p[at..]);
        }
    }
    assert!(pmt.is_some(), "PAT present on join");
    assert_eq!(codecs, [0x1b, 0x0f]);
    assert!(
        first_video
            .windows(4)
            .any(|w| w[..3] == [0, 0, 1] && w[3] & 31 == 5),
        "first video PES includes IDR"
    );
    assert!(pcrs.len() > 2);
    const WRAP: u64 = (1u64 << 33) * 300;
    assert!(
        pcrs.windows(2)
            .all(|p| (p[1] + WRAP - p[0]) % WRAP < 27_000_000),
        "PCR monotonic including wrap"
    );
    pcrs[0]
}

#[test]
#[ignore = "requires freshly built native schema-2 test companion"]
fn real_companion_file_ts_mp4_live_http_and_t3_lease_cleanup() {
    run(async {
        let temp = Temp::new();
        let probe = probe();
        let fixtures = fixtures();
        // T3's real cast fixture: 24 H.264 packets with B frames / reordered
        // timestamps. The older one-sample BBDown stub is not cast media.
        let video = temp.0.join("three-gops.mp4");
        repeated_video(&probe, &fixtures.join("video-reordered-start.mp4"), &video);
        let preflight = probe
            .cast_preflight(&video, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(preflight.packets, 72);
        assert!(preflight.has_b_frames);
        assert!((preflight.gop_us.unwrap() - 1_001_000).abs() < 1000);
        let audio = fixtures.join("audio.m4a");
        let inputs = CastInputs {
            video: &video,
            audio: &audio,
            start_us: 0,
        };
        let session = CastSession::new(&temp.0, 16 * 1024 * 1024).unwrap();
        let endpoint = endpoint().await;
        let address = endpoint.local_addr();
        for (id, format) in [(1, CastFormat::Ts), (2, CastFormat::Mp4)] {
            let file = session
                .mux(&probe, &inputs, format, &AtomicBool::new(false))
                .unwrap();
            let path = file.path().to_owned();
            let expected = std::fs::read(&path).unwrap();
            let media = endpoint.publish_file(id, file, TsMime::Mp2t).unwrap();
            let response = request(&endpoint, "GET", &media.uri, "").await;
            assert_eq!(response.status, 200);
            assert_eq!(
                response.headers["content-length"],
                expected.len().to_string()
            );
            assert_eq!(response.body().await, expected);
            let response = request(&endpoint, "GET", &media.uri, "Range: bytes=1-13\r\n").await;
            assert_eq!(response.status, 206);
            assert_eq!(response.body().await, &expected[1..14]);
            assert!(
                request(&endpoint, "HEAD", &media.uri, "")
                    .await
                    .body()
                    .await
                    .is_empty()
            );
            endpoint.clear_generation();
            until(|| !path.exists()).await;
        }
        let stream = Arc::new(
            LiveStream::new(LiveOptions {
                burst: Duration::ZERO,
                ..LiveOptions::default()
            })
            .unwrap(),
        );
        let media = endpoint
            .publish_live(3, stream.clone(), TsMime::Mp2t)
            .unwrap();
        let mut response = request(&endpoint, "GET", &media.uri, "Range: bytes=9999999-\r\n").await;
        assert_eq!(response.status, 200);
        assert!(!response.headers.contains_key("content-length"));
        let producer = stream.clone();
        let thread = tokio::task::spawn_blocking(move || {
            let probe = super::companion::probe();
            producer.generate(
                &probe,
                &CastInputs {
                    video: &video,
                    audio: &audio,
                    start_us: 0,
                },
                16 * 1024 * 1024,
            )
        });
        let reader = tokio::spawn(async move {
            let mut ts = Vec::new();
            timeout(Duration::from_secs(8), response.socket.read_to_end(&mut ts))
                .await
                .unwrap()
                .unwrap();
            ts
        });
        until(|| stream.snapshot().keyframes >= 2).await;
        assert!(!stream.snapshot().finished);
        let joined = request(&endpoint, "GET", &media.uri, "Range: bytes=-0\r\n").await;
        assert_eq!(joined.status, 200);
        let joined = joined.body().await;
        let ts = reader.await.unwrap();
        thread.await.unwrap().unwrap();
        let initial_pcr = inspect_ts(&ts);
        let join_pcr = inspect_ts(&joined);
        const WRAP: u64 = (1u64 << 33) * 300;
        assert!(
            (join_pcr + WRAP - initial_pcr) % WRAP > 500_000 * 27,
            "second HTTP GET joined a later real GOP"
        );
        assert!(stream.snapshot().start.is_some());
        let diag = endpoint.shutdown().await;
        assert!(diag.first_get_written);
        assert!(
            diag.stages
                .iter()
                .any(|e| e.generation == Some(3) && e.stage == HttpStage::FirstByte)
        );
        drop(session);
        assert_eq!(std::fs::read_dir(temp.0.join(".cast")).unwrap().count(), 1); // stable .lock
        assert!(TcpListener::bind(address).await.is_ok());
    });
}
