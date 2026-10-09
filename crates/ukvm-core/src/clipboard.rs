use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};
use ukvm_protocol::clipboard::{ClipboardMessage, MAX_CLIPBOARD_BYTES};

pub struct ClipboardState {
    origin: String,
    epoch: [u8; 16],
    generation: u64,
    current: Option<String>,
    received: HashMap<String, ([u8; 16], u64)>,
    stamp: Option<(u64, String, [u8; 16])>,
}
impl ClipboardState {
    pub fn new(origin: &str, epoch: [u8; 16]) -> Self {
        Self {
            origin: origin.into(),
            epoch,
            generation: 0,
            current: None,
            received: HashMap::new(),
            stamp: None,
        }
    }
    pub fn matches(&self, text: &str) -> bool {
        self.current.as_deref() == Some(text)
    }
    pub fn clear(&mut self) {
        self.current = None;
        self.received.clear();
        self.stamp = None;
    }

    pub fn local(&mut self, text: String) -> Option<ClipboardMessage> {
        if text.len() > MAX_CLIPBOARD_BYTES || self.current.as_ref() == Some(&text) {
            return None;
        }
        self.current = Some(text.clone());
        let clock = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .unwrap_or(0);
        self.generation = self.generation.checked_add(1)?.max(clock);
        self.stamp = Some((self.generation, self.origin.clone(), self.epoch));
        Some(ClipboardMessage::Text {
            origin: self.origin.clone(),
            epoch: self.epoch,
            generation: self.generation,
            byte_length: text.len() as u32,
            text,
        })
    }
    pub fn remote(
        &mut self,
        peer: &str,
        message: &ClipboardMessage,
    ) -> Result<Option<String>, String> {
        message.validate()?;
        let ClipboardMessage::Text {
            origin,
            epoch,
            generation,
            text,
            ..
        } = message
        else {
            return Err("unexpected clipboard payload".into());
        };
        if origin != peer || origin == &self.origin {
            return Err("clipboard origin differs from authenticated peer".into());
        }
        if self
            .received
            .get(peer)
            .is_some_and(|(old, seq)| old == epoch && *seq >= *generation)
        {
            return Ok(None);
        }
        if self.received.len() >= 64 && !self.received.contains_key(peer) {
            return Err("clipboard peer capacity reached".into());
        }
        self.received.insert(peer.into(), (*epoch, *generation));
        self.generation = self.generation.max(*generation);
        let stamp = (*generation, origin.clone(), *epoch);
        if self.stamp.as_ref().is_some_and(|old| old >= &stamp) {
            return Ok(None);
        }
        self.stamp = Some(stamp);
        if self.current.as_ref() == Some(text) {
            return Ok(None);
        }
        self.current = Some(text.clone());
        Ok(Some(text.clone()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn node(c: char) -> ClipboardState {
        ClipboardState {
            origin: c.to_string().repeat(64),
            epoch: [c as u8; 16],
            generation: 0,
            current: None,
            received: HashMap::new(),
            stamp: None,
        }
    }
    #[test]
    fn bidirectional_unicode_empty_identical_and_echo() {
        let (mut a, mut b) = (node('a'), node('b'));
        for text in ["hello", "世界 🦀", ""] {
            let message = a.local(text.into()).unwrap();
            assert_eq!(b.remote(&a.origin, &message).unwrap(), Some(text.into()));
            assert!(b.local(text.into()).is_none());
            assert!(a.local(text.into()).is_none());
            assert!(b.remote(&a.origin, &message).unwrap().is_none());
        }
        let message = b.local("world".into()).unwrap();
        assert_eq!(a.remote(&b.origin, &message).unwrap(), Some("world".into()));
        assert!(a.local("world".into()).is_none());
    }
    #[test]
    fn multi_peer_origin_and_reconnect_generation() {
        let (mut a, mut b, mut c) = (node('a'), node('b'), node('c'));
        let message = a.local("private".into()).unwrap();
        assert!(b.remote(&c.origin, &message).is_err());
        assert!(b.remote(&a.origin, &message).unwrap().is_some());
        assert!(c.remote(&a.origin, &message).unwrap().is_some());
        assert!(b.remote(&a.origin, &message).unwrap().is_none());
        let mut restarted = node('a');
        restarted.epoch = [9; 16];
        restarted.generation = a.generation;
        let message = restarted.local("after restart".into()).unwrap();
        assert!(b.remote(&a.origin, &message).unwrap().is_some());
        assert!(!format!("{message:?}").contains("after restart"));
    }
    #[test]
    fn simultaneous_changes_converge_without_relay() {
        let node = |c: char| ClipboardState {
            origin: c.to_string().repeat(64),
            epoch: [c as u8; 16],
            generation: 0,
            current: None,
            received: HashMap::new(),
            stamp: None,
        };
        let (mut a, mut b) = (node('a'), node('b'));
        let a_message = a.local("A".into()).unwrap();
        let b_message = b.local("B".into()).unwrap();
        a.remote(&b.origin, &b_message).unwrap();
        b.remote(&a.origin, &a_message).unwrap();
        assert_eq!(a.current, b.current);
        assert!(a.local(a.current.clone().unwrap()).is_none());
        assert!(b.local(b.current.clone().unwrap()).is_none());
    }
}
