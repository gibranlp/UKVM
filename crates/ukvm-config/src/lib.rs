//! Human-readable node preferences. Trust records and private identity live elsewhere.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use ukvm_core::Role;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub node: Node,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub layout: Layout,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub name: String,
    #[serde(default)]
    pub role: ConfigRole,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigRole {
    Controller,
    Controlled,
    #[default]
    Both,
}
impl From<ConfigRole> for Role {
    fn from(value: ConfigRole) -> Self {
        match value {
            ConfigRole::Controller => Self::Controller,
            ConfigRole::Controlled => Self::Controlled,
            ConfigRole::Both => Self::Both,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Network {
    #[serde(default = "default_listen")]
    pub listen: String,
}
fn default_listen() -> String {
    "0.0.0.0:24800".into()
}
impl Default for Network {
    fn default() -> Self {
        Self {
            listen: default_listen(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(default)]
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub edge: Edge,
    pub peer_id: String,
    pub entry_edge: Edge,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}
impl Edge {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            _ => Err("edge must be left, right, top, or bottom".into()),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    NoConfigDirectory,
    Io(std::io::Error),
    Parse(toml::de::Error),
    Invalid(String),
}
impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoConfigDirectory => write!(f, "unable to determine user config directory"),
            Self::Io(e) => write!(f, "config I/O error: {e}"),
            Self::Parse(e) => write!(f, "invalid TOML config: {e}"),
            Self::Invalid(e) => write!(f, "invalid config: {e}"),
        }
    }
}
impl std::error::Error for ConfigError {}

pub fn default_path() -> Result<PathBuf, ConfigError> {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok_or(ConfigError::NoConfigDirectory)?;
        Ok(base.join("ukvm/config.toml"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or(ConfigError::NoConfigDirectory)?;
        Ok(PathBuf::from(home).join("Library/Application Support/UniversalKVM/config.toml"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("APPDATA").ok_or(ConfigError::NoConfigDirectory)?;
        Ok(PathBuf::from(base).join("UniversalKVM/config.toml"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(ConfigError::NoConfigDirectory)
    }
}
pub fn default_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|n| valid_name(n))
        .or_else(|| {
            #[cfg(target_os = "linux")]
            {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|n| valid_name(n))
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        })
        .unwrap_or_else(|| "ukvm-node".into())
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= 63 && !name.chars().any(char::is_control)
}
impl Default for Config {
    fn default() -> Self {
        Self {
            node: Node {
                name: default_name(),
                role: ConfigRole::Both,
            },
            network: Network::default(),
            layout: Layout::default(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !valid_name(&self.node.name) {
            return Err(ConfigError::Invalid(
                "node.name must be 1-63 printable characters".into(),
            ));
        }
        if self.network.listen.parse::<std::net::SocketAddr>().is_err() {
            return Err(ConfigError::Invalid(
                "network.listen must be an IP address and port".into(),
            ));
        }
        let mut edges = std::collections::HashSet::new();
        for link in &self.layout.links {
            if !edges.insert(link.edge) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate {} layout edge",
                    link.edge.as_str()
                )));
            }
            if link.peer_id.len() != 64 || !link.peer_id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ConfigError::Invalid(format!(
                    "invalid peer ID on {} edge",
                    link.edge.as_str()
                )));
            }
        }
        Ok(())
    }
    pub fn set_link(&mut self, edge: Edge, peer_id: String) {
        self.layout.links.retain(|l| l.edge != edge);
        self.layout.links.push(Link {
            edge,
            peer_id,
            entry_edge: edge.opposite(),
        });
    }
    pub fn remove_link(&mut self, edge: Edge) -> bool {
        let len = self.layout.links.len();
        self.layout.links.retain(|l| l.edge != edge);
        self.layout.links.len() != len
    }
}
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let source = fs::read_to_string(path).map_err(ConfigError::Io)?;
    let config: Config = toml::from_str(&source).map_err(ConfigError::Parse)?;
    config.validate()?;
    Ok(config)
}
pub fn load_or_default(path: &Path) -> Result<Config, ConfigError> {
    match load(path) {
        Err(ConfigError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Config::default())
        }
        other => other,
    }
}
pub fn save(path: &Path, config: &Config) -> Result<(), ConfigError> {
    config.validate()?;
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::Invalid("config path has no parent".into()))?;
    fs::create_dir_all(parent).map_err(ConfigError::Io)?;
    let source = toml::to_string_pretty(config).map_err(|e| ConfigError::Invalid(e.to_string()))?;
    let temp = parent.join(format!(
        ".config-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| -> Result<(), ConfigError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(ConfigError::Io)?;
        file.write_all(source.as_bytes()).map_err(ConfigError::Io)?;
        file.sync_all().map_err(ConfigError::Io)?;
        fs::rename(&temp, path).map_err(ConfigError::Io)?;
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_minimal_config() {
        let config: Config = toml::from_str("[node]\nname = 'desktop'\n").unwrap();
        assert_eq!(config.node.role, ConfigRole::Both);
        assert_eq!(config.network.listen, default_listen());
    }
    #[test]
    fn unknown_config_is_rejected() {
        assert!(toml::from_str::<Config>("[node]\nname = 'desktop'\nsecret = 'bad'\n").is_err());
    }
    #[test]
    fn layout_validation_and_roundtrip() {
        let mut c = Config::default();
        c.set_link(Edge::Right, "a".repeat(64));
        c.set_link(Edge::Right, "b".repeat(64));
        assert_eq!(c.layout.links.len(), 1);
        let s = toml::to_string(&c).unwrap();
        let d: Config = toml::from_str(&s).unwrap();
        d.validate().unwrap();
        assert!(c.remove_link(Edge::Right));
        assert!(!c.remove_link(Edge::Right));
    }
    #[test]
    fn layout_entry_edges_are_inverse_in_all_directions() {
        for (outbound, inbound) in [
            (Edge::Right, Edge::Left),
            (Edge::Left, Edge::Right),
            (Edge::Top, Edge::Bottom),
            (Edge::Bottom, Edge::Top),
        ] {
            let mut config = Config::default();
            config.set_link(outbound, "a".repeat(64));
            assert_eq!(config.layout.links[0].entry_edge, inbound);
        }
    }
    #[test]
    fn invalid_layout() {
        let mut c = Config::default();
        c.set_link(Edge::Left, "bad".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn save_replaces_complete_config_and_keeps_old_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[node]\nname = 'legacy'\n").unwrap();
        let mut config = load(&path).unwrap();
        assert_eq!(config.node.name, "legacy");
        config.set_link(Edge::Right, "a".repeat(64));
        save(&path, &config).unwrap();
        assert_eq!(load(&path).unwrap(), config);
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }
}
