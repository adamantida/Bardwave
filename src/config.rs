//! Configuration management for SoundShared

use crate::error::{ConfigError, Result};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Network server configuration
    #[serde(default)]
    pub server: ServerConfig,

    /// Audio engine configuration
    #[serde(default)]
    pub audio: AudioConfig,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Persistent sound library and group configuration
    #[serde(default)]
    pub library: LibraryConfig,

    /// Desktop GUI options
    #[serde(default)]
    pub gui: GuiConfig,
}

/// Network server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Port to bind the HTTP/WebSocket server to
    #[serde(default = "default_port")]
    pub port: u16,

    /// Host to bind to (0.0.0.0 for all interfaces)
    #[serde(default = "default_host")]
    pub host: String,

    /// Try to open this port on the local router via UPnP IGD so it is
    /// reachable from the global IP. Enabled by default.
    #[serde(default = "default_upnp")]
    pub upnp: bool,

    /// Key players need in the link (`/?k=…`) to hear the stream.
    ///
    /// Generated on first start when missing. Set it to `""` to let anyone
    /// who can reach the port listen (not recommended with UPnP on).
    #[serde(default)]
    pub access_token: Option<String>,
}

/// Audio engine configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Audio buffer size in samples (per channel)
    #[serde(default = "default_buffer_size")]
    pub buffer_size: usize,

    /// Sample rate for audio processing
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,

    /// Number of output channels (2 for stereo)
    #[serde(default = "default_channels")]
    pub channels: u16,

    /// Master volume (0.0 to 1.0)
    #[serde(default = "default_master_volume")]
    pub master_volume: f32,
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Enable file logging in addition to stdout
    #[serde(default)]
    pub file_logging: bool,
}

/// Desktop GUI options.
///
/// These are console presentation choices, not features: the design system
/// requires the app to respect reduced motion, and egui 0.30 exposes no such
/// signal, so the operator states it here. Missing section in an older
/// `config.toml` means "animated" (the default), so nothing changes for configs
/// written before this existed.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GuiConfig {
    /// Disable the live-dot pulse and the backdrop scanline.
    ///
    /// State changes stay instant; only the two decorative motions stop.
    #[serde(default)]
    pub reduce_motion: bool,
}

/// Persistent library + groups configuration
///
/// This mirrors the in-memory sound library model (`crate::gui::groups::Library`)
/// so a user's groups, sound paths, per-sound volumes and group membership
/// survive restarts. Everything is optional in the file so configs written by
/// older versions (which had no `[library]` section) still load.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct LibraryConfig {
    /// Sound groups, in insertion order
    #[serde(default)]
    pub groups: Vec<GroupConfig>,

    /// Registered sounds, in insertion order
    #[serde(default)]
    pub sounds: Vec<SoundConfigEntry>,
}

/// A single sound group (persisted form)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupConfig {
    /// Group identifier (UUID string)
    pub id: String,

    /// Human-readable group name
    pub name: String,
}

/// A single registered sound (persisted form)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SoundConfigEntry {
    /// Absolute or user-supplied path to the audio file
    pub path: String,

    /// Display name; falls back to the file name when empty
    #[serde(default)]
    pub name: String,

    /// Per-sound volume, 0.0..=1.0
    #[serde(default = "default_sound_volume")]
    pub volume: f32,

    /// Whether the sound restarts from the beginning when it ends
    #[serde(default)]
    pub looped: bool,

    /// Group this sound belongs to, if any (UUID string)
    #[serde(default)]
    pub group: Option<String>,
}

fn default_port() -> u16 {
    8080
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_upnp() -> bool {
    true
}

fn default_buffer_size() -> usize {
    // 10 ms at 48 kHz: what the engine always asked for before this setting
    // was applied.
    480
}

fn default_sample_rate() -> u32 {
    48000
}

fn default_channels() -> u16 {
    2
}

fn default_master_volume() -> f32 {
    0.8
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Default per-sound volume used when a `SoundConfigEntry` omits `volume`
pub fn default_sound_volume() -> f32 {
    1.0
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            upnp: default_upnp(),
            access_token: None,
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            buffer_size: default_buffer_size(),
            sample_rate: default_sample_rate(),
            channels: default_channels(),
            master_volume: default_master_volume(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file_logging: false,
        }
    }
}

/// Parse a port typed into the CONFIGURATION field.
///
/// Only `1..=65535` is accepted: empty text, `0`, out-of-range values and
/// anything that is not plain ASCII digits are rejected, so the caller can keep
/// the last valid port in force instead of applying garbage.
pub fn parse_port(text: &str) -> Option<u16> {
    let trimmed = text.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match trimmed.parse::<u16>() {
        Ok(0) | Err(_) => None,
        Ok(port) => Some(port),
    }
}

/// Parse a bind address typed into the CONFIGURATION field.
///
/// Only IP literals are accepted (`0.0.0.0`, `127.0.0.1`, `192.168.1.245`,
/// `::`, `::1`). Host names are rejected because the value is handed straight to
/// `TcpListener::bind`, which takes a numeric address.
pub fn parse_bind_host(text: &str) -> Option<IpAddr> {
    text.trim().parse::<IpAddr>().ok()
}

impl Config {
    /// Load configuration from config.toml next to the executable
    /// Falls back to defaults if file doesn't exist or is invalid
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path()?)
    }

    /// Load configuration from an explicit path.
    ///
    /// Split out of [`Config::load`] so tests (and any caller that knows its own
    /// config location) can round-trip a temporary file instead of the real
    /// `config.toml` that sits next to the executable.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            tracing::info!("Config file not found at {:?}, using defaults", path);
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::NotFound(format!("{:?}: {}", path, e)))?;

        let config: Config = toml::from_str(&content)
            .map_err(|e| ConfigError::Parse(format!("{:?}: {}", path, e)))?;

        tracing::info!("Loaded configuration from {:?}", path);
        Ok(config)
    }

    /// Save configuration to config.toml next to the executable
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::config_path()?)
    }

    /// Save configuration to an explicit path (see [`Config::load_from`]).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Write(format!("Serialization failed: {}", e)))?;

        std::fs::write(path, content)
            .map_err(|e| ConfigError::Write(format!("{:?}: {}", path, e)))?;

        tracing::info!("Saved configuration to {:?}", path);
        Ok(())
    }

    /// Get the path to config.toml (next to executable)
    fn config_path() -> Result<PathBuf> {
        let exe_path = std::env::current_exe()
            .map_err(|e| ConfigError::NotFound(format!("Failed to get executable path: {}", e)))?;

        let config_dir = exe_path.parent().ok_or_else(|| {
            ConfigError::NotFound("Executable has no parent directory".to_string())
        })?;

        Ok(config_dir.join("config.toml"))
    }

    /// Get the socket address for the server
    pub fn server_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch path for round-trip tests. It lives under `target/`, which the
    /// running test binary can always write, and it never matches the real
    /// `config.toml` next to the executable.
    fn temp_config_path(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-config");
        std::fs::create_dir_all(&dir).expect("create test config dir");
        dir.join(format!("{}-{}.toml", name, std::process::id()))
    }

    /// The commented config.toml shipped in the release archives must load,
    /// and must not carry an access key (each install generates its own).
    #[test]
    fn shipped_example_config_loads() {
        let config: Config = toml::from_str(include_str!("../packaging/config.toml"))
            .expect("packaging/config.toml");
        assert_eq!(config.server.port, 8080);
        assert!(config.server.access_token.is_none());
        assert_eq!(config.audio.buffer_size, 480);
        assert!(config.library.sounds.is_empty());
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.audio.buffer_size, 480);
        assert_eq!(config.audio.sample_rate, 48000);
        assert_eq!(config.audio.channels, 2);
        assert_eq!(config.audio.master_volume, 0.8);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn test_server_addr() {
        let config = Config::default();
        assert_eq!(config.server_addr(), "0.0.0.0:8080");
    }

    #[test]
    fn test_default_library_is_empty() {
        let config = Config::default();
        assert!(config.library.groups.is_empty());
        assert!(config.library.sounds.is_empty());
        assert_eq!(default_sound_volume(), 1.0);
    }

    /// Backwards compatibility: a config.toml written before the `[library]`
    /// section existed must still parse, defaulting the library to empty.
    #[test]
    fn test_legacy_config_without_library_section_parses() {
        let legacy = r#"
[server]
port = 9000
host = "127.0.0.1"

[audio]
buffer_size = 512
sample_rate = 44100
channels = 2
master_volume = 0.5

[logging]
level = "debug"
file_logging = true
"#;

        let config: Config = toml::from_str(legacy).expect("legacy config must parse");
        assert_eq!(config.server.port, 9000);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.audio.buffer_size, 512);
        assert_eq!(config.logging.level, "debug");
        assert!(config.logging.file_logging);
        assert!(config.library.groups.is_empty());
        assert!(config.library.sounds.is_empty());
    }

    /// A document that carries only empty sections plus `[library]` must parse
    /// with every individual setting at its default.
    #[test]
    fn test_config_with_only_library_parses_with_defaults() {
        let text = r#"
[server]
[audio]
[logging]

[[library.groups]]
id = "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22"
name = "Combat"
"#;

        let config: Config = toml::from_str(text).expect("library-only config must parse");
        assert_eq!(config.server.port, default_port());
        assert_eq!(config.audio.master_volume, default_master_volume());
        assert_eq!(config.logging.level, default_log_level());
        assert_eq!(config.library.groups.len(), 1);
        assert_eq!(config.library.groups[0].name, "Combat");
        assert!(config.library.sounds.is_empty());
    }

    #[test]
    fn test_library_config_roundtrip() {
        let config = Config {
            library: LibraryConfig {
                groups: vec![
                    GroupConfig {
                        id: "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22".to_string(),
                        name: "Combat".to_string(),
                    },
                    GroupConfig {
                        id: "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f".to_string(),
                        name: "Ambience".to_string(),
                    },
                ],
                sounds: vec![
                    SoundConfigEntry {
                        path: r"C:\fake\hit.wav".to_string(),
                        name: "Hit".to_string(),
                        volume: 0.75,
                        looped: true,
                        group: Some("6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22".to_string()),
                    },
                    SoundConfigEntry {
                        path: r"C:\fake\wind.ogg".to_string(),
                        name: String::new(),
                        volume: 1.0,
                        looped: false,
                        group: None,
                    },
                ],
            },
            ..Config::default()
        };

        let text = toml::to_string_pretty(&config).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");

        assert_eq!(back.library.groups, config.library.groups);
        assert_eq!(back.library.sounds, config.library.sounds);
        assert_eq!(back.server.port, config.server.port);
    }

    /// `volume` in a stored entry must default to 1.0 when omitted, and
    /// `group`/`name` must default to None/empty.
    #[test]
    fn test_sound_entry_defaults_when_fields_omitted() {
        let text = r#"
[server]
port = 8080
host = "0.0.0.0"

[audio]
[logging]

[[library.sounds]]
path = 'C:\fake\hit.wav'

[[library.groups]]
id = "6f1e1b0e-0f8e-4a8e-9a2c-2b2a0e9d1f22"
name = "Combat"
"#;

        let config: Config = toml::from_str(text).expect("parse");
        assert_eq!(config.library.sounds.len(), 1);
        let entry = &config.library.sounds[0];
        assert_eq!(entry.volume, 1.0);
        assert_eq!(entry.name, "");
        assert_eq!(entry.group, None);
        // Configs written before the repeat toggle existed must load as "off".
        assert!(!entry.looped);
    }

    // ===== GUI options =====

    /// A `config.toml` written before `[gui]` existed must load with the console
    /// animating, so upgrading never changes how the app looks.
    #[test]
    fn missing_gui_section_defaults_to_animated() {
        let text = r#"
[server]
port = 8080
host = "0.0.0.0"

[audio]
[logging]
"#;

        let config: Config = toml::from_str(text).expect("parse");

        assert!(!config.gui.reduce_motion);
    }

    #[test]
    fn reduce_motion_round_trips_through_toml() {
        let mut config = Config::default();
        config.gui.reduce_motion = true;

        let text = toml::to_string_pretty(&config).expect("serialize");
        let back: Config = toml::from_str(&text).expect("deserialize");

        assert!(back.gui.reduce_motion);
        assert!(text.contains("reduce_motion = true"));
    }

    // ===== CONFIGURATION field validation =====

    #[test]
    fn parse_port_accepts_the_valid_range() {
        assert_eq!(parse_port("1"), Some(1));
        assert_eq!(parse_port("8080"), Some(8080));
        assert_eq!(parse_port(" 65535 "), Some(65535));
    }

    #[test]
    fn parse_port_rejects_invalid_text() {
        for bad in [
            "", "   ", "0", "65536", "99999", "80a", "-1", "+80", "80.5", "0x1f",
        ] {
            assert_eq!(parse_port(bad), None, "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn parse_bind_host_accepts_ip_literals() {
        assert_eq!(parse_bind_host("0.0.0.0"), "0.0.0.0".parse::<IpAddr>().ok());
        assert_eq!(
            parse_bind_host(" 127.0.0.1 "),
            "127.0.0.1".parse::<IpAddr>().ok()
        );
        assert_eq!(
            parse_bind_host("192.168.1.245"),
            "192.168.1.245".parse::<IpAddr>().ok()
        );
        assert_eq!(parse_bind_host("::"), "::".parse::<IpAddr>().ok());
        assert_eq!(parse_bind_host("::1"), "::1".parse::<IpAddr>().ok());
        assert_eq!(parse_bind_host("fe80::1"), "fe80::1".parse::<IpAddr>().ok());
    }

    #[test]
    fn parse_bind_host_rejects_non_literals() {
        for bad in [
            "",
            "   ",
            "localhost",
            "192.168.1",
            "192.168.1.256",
            "8080",
            "soundshared.local",
            "0.0.0.0:8080",
        ] {
            assert_eq!(
                parse_bind_host(bad),
                None,
                "expected {bad:?} to be rejected"
            );
        }
    }

    /// An edited host + port must survive the exact save/load pair the GUI uses
    /// to persist the CONFIGURATION fields.
    #[test]
    fn save_to_and_load_from_roundtrip_edited_host_and_port() {
        let path = temp_config_path("server-edit");
        let mut config = Config::default();
        config.server.host = "192.168.1.245".to_string();
        config.server.port = 9000;
        config.audio.master_volume = 0.42;

        config.save_to(&path).expect("save_to");
        let back = Config::load_from(&path).expect("load_from");

        assert_eq!(back.server.host, "192.168.1.245");
        assert_eq!(back.server.port, 9000);
        assert_eq!(back.audio.master_volume, 0.42);
        assert_eq!(back.library, config.library);

        // IPv6 literals must round-trip too; the value is re-serialised as a
        // plain TOML string and handed straight to `TcpListener::bind`.
        config.server.host = "::1".to_string();
        config.server.port = 65535;
        config.save_to(&path).expect("save_to ipv6");
        let back = Config::load_from(&path).expect("load_from ipv6");
        assert_eq!(back.server.host, "::1");
        assert_eq!(back.server.port, 65535);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_missing_path_falls_back_to_defaults() {
        let path = temp_config_path("missing");
        let _ = std::fs::remove_file(&path);

        let config = Config::load_from(&path).expect("defaults");

        assert_eq!(config.server.port, default_port());
        assert_eq!(config.server.host, default_host());
    }
}
