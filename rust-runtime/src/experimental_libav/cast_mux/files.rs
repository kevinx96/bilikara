//! Private, quota-bounded cast files under the caller's authoritative cache root.
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicU64, Ordering},
    },
};

fn io_error(error: impl std::fmt::Display) -> ProbeError {
    ProbeError::BackendFailure(format!("cast staging: {error}"))
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 // all reparse points, including junctions
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn private_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}
fn lock_file(path: &Path) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(m) if is_link(&m) || !m.is_file() => {
            return Err(std::io::Error::other("invalid cast lock"));
        }
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}
// Never unlink this stable gate: all processes must lock the same inode.
fn root_gate(cast: &Path) -> std::io::Result<File> {
    let meta = fs::symlink_metadata(cast)?;
    if is_link(&meta) || !meta.is_dir() {
        return Err(std::io::Error::other("cast root is not a real directory"));
    }
    let file = lock_file(&cast.join(".lock"))?;
    fs2::FileExt::lock_exclusive(&file)?;
    Ok(file)
}
fn recover_locked(cast: &Path) -> std::io::Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(cast)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() != 32 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if is_link(&meta) || !meta.is_dir() {
            continue;
        }
        let lease = lock_file(&path.join(".lease"))?;
        match fs2::FileExt::try_lock_exclusive(&lease) {
            Ok(()) => (),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                continue;
            }
            Err(e) => return Err(e),
        }
        // Root gate still excludes creation and other collectors. Close before
        // Windows deletion; no live owner can acquire an abandoned directory.
        drop(lease);
        fs::remove_dir_all(&path)?; // std does not follow child symlinks/junctions
        count += 1;
    }
    Ok(count)
}
struct Directory {
    path: PathBuf,
    lease: Option<File>,
}
impl Drop for Directory {
    fn drop(&mut self) {
        if let Some(root) = self.path.parent()
            && let Ok(_gate) = root_gate(root)
        {
            drop(self.lease.take());
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
/// One per cast session. Files and in-flight readers share its private directory.
/// Retained files count against the quota; drop obsolete generations to free it.
pub struct CastSession {
    directory: Arc<Directory>,
    used: Arc<AtomicU64>,
    limit: u64,
    generator: Mutex<()>,
}
pub struct CastFile {
    path: PathBuf,
    pub summary: CastSummary,
    _directory: Arc<Directory>,
    used: Arc<AtomicU64>,
    charged: u64,
}
impl CastFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for CastFile {
    fn drop(&mut self) {
        // On Windows a reader must retain this lease until its file handle closes.
        if fs::remove_file(&self.path).is_ok() {
            self.used.fetch_sub(self.charged, Ordering::AcqRel);
        }
    }
}
impl CastSession {
    /// `cache_root` is runtime/data/cache (or its explicit configured override).
    /// The caller keeps this object until session end, and file leases until the
    /// last reader closes. No hard links, cache source mutation or external path.
    pub fn new(cache_root: &Path, max_bytes: u64) -> Result<Self, ProbeError> {
        if max_bytes == 0 || max_bytes > i64::MAX as u64 {
            return Err(io_error("invalid quota"));
        }
        let root = fs::canonicalize(cache_root).map_err(io_error)?;
        let cast = root.join(".cast");
        match private_directory(&cast) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(io_error(e)),
        }
        let _gate = root_gate(&cast).map_err(io_error)?;
        recover_locked(&cast).map_err(io_error)?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(io_error)?;
        let name = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let path = cast.join(name);
        private_directory(&path).map_err(io_error)?;
        let lease = lock_file(&path.join(".lease")).map_err(io_error)?;
        fs2::FileExt::try_lock_exclusive(&lease).map_err(io_error)?;
        Ok(Self {
            directory: Arc::new(Directory {
                path,
                lease: Some(lease),
            }),
            used: Arc::new(AtomicU64::new(0)),
            generator: Mutex::new(()),
            limit: max_bytes,
        })
    }
    /// Reclaim crash-abandoned generations only. Live sessions and CastFile
    /// readers keep an OS lease and are never deleted by startup/cache cleanup.
    pub fn recover(cache_root: &Path) -> std::io::Result<usize> {
        let cast = fs::canonicalize(cache_root)?.join(".cast");
        match fs::symlink_metadata(&cast) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
            Ok(_) => (),
        }
        let _gate = root_gate(&cast)?;
        recover_locked(&cast)
    }
    pub fn mux(
        &self,
        probe: &LibavMetadataProbe,
        inputs: &CastInputs<'_>,
        format: CastFormat,
        stop: &AtomicBool,
    ) -> Result<CastFile, ProbeError> {
        // Serialize generators within a session; readers never acquire this lock.
        let _generation = loop {
            if stop.load(Ordering::Acquire) {
                return Err(ProbeError::Cancelled);
            }
            match self.generator.try_lock() {
                Ok(lock) => break lock,
                Err(TryLockError::Poisoned(error)) => return Err(io_error(error)),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let remaining = self.limit.saturating_sub(self.used.load(Ordering::Acquire));
        if remaining == 0 {
            return Err(io_error("session quota exhausted"));
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(io_error)?;
        let name = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let staging = self.directory.path.join(format!("{name}.part"));
        let published = self.directory.path.join(format!(
            "{name}.{}",
            if format == CastFormat::Ts {
                "ts"
            } else {
                "mp4"
            }
        ));
        // Unique exclusive directory and filenames make rename no-replace by
        // construction, including Android (which forbids hard-link publication).
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staging)
                .map_err(io_error)?;
            let mut write_error = None;
            let summary = if format == CastFormat::Ts {
                let result = probe.cast_run(
                    inputs,
                    CastOutput::FileTs,
                    CastPacing::FullSpeed,
                    remaining,
                    stop,
                    &mut |event| match event {
                        CastEvent::Start(_) => true,
                        CastEvent::Data { bytes, .. } => match file.write_all(bytes) {
                            Ok(()) => true,
                            Err(e) => {
                                write_error = Some(e);
                                false
                            }
                        },
                    },
                );
                if let Some(e) = write_error {
                    return Err(io_error(e));
                }
                result?
            } else {
                drop(file);
                let summary = probe.cast_run(
                    inputs,
                    CastOutput::Mp4(&staging),
                    CastPacing::FullSpeed,
                    remaining,
                    stop,
                    &mut |_| true,
                )?;
                file = OpenOptions::new()
                    .write(true)
                    .open(&staging)
                    .map_err(io_error)?;
                summary
            };
            file.sync_all().map_err(io_error)?;
            drop(file);
            let size = fs::metadata(&staging).map_err(io_error)?.len();
            if size != summary.bytes || size > remaining {
                return Err(io_error("invalid finalized size"));
            }
            if stop.load(std::sync::atomic::Ordering::Acquire) {
                return Err(ProbeError::Cancelled);
            }
            #[cfg(target_os = "android")]
            crate::file_publication::publish_no_replace(&staging, &published).map_err(io_error)?;
            #[cfg(not(target_os = "android"))]
            fs::rename(&staging, &published).map_err(io_error)?;
            self.used.fetch_add(size, Ordering::AcqRel);
            Ok(CastFile {
                path: published,
                summary,
                _directory: self.directory.clone(),
                used: self.used.clone(),
                charged: size,
            })
        })();
        if result.is_err() && fs::remove_file(&staging).is_err() {
            // A failed cleanup still consumes disk until the session directory
            // is removed; retries must not bypass the quota with orphan parts.
            if let Ok(metadata) = fs::metadata(&staging) {
                self.used.fetch_add(metadata.len(), Ordering::AcqRel);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path =
            std::env::temp_dir().join(format!("cast-recovery-{:x}", u128::from_ne_bytes(nonce)));
        fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn crash_owner_child() {
        let Some(root) = std::env::var_os("BILIKARA_CAST_CRASH_TEST_ROOT") else {
            return;
        };
        let session = CastSession::new(Path::new(&root), 1024).unwrap();
        fs::write(session.directory.path.join("crashed.part"), b"unfinished").unwrap();
        std::process::exit(0); // deliberate crash-like exit: no Directory/lease Drop
    }
    #[test]
    fn recovery_reclaims_crashed_owner_but_preserves_live_reader_lease() {
        let root = root();
        let session = CastSession::new(&root, 1024).unwrap();
        let reader = session.directory.clone();
        let path = reader.path.clone();
        fs::write(path.join("live.ts"), b"live").unwrap();
        drop(session);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "experimental_libav::cast_mux::files::tests::crash_owner_child",
                "--nocapture",
            ])
            .env("BILIKARA_CAST_CRASH_TEST_ROOT", &root)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(CastSession::recover(&root).unwrap(), 1);
        assert!(path.join("live.ts").exists());
        assert_eq!(CastSession::recover(&root).unwrap(), 0);
        drop(reader);
        assert!(!path.exists());
        // A crash between mkdir and lease creation is also recoverable.
        let orphan = root.join(".cast").join("0123456789abcdef0123456789abcdef");
        fs::create_dir(&orphan).unwrap();
        fs::write(orphan.join("partial"), b"x").unwrap();
        let next = CastSession::new(&root, 1024).unwrap();
        assert!(!orphan.exists());
        drop(next);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn recovery_rejects_non_directory_root_and_preserves_unrecognized_paths() {
        let root = root();
        fs::write(root.join(".cast"), b"not ours").unwrap();
        assert!(CastSession::recover(&root).is_err());
        assert!(CastSession::new(&root, 1024).is_err());
        fs::remove_file(root.join(".cast")).unwrap();
        let session = CastSession::new(&root, 1024).unwrap();
        let other = root.join(".cast/not-a-session");
        fs::create_dir(&other).unwrap();
        assert_eq!(CastSession::recover(&root).unwrap(), 0);
        assert!(other.exists());
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }
}
