//! File Protocol 1 is independent of input and clipboard negotiation.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
pub const VERSION: u16 = 1;
pub const CHUNK_BYTES: usize = 65_536;
pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_FRAME: usize = 4 * 1024 * 1024;
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub const BYTES_PER_SECOND: u64 = 16 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub path: Vec<String>,
    pub kind: Kind,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub entries: Vec<Entry>,
    pub total_bytes: u64,
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub fn component(name: &str) -> Result<(), String> {
    let base = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if name.is_empty()
        || name.len() > 240
        || matches!(name, "." | "..")
        || name.chars().any(|c| {
            c.is_control()
                || matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')
                || "/\\:<>\"|?*".contains(c)
        })
        || name.ends_with(['.', ' '])
        || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.chars().count() == 4
            && matches!(base.chars().nth(3), Some('1'..='9' | '¹' | '²' | '³')))
    {
        return Err("unsafe or nonportable filename".into());
    }
    Ok(())
}
impl Offer {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id)
            || self.entries.is_empty()
            || self.entries.len() > MAX_ENTRIES
            || self.total_bytes > MAX_BYTES
        {
            return Err("invalid transfer offer limits".into());
        }
        let mut paths = HashSet::new();
        let mut directories = HashSet::new();
        let mut total = 0u64;
        for entry in &self.entries {
            if entry.path.is_empty()
                || entry.path.len() > 64
                || entry.path.iter().map(String::len).sum::<usize>() > 4096
            {
                return Err("invalid transfer path".into());
            }
            for name in &entry.path {
                component(name)?;
            }
            let path = entry
                .path
                .iter()
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>();
            if !paths.insert(path.clone())
                || (path.len() > 1 && !directories.contains(&path[..path.len() - 1]))
            {
                return Err("duplicate path or missing parent directory".into());
            }
            match entry.kind {
                Kind::Directory => {
                    if entry.size != 0 {
                        return Err("directory declared nonzero size".into());
                    }
                    directories.insert(path);
                }
                Kind::File => total = total.checked_add(entry.size).ok_or("size overflow")?,
            }
        }
        if total != self.total_bytes {
            return Err("declared transfer size mismatch".into());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Message {
    Hello {
        version: u16,
        chunk_bytes: u32,
    },
    Offer(Offer),
    Accepted,
    Chunk {
        index: u32,
        offset: u64,
        bytes: Vec<u8>,
    },
    FileEnd {
        index: u32,
        size: u64,
        sha256: [u8; 32],
    },
    Ack {
        bytes: u64,
    },
    Complete,
    Result {
        destination: String,
    },
    Cancel,
    Rejected,
}
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Hello { .. } => "Hello",
            Self::Offer(_) => "Offer",
            Self::Accepted => "Accepted",
            Self::Chunk { .. } => "Chunk [redacted]",
            Self::FileEnd { .. } => "FileEnd",
            Self::Ack { .. } => "Ack",
            Self::Complete => "Complete",
            Self::Result { .. } => "Result",
            Self::Cancel => "Cancel",
            Self::Rejected => "Rejected",
        };
        f.write_str(kind)
    }
}
impl Message {
    pub fn hello() -> Self {
        Self::Hello {
            version: VERSION,
            chunk_bytes: CHUNK_BYTES as u32,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hello {
                version,
                chunk_bytes,
            } if *version != VERSION || *chunk_bytes != CHUNK_BYTES as u32 => {
                Err("incompatible file protocol capability".into())
            }
            Self::Offer(offer) => offer.validate(),
            Self::Chunk { bytes, .. } if bytes.is_empty() || bytes.len() > CHUNK_BYTES => {
                Err("invalid file chunk length".into())
            }
            Self::Result { destination } => component(destination),
            _ => Ok(()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostile_names_and_metadata() {
        for name in [
            "../x",
            "/root",
            "C:\\test",
            "..",
            "a/b",
            "a\\b",
            "a\n.txt",
            "NUL.txt",
            "COM1",
            "foo.",
            "foo ",
            "",
            "safe\u{202e}exe.txt",
        ] {
            assert!(component(name).is_err(), "{name}");
        }
        for name in ["你好.txt", "a b.txt", "photo.jpg", "report (1).pdf"] {
            assert!(component(name).is_ok());
        }
        let mut o = Offer {
            id: "a".repeat(32),
            entries: vec![
                Entry {
                    path: vec!["dir".into()],
                    kind: Kind::Directory,
                    size: 0,
                },
                Entry {
                    path: vec!["dir".into(), "a".into()],
                    kind: Kind::File,
                    size: 2,
                },
            ],
            total_bytes: 2,
        };
        assert!(o.validate().is_ok());
        o.entries.push(o.entries[1].clone());
        assert!(o.validate().is_err());
        o.entries.pop();
        o.total_bytes = 3;
        assert!(o.validate().is_err());
    }
    #[test]
    fn chunk_redaction() {
        let m = Message::Chunk {
            index: 0,
            offset: 0,
            bytes: b"private-secret-file".to_vec(),
        };
        assert!(!format!("{m:?}").contains("private-secret"));
        assert!(Message::Chunk {
            index: 0,
            offset: 0,
            bytes: vec![0; CHUNK_BYTES + 1]
        }
        .validate()
        .is_err());
    }
}
