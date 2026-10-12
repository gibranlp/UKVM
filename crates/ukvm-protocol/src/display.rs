//! Display v1: independent JPEG images; never carries input or grants authority.
use serde::{Deserialize, Serialize};
pub const VERSION: u16 = 1;
pub const MAX_JPEG: usize = 2 * 1024 * 1024;
pub const MAX_FRAME: usize = MAX_JPEG + 256;
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
impl ScreenRect {
    pub fn validate(self) -> Result<(), String> {
        if self.width == 0
            || self.height == 0
            || self.width > 16384
            || self.height > 16384
            || self.x.abs_diff(0) > 32768
            || self.y.abs_diff(0) > 32768
            || i64::from(self.x) + i64::from(self.width) > 32768
            || i64::from(self.y) + i64::from(self.height) > 32768
        {
            return Err("invalid screen geometry".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WindowTarget {
    pub screen: ScreenRect,
    pub origin_known: bool,
    pub mapping_id: Option<String>,
}
impl WindowTarget {
    pub fn validate(&self) -> Result<(), String> {
        self.screen.validate()?;
        if self
            .mapping_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        {
            return Err("invalid screen mapping ID".into());
        }
        Ok(())
    }
    pub fn fractions(&self, x: f64, y: f64) -> Result<(f64, f64), String> {
        self.validate()?;
        let x = (x - f64::from(self.screen.x)) / f64::from(self.screen.width);
        let y = (y - f64::from(self.screen.y)) / f64::from(self.screen.height);
        if !x.is_finite() || !y.is_finite() || !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y)
        {
            return Err("pointer outside selected screen".into());
        }
        Ok((x, y))
    }
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
pub struct Frame {
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    /// Portal compositor geometry, not encoded pixel dimensions. None is view-only.
    pub screen: Option<ScreenRect>,
    pub origin_known: bool,
    pub mapping_id: Option<String>,
    pub jpeg: Vec<u8>,
}
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("sequence", &self.sequence)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}
impl Frame {
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_WIDTH
            || self.height > MAX_HEIGHT
            || self.jpeg.is_empty()
            || self.jpeg.len() > MAX_JPEG
        {
            return Err("display frame exceeds bounds".into());
        }
        if self
            .mapping_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        {
            return Err("invalid screen mapping ID".into());
        }
        if let Some(rect) = self.screen {
            WindowTarget {
                screen: rect,
                origin_known: self.origin_known,
                mapping_id: self.mapping_id.clone(),
            }
            .validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum Message {
    Hello(u16),
    Ready { interactive: bool },
    Frame(Frame),
    Ack(u64),
}
impl Message {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hello(version) if *version != VERSION => {
                Err("incompatible display version".into())
            }
            Self::Frame(frame) => frame.validate(),
            _ => Ok(()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_geometry_and_frames() {
        let mut frame = Frame {
            sequence: 1,
            width: 1920,
            height: 1080,
            screen: None,
            origin_known: false,
            mapping_id: None,
            jpeg: vec![1],
        };
        frame.validate().unwrap();
        frame.width = u32::MAX;
        assert!(frame.validate().is_err());
        frame.width = 1920;
        frame.jpeg = vec![0; MAX_JPEG + 1];
        assert!(frame.validate().is_err());
        assert!(ScreenRect {
            x: i32::MIN,
            y: 0,
            width: 1,
            height: 1
        }
        .validate()
        .is_err());
        assert!(Message::Hello(2).validate().is_err());
    }
    #[test]
    fn logical_fractions_are_independent_of_video_resolution() {
        let target = WindowTarget {
            origin_known: true,
            screen: ScreenRect {
                x: -3840,
                y: 200,
                width: 3840,
                height: 2160,
            },
            mapping_id: Some("output-1".into()),
        };
        assert_eq!(target.fractions(-1920., 1280.).unwrap(), (0.5, 0.5));
        assert!(target.fractions(0., 1280.).is_err());
        assert!(target.fractions(f64::NAN, 1280.).is_err());
        let mut invalid = target;
        invalid.mapping_id = Some("x".repeat(129));
        assert!(invalid.validate().is_err());
    }
}
