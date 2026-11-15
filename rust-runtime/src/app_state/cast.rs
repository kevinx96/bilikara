//! Persisted device choices. Addresses, credentials and active sessions never belong here.
use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Auto,
    File,
    Live,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearnedMode {
    File,
    Live,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    Original,
    Uniform,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    #[default]
    Generic,
    Lebo,
    Dolphinstar,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Device {
    pub label: String,
    pub vendor_class: Vendor,
    pub mode: Mode,
    pub learned_mode: Option<LearnedMode>,
    pub learned_variant: Option<Variant>,
    pub learned_at: Option<String>,
    pub self_test_report_version: Option<u32>,
}
impl Device {
    pub fn effective_mode(&self) -> Mode {
        match self.mode {
            Mode::Auto => match self.learned_mode {
                Some(LearnedMode::Live) => Mode::Live,
                _ => Mode::File,
            },
            other => other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u32,
    pub devices: BTreeMap<String, Device>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            devices: BTreeMap::new(),
        }
    }
}
impl Preferences {
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
    pub(super) fn validate(&self) -> Result<(), ExecuteError> {
        if self.schema_version != 1
            || self.devices.len() > 256
            || self.devices.iter().any(|(key, d)| {
                d.label.chars().count() > 100
                    || d.label.chars().any(char::is_control)
                    || !valid_key(key)
                    || d.self_test_report_version.is_some_and(|v| v != 1)
                    || d.learned_at
                        .as_ref()
                        .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_err())
            })
        {
            return Err(rejected(
                "invalid_cast_preferences",
                "Invalid cast device preferences",
            ));
        }
        Ok(())
    }
}
pub fn valid_key(key: &str) -> bool {
    key.len() == 32
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(feature = "native-host")]
impl AppState {
    pub(crate) fn cast_preferences(&self) -> Result<&Preferences, crate::native_host::ApiError> {
        self.data.as_ref().map(|d| &d.cast).ok_or_else(|| {
            crate::native_host::ApiError::new(503, "state_unavailable", "Host is not initialized")
        })
    }

    pub(crate) fn cast_set_device(
        &mut self,
        key: &str,
        expected_revision: u64,
        device: Device,
    ) -> Result<(), crate::native_host::ApiError> {
        use crate::native_host::ApiError;
        let mut next = self
            .data
            .clone()
            .ok_or_else(|| ApiError::new(503, "state_unavailable", "Host is not initialized"))?;
        if next.revision != expected_revision {
            return Err(ApiError::new(
                409,
                "cast_revision_conflict",
                "Device preferences changed; refresh and retry",
            ));
        }
        next.cast.devices.insert(key.to_owned(), device);
        next.cast
            .validate()
            .map_err(|_| ApiError::invalid("Invalid cast device preferences"))?;
        next.revision += 1;
        self.persist_native(&next).map_err(|_| {
            ApiError::new(503, "persistence_error", "Cannot persist cast preferences")
        })?;
        self.data = Some(next);
        self.native_session.revision += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> AppStateSeed {
        serde_json::from_value(
            json!({"session_started_at":1,"session_played_file":"old.json","updated_at":1}),
        )
        .unwrap()
    }

    #[test]
    fn old_seed_without_cast_loads_and_round_trips_without_new_fields() {
        let seed = seed();
        assert_eq!(seed.cast, Preferences::default());
        let value = serde_json::to_value(&seed).unwrap();
        assert!(value.get("cast").is_none());
        let mut app = AppState::default();
        assert!(
            app.execute(AppStateRequest::Initialize {
                schema_version: 1,
                state: Box::new(seed)
            })
            .error()
            .is_none()
        );
        assert_eq!(app.data.as_ref().unwrap().cast, Preferences::default());
        assert!(
            serde_json::to_value(app.data.as_ref().unwrap().snapshot().unwrap())
                .unwrap()
                .get("cast")
                .is_none()
        );
    }

    #[cfg(feature = "native-host")]
    #[test]
    fn legacy_checkpoint_load_and_device_preferences_persist_with_revision_conflicts() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".tmp")
            .join(format!(
                "cast-checkpoint-{}",
                crate::native_host::token().unwrap()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let key = "0123456789abcdef0123456789abcdef";
        let mut app = AppState::default();
        assert!(app.initialize_native(&root, seed()).error().is_none());
        drop(app);
        let original = std::fs::read(root.join("host-state.json")).unwrap();
        assert!(!String::from_utf8_lossy(&original).contains("\"cast\""));
        let mut app = AppState::default();
        assert!(app.initialize_native(&root, seed()).error().is_none());
        assert_eq!(app.cast_preferences().unwrap(), &Preferences::default());
        assert!(!app.native_session.cast.visible());
        let revision = app.data.as_ref().unwrap().revision;
        let device = Device {
            label: String::new(),
            vendor_class: Vendor::Generic,
            mode: Mode::Auto,
            learned_mode: Some(LearnedMode::Live),
            learned_variant: Some(Variant::Original),
            learned_at: Some("2026-11-15T00:00:00Z".into()),
            self_test_report_version: Some(1),
        };
        app.cast_set_device(key, revision, device.clone()).unwrap();
        assert_eq!(
            app.cast_preferences().unwrap().devices[key].effective_mode(),
            Mode::Live
        );
        assert_eq!(
            app.cast_set_device(key, revision, Device::default())
                .unwrap_err()
                .code,
            "cast_revision_conflict"
        );
        drop(app);
        let mut app = AppState::default();
        assert!(app.initialize_native(&root, seed()).error().is_none());
        assert_eq!(app.cast_preferences().unwrap().devices[key], device);
        let bytes = std::fs::read(root.join("host-state.json")).unwrap();
        let rev = app.data.as_ref().unwrap().revision;
        assert!(
            app.cast_set_device("192.168.1.2", rev, Device::default())
                .is_err()
        );
        assert_eq!(bytes, std::fs::read(root.join("host-state.json")).unwrap());
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "native-host")]
    #[test]
    fn casting_rejects_pitch_delay_and_stale_local_observations_without_mutation() {
        let mut app = AppState::default();
        app.execute(AppStateRequest::Initialize {
            schema_version: 1,
            state: Box::new(seed()),
        });
        app.native_session.cast.owns_playback = true;
        let before = app.data.as_ref().unwrap().snapshot().unwrap();
        for request in [
            AppStateRequest::SetKeyShift {
                schema_version: 1,
                key_shift: 2,
                now: 2.0,
            },
            AppStateRequest::ApplyAvDelay {
                schema_version: 1,
                action: AvDelayCommand::Adjust { delta_ms: 20 },
                now: 2.0,
            },
        ] {
            assert_eq!(
                app.execute(request).error().unwrap().kind,
                "cast_setting_unavailable"
            );
            assert_eq!(app.data.as_ref().unwrap().snapshot().unwrap(), before);
        }
        assert_eq!(
            app.execute(AppStateRequest::ApplyPlayerStatusObservation {
                schema_version: 1,
                expected_playback_generation: before.playback_generation,
                item_id: "stale-local-player".into(),
                is_paused: false,
                current_time: 99.0,
                duration: 120.0,
                now: 2.0,
            })
            .error()
            .unwrap()
            .kind,
            "cast_owns_playback"
        );
        assert_eq!(app.data.as_ref().unwrap().snapshot().unwrap(), before);
    }
}
