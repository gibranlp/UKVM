//! Versioned semantic messages and the provisional development wire message set.

pub mod clipboard;

use serde::{Deserialize, Serialize};

/// First protocol major version. Incompatible peers must fail closed.
pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

/// A position along an edge, measured from top or left, in [0, 1].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct EdgePosition {
    pub edge: Edge,
    pub fraction: f32,
}

impl EdgePosition {
    pub fn validate(self) -> Result<Self, String> {
        if self.fraction.is_finite() && (0.0..=1.0).contains(&self.fraction) {
            Ok(self)
        } else {
            Err("invalid edge position".into())
        }
    }
}

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
        self.major == other.major && self.minor == other.minor
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

/// Validate values before any sink sees them, including non-network callers.
pub fn validate_event(event: &InputEvent) -> Result<(), String> {
    let valid_delta = |value: f64| value.is_finite() && value.abs() <= 32768.0;
    match event {
        InputEvent::PointerMotion { dx, dy } | InputEvent::Scroll { dx, dy } => {
            if !valid_delta(*dx) || !valid_delta(*dy) {
                return Err("invalid input delta".into());
            }
        }
        InputEvent::PointerButton { button, .. } if *button == 0 || *button > 0x2ff => {
            return Err("invalid button code".into())
        }
        InputEvent::Key {
            code: KeyCode::LinuxEvdev(code),
            ..
        } if *code == 0 || *code > 0x2ff => return Err("invalid key code".into()),
        _ => {}
    }
    Ok(())
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
        remote_input: bool,
    },
    SessionStart,
    SessionAccept,
    Input(InputEvent),
    Heartbeat(u64),
    HeartbeatAck(u64),
    SessionEnd,
    Error(String),
    /// Separate from SessionStart, which always selects the non-injecting DebugSink.
    RemoteSessionStart,
    RemoteSessionAccept,
    /// Sent after InputCapture activation and before any remote input.
    RemoteEntry(EdgePosition),
    /// Sent by the controlled node after it releases held input state.
    ReturnControl(EdgePosition),
    /// Source-selected permitted exits; never grants trust or input permission.
    TopologyEntry {
        entry: EdgePosition,
        exits: Vec<Edge>,
    },
    /// Refresh ready exits without recreating the active permissioned session.
    TopologyExits(Vec<Edge>),
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
        assert!(ProtocolVersion::CURRENT.compatible_with(ProtocolVersion { major: 1, minor: 3 }));
        assert!(!ProtocolVersion::CURRENT.compatible_with(ProtocolVersion { major: 1, minor: 1 }));
        assert!(!ProtocolVersion::CURRENT.compatible_with(ProtocolVersion { major: 1, minor: 2 }));
        assert!(!ProtocolVersion::CURRENT.compatible_with(ProtocolVersion { major: 2, minor: 0 }));
    }
}
