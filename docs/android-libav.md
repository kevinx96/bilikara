# Android libav construction

`xtask android-libav` builds FFmpeg 9.0.1 and the C media companion for Android
API 24, using NDK **27.0.12077973**. Supported APK ABIs are `arm64-v8a` and
`x86_64`. This is a host-native construction tool, independent of the application
Runtime and desktop preparation. The recipe is adapted from bilikara-next's
MIT implementation; its FFmpeg whitelist and upstream C cache identity remain
aligned with that implementation.

Install the repository's pinned Rust toolchain, Node version from `.nvmrc`, the
NDK, and `bash`, `make`, `gcc`, `tar`, `curl`, `gpg` and `gpgv`. On Windows use
MSYS2 for the POSIX tools and initialize the native MSVC environment before
building xtask. Put MSYS2 `usr/bin` on PATH after the MSVC tool directories.
Run Cargo from the checkout root. Pass absolute, non-overlapping NDK, output,
cache and optional source paths. Use the NDK's normal native path spelling;
the compiler discovers its own sysroot from its executable path. The tool keeps
canonical paths for validation and uses relative compiler inputs and outputs
under its owned build directory, without stripping Windows verbatim prefixes.
FFmpeg's embedded configure string uses the fixed logical prefix
`/bilikara-android-libav`; installation uses a separate temporary `DESTDIR`.
The temporary directory therefore does not become part of the library's
configuration or the companion's configuration handshake.

```text
cargo run --manifest-path xtask/Cargo.toml --locked --target host-tuple -- android-libav --ndk <absolute-ndk> --abi arm64-v8a --out <absolute-output> --cache <absolute-cache>
```

For offline builds append `--source-dir <absolute-source-directory>` containing
all three original files: `ffmpeg-9.0.1.tar.xz`, `ffmpeg-9.0.1.tar.xz.asc`, and
`ffmpeg-devel.asc`. The archive must match SHA-256
`cf38e0e28c7e5605942c4a77755349b0145804a397af37eb1fb4c77cb237f635` and its signature
must match fingerprint `FCF986EA15E6E293A5644F10B4322F04D67658D8` before extraction.
Without the option the tool downloads these pinned inputs from ffmpeg.org.

The default output is:

```text
<output>/
  build-info.json
  jniLibs/<abi>/
    libavformat.so
    libavcodec.so
    libavutil.so
    libswresample.so
    libbilikara_media_libav.so
  assets/bilikara-media/<abi>/
    build-info.json
    licenses/libav-source.txt
    licenses/libav/...
    licenses/THIRD_PARTY_NOTICES.md
  sources/bilikara-android-libav-9.0.1-<abi>-source-kit.zip
```

ELF verification checks the selected machine, LOAD alignment of at least 16 KB,
16 KB aligned GNU_RELRO end, exact unversioned SONAME, declared dependencies
(these five libraries plus `libc.so`, `libm.so`, `libdl.so`), and companion
exports. `--verify-only` repeats these inspections and verifies the complete
output inventory. A failed build preserves the previous validated output;
unrelated or modified output directories are refused rather than overwritten.

The default profile has no Signalsmith or `libc++_shared.so`. The reserved
`--with-signalsmith` path preserves next's downstream build structure, but
requires its companion C++ source and vendored license files; this checkout
does not provide them, so explicitly selecting it fails. It does not change
the upstream C cache key.

## Cache and Gradle

Append `--cache-key` to the same command to print only
`key=android-libav-v1-<abi>-<sha256>`. This validates the selected NDK and reads
its compiler version but performs no download or build. Inputs are the pinned
source digest, NDK version, ABI, API 24, FFmpeg configure list, 16 KB page size
and compiler-version output. Changing application, Runtime or companion code
does not rotate this key. Compiler output can differ between host installations;
use the actual emitted key, not a guessed cross-host key.

Cache `<cache>/<key>/` contains only upstream C libraries, headers, signed
source, licenses and build records plus `cache-manifest.json`. Cache hits check
inventory, hashes, source identity and configuration and skip FFmpeg compilation.
The fixed-prefix and binary-path checks also run on cache hits. Older caches
with a machine-specific prefix are rejected with their path and instructions
to select a fresh `--cache` directory. The cache key format is unchanged; an
old cache is never silently reused or overwritten to bypass these checks.
The companion, licenses and source kit are rebuilt after a hit. Never cache
`jniLibs` or an application binary as part of this C cache.

`src-tauri/gen/android/app/media.gradle.kts` runs the host-native xtask before
`merge*JniLibFolders` and `merge*Assets`, with Tauri's `abiList`. It pins the NDK
and sets `packaging.jniLibs.useLegacyPackaging = true`, so the trusted Android
loader can access regular files in `ApplicationInfo.nativeLibraryDir`.

- Gradle output: `src-tauri/gen/android/app/build/generated/bilikara-media/<abi>/`.
- Default C cache: `src-tauri/gen/android/.gradle/bilikara-libav-cache`.
- `BILIKARA_ANDROID_LIBAV_CACHE`: optional absolute C cache directory.
- `BILIKARA_LIBAV_SOURCE_DIR`: optional absolute offline source directory.

Only the generated `jniLibs` and `assets` subdirectories enter the APK. Publish
the generated source-kit ZIP **separately beside the corresponding APK at the
same download location**. The APK's license notice identifies the exact ZIP,
upstream source, version, hash, signer and configure string. The ZIP includes
the original signed archive, licenses, configuration records, companion source,
xtask sources and lockfile, and `REBUILD-ANDROID.md`. Rebuild from the extracted
kit root with its `source/` directory and a fresh output/cache.

## Verification

```text
cargo test --manifest-path xtask/Cargo.toml --locked
npm run test:android-libav
npm run test:desktop-build
npm run tauri -- android build --debug --apk --target aarch64
```

Set `BILIKARA_TEST_ANDROID_LIBAV_DIR` to the absolute real-built
`<output>/jniLibs/<abi>` directory when running `npm run test:android-libav`.
This enables the binary-string acceptance test for the five real libraries:
it rejects build-directory/home paths and the current builder's username,
and requires the fixed FFmpeg prefix. Without that input the real-library
test explicitly skips; synthetic ELF tests alone do not qualify this check.

After a real NDK build, repeat the xtask command with `--verify-only`; rerun
without that flag and confirm `Android libav C cache hit` with no FFmpeg rebuild.
For the resulting APK run the selected Android build-tools `apksigner verify`
and `zipalign -c -P 16 -v 4 <apk>`. Inspect the extracted five media libraries
with the selected NDK's `llvm-readelf -W -h -l -d`, and verify the APK's licenses
and absence of a source-kit ZIP in assets. The Rust application `.so` is an
additional APK library outside the five-library media output. Fixture tests
do not qualify an APK, Android loading, or physical DLNA operation.

## Android runtime capability

Before starting the Host, the shell reads `ApplicationInfo.nativeLibraryDir`
through JNI and selects `libbilikara_media_libav.so` in that directory.
`useLegacyPackaging=true` makes this an installed regular file. No JavaScript,
HTTP request, application-data directory or PATH lookup selects executable code.
The startup negotiation checks the same ABI/build contract used on desktop.

Android cache normalization, media inspection, automatic volume and casting
share this companion configuration. Missing libraries, loader errors and ABI
failures do not block Host startup. Normalization/inspection retain their existing
Rust compatibility route and record the fallback reason. Automatic volume has
no separate Rust decoder: when the companion PCM capability is unavailable,
the scanner remains unavailable, with an `automatic-volume-unavailable` event.
`android-media-capability` records startup capabilities and a bounded reason
code without loader paths. S0 independently checks the shared cast probe;
unavailable casting returns `cast_backend_unavailable`.

Negotiation failures use structured ABI, runtime-configuration and layout/build
reason codes. The process caches failed companion loads until the installed
file's size, modification/creation time or availability changes. Repeated
normalization, inspection and casting attempts reuse that failure; automatic
volume reports unavailability once per file identity, including its startup
report. A changed file permits another load and another failure report. Media
errors are not cached, and successful handles keep their normal lifetime.

Existing cache keys, filenames, reuse checks and immutable media serving do not
change. Switching to libav does not by itself re-download or re-normalize cache.
Existing caches are deliberately retained without re-normalization. New libav
outputs preserve source presentation offsets and AAC priming edits that the
former Rust normalization omitted. Android local audio/video alignment can
therefore shift by approximately **45–67 ms for B-frame content**, or about
**146 ms for the fragmented sample**. These are fixture-specific differences,
not a universal correction. Old and newly cached songs can differ by tens of
milliseconds; testers who previously calibrated audio/video delay may need to
adjust it when comparing these songs.

### Runtime regression checks

Build a same-source native **test** companion, keeping shared library prefixes
read-only. On Windows, enter the x64 MSVC environment with `VSLANG=1033`, put
the prepared prefix's `bin` on PATH for construction only, and run:

```text
cargo run --manifest-path xtask/Cargo.toml --locked --target x86_64-pc-windows-msvc -- libav-companion --prefix <absolute-prefix> --test --out <absolute-owned-directory>
```

Copy that prefix's `avcodec-63.dll`, `avformat-63.dll`, `avutil-61.dll` and
`swresample-7.dll` beside `bilikara_media_libav_test.dll`. Remove the prefix from
PATH for the tests, and set `BILIKARA_TEST_CAST_COMPANION` to the test DLL.
The startup matrix compiles `tests/fixtures/libav_incompatible.c` into an owned
temporary directory and removes it afterward. It uses the native `cl.exe` on
Windows or `cc` on POSIX (`.dll`, `.so` or `.dylib` as appropriate). This fixture
exports ABI version 0, including the required Windows file-descriptor bridge,
so it tests actual ABI rejection rather than a missing symbol or dependency.
No additional fixture environment variable is required. Then run:

```text
cargo test --manifest-path rust-runtime/Cargo.toml --lib --features native-host --locked media_routing::android_tests:: -- --include-ignored --nocapture
cargo test --manifest-path rust-runtime/Cargo.toml --lib --features native-host --locked android_media_startup_cache_and_failure_matrix -- --ignored --nocapture
cargo test --manifest-path rust-runtime/Cargo.toml --lib --features native-host --locked sync_keeps_complete_reported_cache_out_of_the_worker_queue
cargo test --manifest-path rust-runtime/Cargo.toml --lib --features native-host --locked experimental_libav::cast_mux:: -- --include-ignored --test-threads=1
cargo test --manifest-path rust-runtime/Cargo.toml --lib --features native-host --locked cast_session:: -- --include-ignored --test-threads=1
```

Keep the native cast tests serial: the test companion's I/O fault injection
uses process-global counters. Concurrent fault and cancellation tests can affect
each other; this is a test-only facility, absent from the production companion.

The comparison consumes checked-in AAC, FLAC-in-MP4 and H.264 fixtures, including
the 30-second cast samples. Ordinary duration differences are bounded to 50 ms
(one AAC access unit/video frame plus timescale rounding); loudness is bounded
to 0.1 LU (priming/edit boundaries and histogram quantization). Codec, dimensions,
rate, channels, packet counts and payload sizes must match. A historical short
H.264 fixture has a corrupt timescale; the test pins its exact
out-of-tolerance values separately instead of relaxing ordinary tolerances.
The comparison also pins source/Rust/libav presentation starts for B-frame,
fragmented and AAC priming samples. AAC start metadata is zero on both paths;
the extra 1024 samples in the Rust output are asserted separately.
These checks demonstrate demux/PCM and cache-serving compatibility, not actual
Android WebView or television playback. Test processes use the same production
configuration entry point, with independent processes for available, missing,
invalid-library, incompatible-ABI and JNI-failure cases.

Compile-check both Runtime feature sets and the Android shell:

```text
cargo clippy --manifest-path rust-runtime/Cargo.toml --all-targets --target aarch64-linux-android --locked -- -D warnings
cargo clippy --manifest-path rust-runtime/Cargo.toml --all-targets --features native-host --target aarch64-linux-android --locked -- -D warnings
cargo clippy --manifest-path src-tauri/Cargo.toml --target aarch64-linux-android --locked -- -D warnings
```

Use the selected NDK compiler and process-local `LIBCLANG_PATH` for bindgen.
Actual bionic loading/namespace resolution, JNI directory lookup on a device,
and S0/casting still require the packaged Android acceptance run.
