//! Platform filesystem seams; transfer contents never enter presentation state.
use serde::{Deserialize, Serialize};
use ukvm_protocol::files::{Offer, CHUNK_BYTES};
pub trait FileSource: Send {
    fn offer(&self) -> &Offer;
    fn read_chunk(&mut self, index: usize) -> Result<Vec<u8>, String>;
}
pub trait FileReceiver: Send {
    fn write_chunk(&mut self, index: usize, bytes: &[u8]) -> Result<(), String>;
    fn finish_file(&mut self, index: usize) -> Result<(), String>;
    fn commit(&mut self) -> Result<String, String>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferStatus {
    pub id: String,
    pub peer_id: String,
    pub peer_name: String,
    pub direction: String,
    pub name: String,
    pub state: String,
    pub bytes: u64,
    pub total_bytes: u64,
    pub entries: usize,
    pub started: u64,
    pub updated: u64,
    pub destination: Option<String>,
    pub error: Option<String>,
}
impl TransferStatus {
    pub fn active(&self) -> bool {
        matches!(
            self.state.as_str(),
            "preparing" | "connecting" | "transferring" | "verifying"
        )
    }
}
pub const MAX_BUFFERED_BYTES: usize = CHUNK_BYTES;
