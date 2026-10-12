//! Display v2: independent JPEG images; never carries input or grants authority.
use serde::{Deserialize, Serialize};
pub const VERSION: u16 = 2;
pub const MAX_JPEG: usize = 8 * 1024 * 1024;
pub const MAX_FRAME: usize = MAX_JPEG + 1024;
pub const MAX_WIDTH: u32 = 8192;
pub const MAX_HEIGHT: u32 = 8192;
/// Viewer preferences are bounded independently of trust and portal consent.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Options {
    pub baseline: bool,
    pub fps: u16,
    pub quality: u8,
    pub mbps: u16,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            baseline: false,
            fps: 60,
            quality: 97,
            mbps: 500,
        }
    }
}
impl Options {
    pub fn baseline() -> Self {
        Self {
            baseline: true,
            fps: 30,
            quality: 92,
            mbps: 67,
        }
    }
    pub fn validate(self) -> Result<(), String> {
        if self.baseline && self != Self::baseline() {
            return Err("baseline uses fixed FPS, quality and bitrate settings".into());
        }
        if !(1..=60).contains(&self.fps)
            || !(45..=100).contains(&self.quality)
            || !(10..=1000).contains(&self.mbps)
        {
            return Err("invalid display preferences".into());
        }
        Ok(())
    }
    pub fn interval(self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(1. / f64::from(self.fps))
    }
}
/// Four monotonic timestamps estimate viewer-minus-source clock offset.
/// Half the exchange delay expresses uncertainty from network asymmetry.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct ClockEstimate {
    pub offset_us: i64,
    pub uncertainty_us: u64,
}
impl ClockEstimate {
    pub fn exchange(t1: u64, t2: u64, t3: u64, t4: u64) -> Result<Self, String> {
        let roundtrip = t4.checked_sub(t1).ok_or("clock exchange regressed")?;
        let processing = t3
            .checked_sub(t2)
            .ok_or("clock acknowledgement regressed")?;
        let delay = roundtrip
            .checked_sub(processing)
            .ok_or("invalid clock exchange delay")?;
        let offset = ((i128::from(t2) - i128::from(t1)) + (i128::from(t3) - i128::from(t4))) / 2;
        Ok(Self {
            offset_us: i64::try_from(offset).map_err(|_| "clock offset exceeds bounds")?,
            uncertainty_us: delay / 2,
        })
    }
}
/// Durations, never wall-clock timestamps or screen contents.
#[derive(Clone, Copy, Default, Debug, Deserialize, Serialize, PartialEq)]
pub struct Telemetry {
    pub sent_us: u64,
    pub clock: Option<ClockEstimate>,
    pub encoded_depth: u8,
    pub raw_depth: u8,
    pub source_width: u32,
    pub source_height: u32,
    pub capture_fps: f64,
    pub copy_us: u64,
    pub encode_queue_us: u64,
    pub encode_us: u64,
    pub source_age_us: u64,
    pub previous_ack_us: u64,
    pub pipewire_dropped: u64,
    pub capture_throttled: u64,
    pub raw_dropped: u64,
    pub encoded_dropped: u64,
}
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
    pub telemetry: Telemetry,
    pub width: u32,
    pub height: u32,
    /// Portal compositor geometry, not encoded pixel dimensions. None is view-only.
    pub screen: Option<ScreenRect>,
    pub origin_known: bool,
    pub mapping_id: Option<String>,
    pub jpeg: std::sync::Arc<Vec<u8>>,
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
            || u64::from(self.width) * u64::from(self.height) > 16_777_216
            || self.telemetry.encoded_depth > 1
            || self.telemetry.raw_depth > 1
            || !self.telemetry.capture_fps.is_finite()
            || self.telemetry.capture_fps < 0.
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
    Configure(Options),
    Ready {
        interactive: bool,
    },
    Frame(Frame),
    Ack {
        sequence: u64,
        received_us: u64,
        sent_us: u64,
    },
}
impl Message {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Hello(version) if *version != VERSION => {
                Err("incompatible display version".into())
            }
            Self::Frame(frame) => frame.validate(),
            Self::Configure(options) => options.validate(),
            _ => Ok(()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_estimate_accounts_for_processing_offset_and_asymmetry() {
        // Viewer clock is 1 second ahead; symmetric 100us each way,
        // with 50us viewer processing excluded from transit uncertainty.
        let estimate = ClockEstimate::exchange(100, 1_000_200, 1_000_250, 350).unwrap();
        assert_eq!(estimate.offset_us, 1_000_000);
        assert_eq!(estimate.uncertainty_us, 100);
        let asymmetric = ClockEstimate::exchange(100, 1_000_150, 1_000_200, 350).unwrap();
        assert_eq!(asymmetric.offset_us, 999_950);
        assert_eq!(asymmetric.uncertainty_us, 100);
        assert!(ClockEstimate::exchange(100, 200, 199, 300).is_err());
        assert!(ClockEstimate::exchange(100, 200, 500, 300).is_err());
        assert!(ClockEstimate::exchange(100, 200, 200, 99).is_err());
        assert!(ClockEstimate::exchange(0, u64::MAX, u64::MAX, 0).is_err());
    }
    #[test]
    fn video_preferences_fail_closed_and_do_not_change_authority() {
        Options::default().validate().unwrap();
        Options::baseline().validate().unwrap();
        for invalid in [
            Options {
                fps: 0,
                ..Options::default()
            },
            Options {
                fps: 61,
                ..Options::default()
            },
            Options {
                quality: 101,
                ..Options::default()
            },
            Options {
                mbps: 1001,
                ..Options::default()
            },
        ] {
            assert!(Message::Configure(invalid).validate().is_err());
        }
        assert!(Message::Hello(1).validate().is_err());
    }
    #[test]
    fn bounded_geometry_and_frames() {
        let mut frame = Frame {
            sequence: 1,
            telemetry: Telemetry::default(),
            width: 1920,
            height: 1080,
            screen: None,
            origin_known: false,
            mapping_id: None,
            jpeg: std::sync::Arc::new(vec![1]),
        };
        frame.validate().unwrap();
        let cloned = frame.clone();
        assert!(std::sync::Arc::ptr_eq(&cloned.jpeg, &frame.jpeg));
        frame.telemetry.capture_fps = f64::NAN;
        assert!(frame.validate().is_err());
        frame.telemetry.capture_fps = 0.;
        frame.width = 8192;
        frame.height = 8192;
        assert!(frame.validate().is_err());
        frame.height = 1080;
        frame.width = u32::MAX;
        assert!(frame.validate().is_err());
        frame.width = 1920;
        frame.jpeg = std::sync::Arc::new(vec![0; MAX_JPEG + 1]);
        assert!(frame.validate().is_err());
        assert!(ScreenRect {
            x: i32::MIN,
            y: 0,
            width: 1,
            height: 1
        }
        .validate()
        .is_err());
        assert!(Message::Hello(1).validate().is_err());
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
