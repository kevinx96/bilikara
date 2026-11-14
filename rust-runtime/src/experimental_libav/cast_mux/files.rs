//! Private, quota-bounded cast files under the caller's authoritative cache root.
use super::*;
use std::{
    fs::{self, OpenOptions},
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
struct Directory {
    path: PathBuf,
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
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
        match fs::create_dir(&cast) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(e)),
        }
        let meta = fs::symlink_metadata(&cast).map_err(io_error)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(io_error("cast root is not a real directory"));
        }
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(io_error)?;
        let name = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let path = cast.join(name);
        #[cfg(unix)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(io_error)?;
        Ok(Self {
            directory: Arc::new(Directory { path }),
            used: Arc::new(AtomicU64::new(0)),
            generator: Mutex::new(()),
            limit: max_bytes,
        })
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
                let result = probe.cast_ts(
                    inputs,
                    CastPacing::FullSpeed,
                    remaining,
                    stop,
                    &mut |bytes, _, _| match file.write_all(bytes) {
                        Ok(()) => true,
                        Err(e) => {
                            write_error = Some(e);
                            false
                        }
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
                    Some(&staging),
                    CastPacing::FullSpeed,
                    remaining,
                    stop,
                    &mut |_, _, _| false,
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
