//! Portable remote-window geometry and bounded latest-value handoff.
use ukvm_protocol::display::ScreenRect;
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Viewport {
    pub fn fit(window: (u32, u32), image: (u32, u32)) -> Option<Self> {
        if window.0 == 0 || window.1 == 0 || image.0 == 0 || image.1 == 0 {
            return None;
        }
        let scale = (f64::from(window.0) / f64::from(image.0))
            .min(f64::from(window.1) / f64::from(image.1));
        let width = f64::from(image.0) * scale;
        let height = f64::from(image.1) * scale;
        Some(Self {
            x: (f64::from(window.0) - width) / 2.,
            y: (f64::from(window.1) - height) / 2.,
            width,
            height,
        })
    }
    pub fn map(self, x: f64, y: f64, screen: ScreenRect) -> Option<(f64, f64)> {
        screen.validate().ok()?;
        if !x.is_finite()
            || !y.is_finite()
            || x < self.x
            || y < self.y
            || x >= self.x + self.width
            || y >= self.y + self.height
        {
            return None;
        }
        Some((
            f64::from(screen.x) + (x - self.x) / self.width * f64::from(screen.width),
            f64::from(screen.y) + (y - self.y) / self.height * f64::from(screen.height),
        ))
    }
}
/// A single replaceable slot: no backlog regardless of consumer speed.
pub struct Latest<T>(std::sync::Mutex<Option<T>>);
impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self(std::sync::Mutex::new(None))
    }
}
impl<T> Latest<T> {
    pub fn replace(&self, value: T) {
        *self.0.lock().unwrap() = Some(value);
    }
    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap().take()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scaling_letterbox_and_negative_origin() {
        let v = Viewport::fit((1000, 1000), (1920, 1080)).unwrap();
        let r = ScreenRect {
            x: -3840,
            y: 200,
            width: 3840,
            height: 2160,
        };
        assert!(v.map(500., 0., r).is_none());
        assert_eq!(v.map(500., 500., r), Some((-1920., 1280.)));
        assert!(v.map(f64::NAN, 500., r).is_none());
        assert!(Viewport::fit((0, 1), (1, 1)).is_none());
    }
    #[test]
    fn slow_consumer_gets_only_newest() {
        let slot = Latest::default();
        for n in 0..1000 {
            slot.replace(n);
        }
        assert_eq!(slot.take(), Some(999));
        assert_eq!(slot.take(), None);
    }
}
