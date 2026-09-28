//! Versioned semantic messages and the provisional development wire message set.

use serde::{Deserialize, Serialize};

/// First protocol major version. Incompatible peers must fail closed.
pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;

/// Stable identity reference; actual identity and trust storage belong to security code.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PeerId(pub String);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const CURRENT: Self = Self {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };

    pub const fn compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

/// Physical key identifier with an explicit code domain. The wire mapping is not fixed yet.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum KeyCode {
    LinuxEvdev(u32),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum InputEvent {
    /// Relative motion in the source device's logical coordinate space. It
    /// must be translated for the destination; no absolute mapping is fixed.
    PointerMotion {
        dx: f64,
        dy: f64,
    },
    PointerButton {
        button: u16,
        pressed: bool,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
    Key {
        code: KeyCode,
        pressed: bool,
    },
}

/// Negotiated, explicitly versioned messages for the development debug sink.
/// This is not yet a frozen interoperability format.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum WireMessage {
    Hello {
        version: ProtocolVersion,
        node_id: String,
        name: String,
        platform: String,
        controller: bool,
        debug_sink: bool,
    },
    SessionStart,
    SessionAccept,
    Input(InputEvent),
    Heartbeat(u64),
    HeartbeatAck(u64),
    SessionEnd,
    Error(String),
}

/// Candidate messages for a future authenticated transport. This is not a wire API.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Hello {
        version: ProtocolVersion,
        node: PeerId,
    },
    Heartbeat {
        sequence: u64,
    },
    ControlEnter {
        session: u64,
    },
    ControlLeave {
        session: u64,
    },
    Input {
        session: u64,
        event: InputEvent,
    },
    DisplayInfo {
        displays: Vec<DisplayInfo>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    pub id: String,
    pub width: u32,
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::ProtocolVersion;

    #[test]
    fn major_version_must_match() {
        assert!(ProtocolVersion::CURRENT.compatible_with(ProtocolVersion { major: 1, minor: 9 }));
        assert!(!ProtocolVersion::CURRENT.compatible_with(ProtocolVersion { major: 2, minor: 0 }));
    }
}
