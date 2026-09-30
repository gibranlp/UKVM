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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<TopologyNode>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TopologyNode {
    pub x: i32,
    pub y: i32,
    pub peer_id: String,
}
impl TopologyNode {
    pub fn coordinate(&self) -> ukvm_core::Coordinate {
        ukvm_core::Coordinate {
            x: self.x,
            y: self.y,
        }
    }
}
impl Layout {
    pub fn adjacent_link(&self, edge: Edge) -> Option<Link> {
        self.links
            .iter()
            .find(|link| link.edge == edge)
            .cloned()
            .or_else(|| {
                self.nodes
                    .iter()
                    .find(|node| node.coordinate() == edge.coordinate())
                    .map(|node| Link {
                        edge,
                        entry_edge: edge.opposite(),
                        peer_id: node.peer_id.clone(),
                    })
            })
    }
    /// Existing edge links remain valid distance-one coordinates.
    pub fn topology(&self) -> Vec<TopologyNode> {
        let mut nodes = self.nodes.clone();
        nodes.extend(self.links.iter().map(|link| {
            let (x, y) = match link.edge {
                Edge::Left => (-1, 0),
                Edge::Right => (1, 0),
                Edge::Top => (0, -1),
                Edge::Bottom => (0, 1),
            };
            TopologyNode {
                x,
                y,
                peer_id: link.peer_id.clone(),
            }
        }));
        nodes
    }
    pub fn validate_topology(&self) -> Result<(), ConfigError> {
        let nodes = self.topology();
        let mut coordinates = std::collections::HashSet::new();
        let mut identities = std::collections::HashSet::new();
        for node in &nodes {
            if !matches!((node.x, node.y), (-2..=-1 | 1..=2, 0) | (0, -2..=-1 | 1..=2)) {
                return Err(ConfigError::Invalid(
                    "topology requires cardinal coordinates at distance 1–2 from local (0,0)"
                        .into(),
                ));
            }
            if !coordinates.insert(node.coordinate()) {
                return Err(ConfigError::Invalid(format!(
                    "conflicting topology coordinate {}",
                    node.coordinate()
                )));
            }
            if node.peer_id.len() != 64 || !node.peer_id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ConfigError::Invalid("invalid topology peer ID".into()));
            }
            if !identities.insert(node.peer_id.to_ascii_lowercase()) {
                return Err(ConfigError::Invalid(
                    "one peer cannot occupy multiple topology coordinates".into(),
                ));
            }
        }
        for node in &nodes {
            if (node.x.abs() == 2 || node.y.abs() == 2)
                && !coordinates.contains(&ukvm_core::Coordinate {
                    x: node.x / 2,
                    y: node.y / 2,
                })
            {
                return Err(ConfigError::Invalid(
                    "distance-two node requires its distance-one adjacency".into(),
                ));
            }
        }
        Ok(())
    }
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
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];
    pub fn coordinate(self) -> ukvm_core::Coordinate {
        let (x, y) = match self {
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
            Self::Top => (0, -1),
            Self::Bottom => (0, 1),
        };
        ukvm_core::Coordinate { x, y }
    }
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
        self.layout.validate_topology()
    }
    pub fn remove_peer_from_layout(&mut self, peer_id: &str) {
        self.layout.links.retain(|link| link.peer_id != peer_id);
        self.layout.nodes.retain(|node| node.peer_id != peer_id);
        let remaining = self.layout.topology();
        self.layout.nodes.retain(|node| {
            node.x.abs() + node.y.abs() == 1
                || remaining
                    .iter()
                    .any(|inner| inner.x == node.x / 2 && inner.y == node.y / 2)
        });
    }
    pub fn set_position(&mut self, x: i32, y: i32, peer_id: String) -> Result<(), ConfigError> {
        let mut next = self.clone();
        next.layout.nodes.retain(|node| node.x != x || node.y != y);
        next.layout
            .links
            .retain(|link| link.edge.coordinate() != ukvm_core::Coordinate { x, y });
        next.layout.nodes.push(TopologyNode { x, y, peer_id });
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub fn remove_position(&mut self, x: i32, y: i32) -> Result<(), ConfigError> {
        let mut next = self.clone();
        let old = next.layout.topology().len();
        next.layout.nodes.retain(|node| node.x != x || node.y != y);
        next.layout
            .links
            .retain(|link| link.edge.coordinate() != ukvm_core::Coordinate { x, y });
        if old == next.layout.topology().len() {
            return Err(ConfigError::Invalid("no node at coordinate".into()));
        }
        next.validate()?;
        *self = next;
        Ok(())
    }
    pub fn set_link(&mut self, edge: Edge, peer_id: String) {
        self.layout.links.retain(|l| l.edge != edge);
        self.layout
            .nodes
            .retain(|node| node.coordinate() != edge.coordinate());
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

#[cfg(test)]
mod topology_tests {
    use super::*;
    fn node(x: i32, y: i32, id: char) -> TopologyNode {
        TopologyNode {
            x,
            y,
            peer_id: id.to_string().repeat(64),
        }
    }
    #[test]
    fn parses_and_roundtrips_all_eight_cardinal_positions() {
        let mut config = Config::default();
        for (index, (x, y)) in [
            (1, 0),
            (2, 0),
            (-1, 0),
            (-2, 0),
            (0, -1),
            (0, -2),
            (0, 1),
            (0, 2),
        ]
        .into_iter()
        .enumerate()
        {
            config
                .layout
                .nodes
                .push(node(x, y, char::from(b'a' + index as u8 % 6)));
            config.layout.nodes[index].peer_id = format!("{index:064x}");
        }
        config.validate().unwrap();
        let source = toml::to_string_pretty(&config).unwrap();
        assert!(source.contains("[[layout.nodes]]"));
        let decoded: Config = toml::from_str(&source).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, config);
    }
    #[test]
    fn duplicate_conflicting_diagonal_local_far_and_gapped_positions_fail_closed() {
        for nodes in [
            vec![node(1, 0, 'a'), node(1, 0, 'b')],
            vec![node(1, 0, 'a'), node(0, -1, 'a')],
            vec![node(1, 1, 'a')],
            vec![node(0, 0, 'a')],
            vec![node(3, 0, 'a')],
            vec![node(2, 0, 'a')],
            vec![node(i32::MIN, 0, 'a')],
        ] {
            let mut config = Config::default();
            config.layout.nodes = nodes;
            assert!(config.validate().is_err());
        }
        assert!(toml::from_str::<Config>(
            "[node]\nname='test'\n[[layout.nodes]]\nx=1\nx=2\ny=0\npeer_id='bad'\n"
        )
        .is_err());
        let mut config = Config::default();
        config.set_link(Edge::Right, "a".repeat(64));
        config.layout.nodes.push(node(1, 0, 'b'));
        assert!(config.validate().is_err());
    }
    #[test]
    fn placement_and_removal_are_atomic_and_legacy_adjacency_is_retained() {
        let mut config = Config::default();
        config.set_link(Edge::Right, "a".repeat(64));
        config.set_position(2, 0, "b".repeat(64)).unwrap();
        let before = config.clone();
        assert!(config.set_position(0, -2, "c".repeat(64)).is_err());
        assert!(config.remove_position(1, 0).is_err());
        assert_eq!(config, before);
        assert_eq!(
            config.layout.topology()[0]
                .coordinate()
                .adjacent(ukvm_core::Edge::Right),
            ukvm_core::Coordinate { x: 3, y: 0 }
        );
        config.remove_position(2, 0).unwrap();
        config.remove_position(1, 0).unwrap();
        assert!(config.layout.topology().is_empty());
    }
}
