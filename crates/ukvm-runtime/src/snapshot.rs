//! Portable, bounded advisory status types. These records never grant authority.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ClipboardStatus {
    pub supported: Option<bool>,
    pub enabled: bool,
    pub connected_peers: usize,
    pub last_sync: Option<u64>,
    pub state: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeerStatus {
    pub id: String,
    pub coordinate: String,
    pub availability: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    pub pid: u32,
    pub updated: u64,
    pub name: String,
    pub node_id: String,
    pub state: String,
    pub focus: String,
    pub active_peer: Option<String>,
    pub active_coordinate: Option<String>,
    pub capture: String,
    pub receiver: String,
    pub peers: Vec<PeerStatus>,
    #[serde(default)]
    pub file_protocol_version: Option<u16>,
    #[serde(default)]
    pub display_protocol_version: Option<u16>,
    #[serde(default)]
    pub clipboard: ClipboardStatus,
}
#[derive(Debug)]
pub enum RuntimeStatus {
    Stopped,
    Running(Box<RuntimeSnapshot>),
    StartingOrUnavailable(String),
}
