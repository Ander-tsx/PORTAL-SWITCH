// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! User configuration: a versioned JSON document, loaded with defaults and saved atomically.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use portal_core::gate::{Corners, SwitchConfig, group};
use portal_core::layout::{Layout, LayoutError, MachineId, Rect, Screen};
use portal_net::identity::{hex, unhex};
use serde::{Deserialize, Serialize};

/// Default TCP port PortalSwitch listens on.
pub const DEFAULT_PORT: u16 = 24850;

/// Configuration problems.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("configuration file is not valid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported configuration version {0}")]
    Version(u32),
    #[error("invalid machine id {0:?}")]
    MachineId(String),
    #[error("invalid layout: {0:?}")]
    Layout(LayoutError),
}

/// One machine's rectangle on the canvas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenConfig {
    /// Hex machine ID.
    pub machine_id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Modifier groups that may be required to switch screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModifierKey {
    Shift,
    Control,
    Alt,
    Meta,
}

/// Switching options as the user edits them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // one flag per physical corner
pub struct SwitchSettings {
    pub delay_ms: u32,
    pub double_tap_ms: u32,
    pub tap_zone_px: u32,
    pub corner_px: u32,
    pub dead_top_left: bool,
    pub dead_top_right: bool,
    pub dead_bottom_left: bool,
    pub dead_bottom_right: bool,
    pub required_modifiers: Vec<ModifierKey>,
}

impl Default for SwitchSettings {
    fn default() -> Self {
        Self {
            delay_ms: 0,
            double_tap_ms: 0,
            tap_zone_px: 40,
            corner_px: 0,
            dead_top_left: false,
            dead_top_right: false,
            dead_bottom_left: false,
            dead_bottom_right: false,
            required_modifiers: Vec::new(),
        }
    }
}

impl SwitchSettings {
    /// The core's view of these settings.
    #[must_use]
    pub fn to_core(&self) -> SwitchConfig {
        let required = self.required_modifiers.iter().fold(0, |acc, m| {
            acc | match m {
                ModifierKey::Shift => group::SHIFT,
                ModifierKey::Control => group::CONTROL,
                ModifierKey::Alt => group::ALT,
                ModifierKey::Meta => group::META,
            }
        });
        SwitchConfig {
            delay_us: u64::from(self.delay_ms) * 1000,
            double_tap_us: u64::from(self.double_tap_ms) * 1000,
            tap_zone_px: self.tap_zone_px,
            corner_px: self.corner_px,
            dead_corners: Corners {
                top_left: self.dead_top_left,
                top_right: self.dead_top_right,
                bottom_left: self.dead_bottom_left,
                bottom_right: self.dead_bottom_right,
            },
            required_modifiers: required,
        }
    }
}

/// A machine to dial on start-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAddress {
    pub address: String,
}

/// The whole configuration document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub machine_name: String,
    /// This machine owns the physical keyboard and mouse.
    pub is_host: bool,
    pub listen_port: u16,
    pub layout: Vec<ScreenConfig>,
    pub switch: SwitchSettings,
    pub peers: Vec<PeerAddress>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            machine_name: "This computer".to_owned(),
            is_host: true,
            listen_port: DEFAULT_PORT,
            layout: Vec::new(),
            switch: SwitchSettings::default(),
            peers: Vec::new(),
        }
    }
}

impl Config {
    /// Load `path`, or return defaults if it does not exist yet.
    ///
    /// # Errors
    /// Unreadable, malformed or newer-than-supported files.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let config: Self = serde_json::from_str(&text)?;
                if config.version != 1 {
                    return Err(ConfigError::Version(config.version));
                }
                Ok(config)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write atomically (temp file + rename).
    ///
    /// # Errors
    /// I/O failures.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Build the screen layout. `connected` decides which machines start out connected.
    ///
    /// # Errors
    /// A malformed machine ID or an invalid arrangement.
    pub fn layout(&self, connected: impl Fn(MachineId) -> bool) -> Result<Layout, ConfigError> {
        let mut screens = Vec::with_capacity(self.layout.len());
        for s in &self.layout {
            let id = parse_machine_id(&s.machine_id)?;
            screens.push(Screen {
                id,
                rect: Rect::new(s.x, s.y, s.width, s.height),
                connected: connected(id),
            });
        }
        Layout::new(screens).map_err(ConfigError::Layout)
    }
}

/// Parse a hex machine ID.
///
/// # Errors
/// Not 32 hex digits.
pub fn parse_machine_id(text: &str) -> Result<MachineId, ConfigError> {
    let bytes = unhex(text).map_err(|_| ConfigError::MachineId(text.to_owned()))?;
    let id: [u8; 16] = bytes
        .try_into()
        .map_err(|_| ConfigError::MachineId(text.to_owned()))?;
    Ok(MachineId(id))
}

/// Format a machine ID as the hex used in configuration and IPC.
#[must_use]
pub fn format_machine_id(id: MachineId) -> String {
    hex(&id.0)
}

/// Where PortalSwitch keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
}

impl Paths {
    /// The platform's per-user data directory (override with `PORTALSWITCH_CONFIG_DIR`).
    #[must_use]
    pub fn default_for_user() -> Self {
        let dir = std::env::var_os("PORTALSWITCH_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(platform_dir)
            .unwrap_or_else(|| PathBuf::from(".portalswitch"));
        Self { dir }
    }

    #[must_use]
    pub fn config(&self) -> PathBuf {
        self.dir.join("config.json")
    }

    #[must_use]
    pub fn identity(&self) -> PathBuf {
        self.dir.join("identity.json")
    }

    #[must_use]
    pub fn trust(&self) -> PathBuf {
        self.dir.join("trust.json")
    }
}

fn platform_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/PortalSwitch"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("PortalSwitch"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(|x| PathBuf::from(x).join("portalswitch"))
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/portalswitch"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("portalswitch-cfg-{}-{name}", std::process::id()))
    }

    #[test]
    fn missing_file_gives_defaults_and_round_trips() {
        let dir = temp("rt");
        let path = dir.join("config.json");
        let mut config = Config::load(&path).unwrap();
        assert_eq!(config, Config::default());
        config.machine_name = "Studio".into();
        config.switch.delay_ms = 250;
        config.switch.required_modifiers = vec![ModifierKey::Shift, ModifierKey::Alt];
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_fields_and_missing_fields_are_tolerated() {
        let dir = temp("compat");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        fs::write(
            &path,
            r#"{"version":1,"machine_name":"X","future_option":true}"#,
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.machine_name, "X");
        assert_eq!(config.listen_port, DEFAULT_PORT);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newer_versions_and_garbage_are_errors_not_silently_replaced() {
        let dir = temp("bad");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        fs::write(&path, r#"{"version":9}"#).unwrap();
        assert!(matches!(Config::load(&path), Err(ConfigError::Version(9))));
        fs::write(&path, "not json").unwrap();
        assert!(matches!(Config::load(&path), Err(ConfigError::Json(_))));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn switch_settings_map_to_the_core() {
        let settings = SwitchSettings {
            delay_ms: 300,
            double_tap_ms: 400,
            dead_top_right: true,
            corner_px: 25,
            required_modifiers: vec![ModifierKey::Control, ModifierKey::Shift],
            ..SwitchSettings::default()
        };
        let core = settings.to_core();
        assert_eq!(
            (core.delay_us, core.double_tap_us, core.corner_px),
            (300_000, 400_000, 25)
        );
        assert!(core.dead_corners.top_right && !core.dead_corners.top_left);
        assert_eq!(core.required_modifiers, group::SHIFT | group::CONTROL);
    }

    #[test]
    fn layout_is_built_and_validated() {
        let a = format_machine_id(MachineId([1; 16]));
        let b = format_machine_id(MachineId([2; 16]));
        let mut config = Config {
            layout: vec![
                ScreenConfig {
                    machine_id: a.clone(),
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                ScreenConfig {
                    machine_id: b.clone(),
                    x: 100,
                    y: 0,
                    width: 100,
                    height: 100,
                },
            ],
            ..Config::default()
        };
        let layout = config.layout(|id| id == MachineId([1; 16])).unwrap();
        assert!(layout.screen(MachineId([1; 16])).unwrap().connected);
        assert!(!layout.screen(MachineId([2; 16])).unwrap().connected);
        config.layout[1].x = 50; // overlaps
        assert!(matches!(
            config.layout(|_| true),
            Err(ConfigError::Layout(_))
        ));
        config.layout[1].machine_id = "zz".into();
        assert!(matches!(
            config.layout(|_| true),
            Err(ConfigError::MachineId(_))
        ));
    }
}
