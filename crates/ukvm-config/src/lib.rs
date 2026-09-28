//! Human-readable, non-secret node preferences. Trust material is never stored here.

use serde::Deserialize;
use std::path::PathBuf;
use ukvm_core::Role;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub node: Node,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub name: String,
    #[serde(default)]
    pub role: ConfigRole,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigRole {
    Controller,
    Controlled,
    #[default]
    Both,
}

impl From<ConfigRole> for Role {
    fn from(role: ConfigRole) -> Self {
        match role {
            ConfigRole::Controller => Self::Controller,
            ConfigRole::Controlled => Self::Controlled,
            ConfigRole::Both => Self::Both,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    NoConfigDirectory,
    Io(std::io::Error),
    Parse(toml::de::Error),
    InvalidName,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoConfigDirectory => write!(f, "unable to determine user config directory"),
            Self::Io(err) => write!(f, "config I/O error: {err}"),
            Self::Parse(err) => write!(f, "invalid TOML config: {err}"),
            Self::InvalidName => write!(f, "node.name must be nonempty and at most 63 characters"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Returns the user config path without reading or creating files.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or(ConfigError::NoConfigDirectory)?;
        Ok(base.join("ukvm").join("config.toml"))
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or(ConfigError::NoConfigDirectory)?;
        Ok(PathBuf::from(home).join("Library/Application Support/UniversalKVM/config.toml"))
    }

    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("APPDATA").ok_or(ConfigError::NoConfigDirectory)?;
        Ok(PathBuf::from(base).join("UniversalKVM").join("config.toml"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(ConfigError::NoConfigDirectory)
    }
}

pub fn load(path: &std::path::Path) -> Result<Config, ConfigError> {
    let source = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
    let config: Config = toml::from_str(&source).map_err(ConfigError::Parse)?;
    if config.node.name.is_empty() || config.node.name.chars().count() > 63 {
        return Err(ConfigError::InvalidName);
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigRole};

    #[test]
    fn parse_minimal_config() {
        let config: Config = toml::from_str("[node]\nname = 'desktop'\n").unwrap();
        assert_eq!(config.node.role, ConfigRole::Both);
    }

    #[test]
    fn unknown_config_is_rejected() {
        assert!(toml::from_str::<Config>("[node]\nname = 'desktop'\nsecret = 'bad'\n").is_err());
    }
}
