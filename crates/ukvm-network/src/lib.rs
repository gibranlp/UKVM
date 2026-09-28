//! Authenticated TLS transport and bounded development wire protocol.

pub mod identity;
pub mod transport;

use bincode::Options;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ukvm_protocol::{InputEvent, KeyCode, ProtocolVersion, WireMessage};

pub const DEFAULT_PORT: u16 = 24800;
pub const MAX_FRAME: usize = 512;
pub const EVENT_QUEUE: usize = 256;

fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_FRAME as u64)
        .reject_trailing_bytes()
}

pub fn encode(message: &WireMessage) -> Result<Vec<u8>, String> {
    validate(message)?;
    let payload = codec().serialize(message).map_err(|e| e.to_string())?;
    if payload.len() > MAX_FRAME {
        return Err("frame too large".into());
    }
    Ok(payload)
}

pub fn decode(payload: &[u8]) -> Result<WireMessage, String> {
    if payload.is_empty() || payload.len() > MAX_FRAME {
        return Err("invalid frame size".into());
    }
    let message: WireMessage = codec()
        .deserialize(payload)
        .map_err(|e| format!("malformed message: {e}"))?;
    validate(&message)?;
    Ok(message)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &WireMessage,
) -> Result<usize, String> {
    validate(message)?;
    let len = codec()
        .serialized_size(message)
        .map_err(|e| e.to_string())? as usize;
    if len == 0 || len > MAX_FRAME {
        return Err("frame too large".into());
    }
    let mut payload = [0u8; MAX_FRAME];
    codec()
        .serialize_into(&mut &mut payload[..len], message)
        .map_err(|e| e.to_string())?;
    let len = u32::try_from(len).map_err(|e| e.to_string())?;
    writer
        .write_all(&len.to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    writer
        .write_all(&payload[..len as usize])
        .await
        .map_err(|e| e.to_string())?;
    writer.flush().await.map_err(|e| e.to_string())?;
    Ok(len as usize + 4)
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<(WireMessage, usize), String> {
    let mut header = [0u8; 4];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|e| e.to_string())?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(format!("invalid frame length {len}"));
    }
    let mut payload = [0u8; MAX_FRAME];
    reader
        .read_exact(&mut payload[..len])
        .await
        .map_err(|e| e.to_string())?;
    Ok((decode(&payload[..len])?, len + 4))
}

pub fn validate(message: &WireMessage) -> Result<(), String> {
    match message {
        WireMessage::Hello {
            version,
            node_id,
            name,
            platform,
            ..
        } => {
            if !ProtocolVersion::CURRENT.compatible_with(*version) {
                return Err("incompatible protocol major version".into());
            }
            if version.minor > ProtocolVersion::CURRENT.minor {
                return Err("unsupported protocol minor version".into());
            }
            if node_id.len() != 64 || !node_id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid node ID".into());
            }
            if name.is_empty() || name.len() > 63 || !name.chars().all(|c| !c.is_control()) {
                return Err("invalid node name".into());
            }
            if platform.is_empty()
                || platform.len() > 20
                || !platform
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err("invalid platform".into());
            }
        }
        WireMessage::Input(event) => validate_event(event)?,
        WireMessage::Error(message) if message.len() > 120 => {
            return Err("error text too long".into())
        }
        _ => {}
    }
    Ok(())
}

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EventCounts {
    pub pointer: u64,
    pub buttons: u64,
    pub scroll: u64,
    pub keyboard: u64,
}

impl EventCounts {
    pub fn add(&mut self, event: &InputEvent) {
        match event {
            InputEvent::PointerMotion { .. } => self.pointer += 1,
            InputEvent::PointerButton { .. } => self.buttons += 1,
            InputEvent::Scroll { .. } => self.scroll += 1,
            InputEvent::Key { .. } => self.keyboard += 1,
        }
    }
    pub fn total(&self) -> u64 {
        self.pointer + self.buttons + self.scroll + self.keyboard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_rejects_untrusted_lengths_and_invalid_payload() {
        let message = WireMessage::Input(InputEvent::PointerMotion { dx: 1.0, dy: -2.0 });
        assert_eq!(decode(&encode(&message).unwrap()).unwrap(), message);
        assert!(decode(&[255, 255, 255, 255]).is_err());
        assert!(encode(&WireMessage::Input(InputEvent::PointerMotion {
            dx: f64::NAN,
            dy: 0.0
        }))
        .is_err());
        assert!(decode(&vec![0; MAX_FRAME + 1]).is_err());
    }
    #[test]
    fn major_version_and_names_are_checked() {
        let hello = WireMessage::Hello {
            version: ProtocolVersion { major: 9, minor: 0 },
            node_id: "a".repeat(64),
            name: "node".into(),
            platform: "linux".into(),
            controller: true,
            debug_sink: true,
        };
        assert!(encode(&hello).is_err());
    }
    #[tokio::test]
    async fn oversized_length_rejected_before_body_read() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        writer
            .write_all(&((MAX_FRAME as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(read_frame(&mut reader)
            .await
            .unwrap_err()
            .contains("invalid frame length"));
    }
    #[test]
    fn debug_counts_do_not_retain_input_values() {
        let mut counts = EventCounts::default();
        counts.add(&InputEvent::Key {
            code: KeyCode::LinuxEvdev(42),
            pressed: true,
        });
        assert_eq!(counts.keyboard, 1);
    }
}
