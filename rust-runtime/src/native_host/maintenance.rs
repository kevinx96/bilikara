//! Remove only native-owned, no-longer-referenced immutable cache generations.
//! No recursive deletion of the cache root and no following directory symlinks.
use super::*;

pub(super) fn collect(root: &Path, include_staging: bool) -> std::io::Result<usize> {
    if include_staging {
        with_app(|app| {
            app.open_artifact_lifetime(root).map_err(storage_error)?;
            Ok(())
        })
        .map_err(|_| std::io::Error::other("native cache recovery failed"))?;
        let mut collected = crate::experimental_libav::cast_mux::CastSession::recover(root)?;
        for area in [".retired", "artifacts", ".staging"] {
            crate::app_state::artifact_lifetime::recover_native_artifacts(root, area)?;
            collected += crate::app_state::artifact_lifetime::collect_artifacts(None)?;
        }
        return Ok(collected);
    }
    crate::app_state::artifact_lifetime::collect_artifacts(None)
}

fn storage_error(_: std::io::Error) -> ApiError {
    ApiError::new(503, "cache_storage", "无法回收旧媒体缓存")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_collection_recovers_cast_orphans() {
        use crate::{AppStateRequest, execute_app_state};
        let _serial = crate::app_state::native_session::GLOBAL_APP_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        execute_app_state(AppStateRequest::Shutdown { schema_version: 1 });
        let seed = serde_json::from_value(json!({"session_users":["Alice"],"session_started_at":1.0,"session_played_file":"test.json","updated_at":1.0})).unwrap();
        assert!(
            execute_app_state(AppStateRequest::Initialize {
                schema_version: 1,
                state: Box::new(seed)
            })
            .error()
            .is_none()
        );
        let root = std::env::temp_dir().join(format!("bilikara-cast-startup-{}", token().unwrap()));
        let orphan = root.join(".cast/0123456789abcdef0123456789abcdef");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("crashed.part"), b"partial").unwrap();
        assert_eq!(collect(&root, true).unwrap(), 1);
        assert!(!orphan.exists());
        execute_app_state(AppStateRequest::Shutdown { schema_version: 1 });
        std::fs::remove_dir_all(root).unwrap();
    }
}
