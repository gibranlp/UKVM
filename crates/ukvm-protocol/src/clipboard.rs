use serde::{Deserialize, Serialize};

pub const CLIPBOARD_VERSION: u16 = 1;
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;
pub const CLIPBOARD_MIME: &str = "text/plain;charset=utf-8";

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum ClipboardMessage {
    Hello {
        version: u16,
        mime: String,
        max_bytes: u32,
    },
    Text {
        origin: String,
        epoch: [u8; 16],
        generation: u64,
        byte_length: u32,
        text: String,
    },
    Ping,
    Pong,
}

impl std::fmt::Debug for ClipboardMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClipboardMessage(redacted)")
    }
}

impl ClipboardMessage {
    pub fn hello() -> Self {
        Self::Hello {
            version: CLIPBOARD_VERSION,
            mime: CLIPBOARD_MIME.into(),
            max_bytes: MAX_CLIPBOARD_BYTES as u32,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hello {
                version,
                mime,
                max_bytes,
            } if *version == CLIPBOARD_VERSION
                && mime == CLIPBOARD_MIME
                && *max_bytes == MAX_CLIPBOARD_BYTES as u32 =>
            {
                Ok(())
            }
            Self::Text {
                origin,
                generation,
                byte_length,
                text,
                ..
            } if origin.len() == 64
                && origin.bytes().all(|b| b.is_ascii_hexdigit())
                && *generation > 0
                && text.len() <= MAX_CLIPBOARD_BYTES
                && *byte_length as usize == text.len() =>
            {
                Ok(())
            }
            Self::Ping | Self::Pong => Ok(()),
            _ => Err("invalid or incompatible clipboard message".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_limits_unicode_empty_and_redacted_debug() {
        for text in ["", "hello", "世界 🦀"] {
            let message = ClipboardMessage::Text {
                origin: "a".repeat(64),
                epoch: [1; 16],
                generation: 1,
                byte_length: text.len() as u32,
                text: text.into(),
            };
            assert!(message.validate().is_ok());
            assert_eq!(format!("{message:?}"), "ClipboardMessage(redacted)");
        }
        let mut message = ClipboardMessage::Text {
            origin: "a".repeat(64),
            epoch: [1; 16],
            generation: 1,
            byte_length: 1,
            text: "private".into(),
        };
        assert!(message.validate().is_err());
        if let ClipboardMessage::Text {
            text, byte_length, ..
        } = &mut message
        {
            *text = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
            *byte_length = text.len() as u32;
        }
        assert!(message.validate().is_err());
    }
    #[test]
    fn capability_version_mime_and_limit_fail_closed() {
        assert!(ClipboardMessage::hello().validate().is_ok());
        for message in [
            ClipboardMessage::Hello {
                version: 2,
                mime: CLIPBOARD_MIME.into(),
                max_bytes: MAX_CLIPBOARD_BYTES as u32,
            },
            ClipboardMessage::Hello {
                version: 1,
                mime: "text/html".into(),
                max_bytes: MAX_CLIPBOARD_BYTES as u32,
            },
            ClipboardMessage::Hello {
                version: 1,
                mime: CLIPBOARD_MIME.into(),
                max_bytes: 0,
            },
        ] {
            assert!(message.validate().is_err());
        }
    }
}
