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
