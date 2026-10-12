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
pub struct Latest<T>(
    std::sync::Mutex<Option<T>>,
    std::sync::Condvar,
    std::sync::atomic::AtomicU64,
);
impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self(
            std::sync::Mutex::new(None),
            std::sync::Condvar::new(),
            std::sync::atomic::AtomicU64::new(0),
        )
    }
}
impl<T> Latest<T> {
    pub fn replace(&self, value: T) {
        let mut slot = self.0.lock().unwrap();
        if slot.replace(value).is_some() {
            self.2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.1.notify_one();
    }
    pub fn dropped(&self) -> u64 {
        self.2.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn depth(&self) -> usize {
        usize::from(self.0.lock().unwrap().is_some())
    }
    pub fn wait_take(&self, timeout: std::time::Duration) -> Option<T> {
        let slot = self.0.lock().unwrap();
        let (mut slot, _) = self
            .1
            .wait_timeout_while(slot, timeout, |s| s.is_none())
            .unwrap();
        slot.take()
    }
    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap().take()
    }
}
/// Pacing from the start of a send avoids counting network time twice.
/// Callers must validate the video preferences before computing a deadline.
pub fn video_deadline(
    start: std::time::Instant,
    completed: std::time::Instant,
    bytes: usize,
    options: ukvm_protocol::display::Options,
) -> std::time::Instant {
    let budget = std::time::Duration::from_secs_f64(
        bytes as f64 * 8. / (f64::from(options.mbps) * 1_000_000.),
    );
    if options.baseline {
        completed + budget
    } else {
        // When the link cannot sustain the target, reserve about 10% of the
        // observed exchange period for interactive traffic rather than driving
        // video continuously into a full socket. Healthy LANs keep the FPS cap.
        start
            + budget
                .max(options.interval())
                .max(completed.saturating_duration_since(start).mul_f64(1.1))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_video_send_does_not_add_another_network_wait() {
        let start = std::time::Instant::now();
        let end = start + std::time::Duration::from_millis(100);
        let options = ukvm_protocol::display::Options::default();
        let deadline = video_deadline(start, end, 1_000_000, options);
        assert!(deadline >= end);
        assert!(deadline <= end + std::time::Duration::from_millis(11));
        assert!(
            video_deadline(
                start,
                end,
                1_000_000,
                ukvm_protocol::display::Options::baseline()
            ) > end
        );
        assert_eq!(
            video_deadline(start, start, 0, options),
            start + options.interval()
        );
    }
    #[test]
    fn latest_wait_has_a_bounded_timeout_and_wakes_on_publish() {
        let latest = std::sync::Arc::new(Latest::default());
        assert_eq!(latest.wait_take(std::time::Duration::ZERO), None);
        let publisher = latest.clone();
        let worker = std::thread::spawn(move || publisher.replace(42));
        assert_eq!(
            latest.wait_take(std::time::Duration::from_secs(2)),
            Some(42)
        );
        worker.join().unwrap();
        assert_eq!(latest.depth(), 0);
    }
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
        assert_eq!(slot.depth(), 1);
        assert_eq!(slot.dropped(), 999);
        assert_eq!(slot.take(), Some(999));
        assert_eq!(slot.depth(), 0);
        assert_eq!(slot.take(), None);
    }
}
