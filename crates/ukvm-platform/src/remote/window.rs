//! Native Wayland-only window as an input source. No global grabs or injection.
use std::{
    collections::HashSet,
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use ukvm_core::remote::{Latest, Viewport};
use ukvm_protocol::{
    display::{Frame, ScreenRect},
    InputEvent, KeyCode,
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    platform::{scancode::PhysicalKeyExtScancode, wayland::EventLoopBuilderExtWayland},
    window::{Window, WindowId},
};
#[derive(Default)]
pub struct State {
    pub connected: bool,
    pub interactive: bool,
    pub controlling: bool,
    pub requesting: bool,
    pub message: String,
    pub closed: bool,
}
pub struct Shared {
    pub state: Mutex<State>,
    pub frames: Latest<Frame>,
    pub release: tokio::sync::watch::Sender<u64>,
}
impl Default for Shared {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                message: "Connecting".into(),
                ..State::default()
            }),
            frames: Latest::default(),
            release: tokio::sync::watch::channel(0).0,
        }
    }
}
#[derive(Debug)]
pub enum Command {
    Acquire {
        target: ukvm_protocol::display::WindowTarget,
        x: f64,
        y: f64,
        epoch: u64,
    },
    Event {
        event: InputEvent,
        epoch: u64,
    },
    Close,
}
pub fn run(
    peer: String,
    shared: Arc<Shared>,
    commands: mpsc::Sender<Command>,
) -> Result<(), String> {
    let event_loop = EventLoop::builder()
        .with_wayland()
        .build()
        .map_err(|e| e.to_string())?;
    let mut app = App {
        peer,
        shared,
        commands,
        window: None,
        surface: None,
        image: None,
        scaled: None,
        cursor: (0., 0.),
        keys: HashSet::new(),
        modifiers: winit::keyboard::ModifiersState::empty(),
        error: None,
        last_frame: Instant::now(),
        redraw: RedrawClock::new(Instant::now()),
    };
    let result = event_loop.run_app(&mut app).map_err(|e| e.to_string());
    result?;
    if let Some(error) = app.error {
        Err(error)
    } else {
        Ok(())
    }
}
struct Image {
    width: u32,
    height: u32,
    screen: Option<ScreenRect>,
    origin_known: bool,
    mapping_id: Option<String>,
    pixels: Vec<u32>,
}
struct App {
    peer: String,
    shared: Arc<Shared>,
    commands: mpsc::Sender<Command>,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    image: Option<Image>,
    scaled: Option<Scaled>,
    cursor: (f64, f64),
    keys: HashSet<u32>,
    modifiers: winit::keyboard::ModifiersState,
    error: Option<String>,
    last_frame: Instant,
    redraw: RedrawClock,
}
// request_redraw wakes winit immediately, even with WaitUntil. Only request one
// when the deadline expires; otherwise RedrawRequested/about_to_wait form a spin.
struct RedrawClock {
    next: Instant,
}
impl RedrawClock {
    fn new(now: Instant) -> Self {
        Self { next: now }
    }
    fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + Duration::from_millis(33);
        true
    }
}
impl App {
    fn release(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.controlling = false;
        state.requesting = false;
        drop(state);
        self.keys.clear();
        self.modifiers = winit::keyboard::ModifiersState::empty();
        self.shared
            .release
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
    fn event(&mut self, event: InputEvent) {
        if !self.shared.state.lock().unwrap().controlling {
            return;
        }
        if self
            .commands
            .try_send(Command::Event {
                event,
                epoch: *self.shared.release.borrow(),
            })
            .is_err()
        {
            self.release();
        }
    }
    fn position(&self) -> Option<(f64, f64)> {
        let image = self.image.as_ref()?;
        let size = self.window.as_ref()?.inner_size();
        Viewport::fit(
            (size.width, size.height.saturating_sub(48)),
            (image.width, image.height),
        )?
        .map(self.cursor.0, self.cursor.1 - 48., image.screen?)
    }
    fn title(&self) -> String {
        let state = self.shared.state.lock().unwrap();
        let status = if !state.connected {
            state.message.clone()
        } else if state.controlling {
            format!("Controlling {}", self.peer)
        } else if state.requesting {
            "Requesting control".into()
        } else if state.interactive && self.image.as_ref().is_some_and(|i| i.screen.is_some()) {
            format!("Click to control {}", state.message)
        } else {
            "View only".into()
        };
        format!(
            "{} — {} — Ctrl+Alt+Shift+Escape releases control",
            self.peer, status
        )
    }
    fn redraw(&mut self) -> Result<(), String> {
        if let Some(frame) = self.shared.frames.take() {
            self.image = Some(decode(frame)?);
            self.scaled = None;
            self.last_frame = Instant::now();
        }
        let Some(window) = self.window.as_ref() else {
            return Ok(());
        };
        window.set_title(&self.title());
        let size = window.inner_size();
        let Some(width) = NonZeroU32::new(size.width) else {
            return Ok(());
        };
        let Some(height) = NonZeroU32::new(size.height) else {
            return Ok(());
        };
        // Bound local redraw allocation too, including pathological resize events.
        if u64::from(size.width) * u64::from(size.height) > 16_777_216 {
            return Err("window exceeds rendering bound".into());
        }
        if self
            .scaled
            .as_ref()
            .is_none_or(|image| image.window != (size.width, size.height))
        {
            self.scaled = self
                .image
                .as_ref()
                .and_then(|image| scale_image(image, (size.width, size.height)));
        }
        let title = self.title();
        let surface = self.surface.as_mut().ok_or("window surface unavailable")?;
        surface.resize(width, height).map_err(|e| e.to_string())?;
        let mut buffer = surface.buffer_mut().map_err(|e| e.to_string())?;
        buffer.fill(0x151922);
        let heading = title.split(" — Ctrl+").next().unwrap_or(&title);
        draw_text(&mut buffer, size.width, size.height, heading, 8);
        draw_text(
            &mut buffer,
            size.width,
            size.height,
            "Ctrl+Alt+Shift+Escape releases control",
            28,
        );
        if let Some(image) = &self.scaled {
            for (y, row) in image.pixels.chunks_exact(image.width as usize).enumerate() {
                let dest = (y + image.y as usize) * size.width as usize + image.x as usize;
                buffer[dest..dest + row.len()].copy_from_slice(row);
            }
        }
        buffer.present().map_err(|e| e.to_string())
    }
}
impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let result = (|| {
            let window = Arc::new(
                el.create_window(
                    Window::default_attributes()
                        .with_title(self.title())
                        .with_inner_size(winit::dpi::LogicalSize::new(1280., 768.))
                        .with_min_inner_size(winit::dpi::LogicalSize::new(400., 240.)),
                )
                .map_err(|e| e.to_string())?,
            );
            let context = softbuffer::Context::new(window.clone()).map_err(|e| e.to_string())?;
            self.surface = Some(
                softbuffer::Surface::new(&context, window.clone()).map_err(|e| e.to_string())?,
            );
            self.window = Some(window);
            Ok::<_, String>(())
        })();
        if let Err(e) = result {
            self.error = Some(e);
            el.exit();
        }
    }
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.shared.state.lock().unwrap().closed {
            self.release();
            el.exit();
            return;
        }
        if self.error.is_some() {
            self.release();
            el.exit();
            return;
        }
        if self.last_frame.elapsed() > Duration::from_secs(5)
            && self.shared.state.lock().unwrap().connected
        {
            self.release();
            let mut state = self.shared.state.lock().unwrap();
            state.connected = false;
            state.message = "Display stalled; control released".into();
        }
        if self.redraw.due(Instant::now()) {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.redraw.next));
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.release();
                let _ = self.commands.try_send(Command::Close);
                el.exit();
            }
            WindowEvent::Focused(false) => self.release(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.redraw() {
                    self.error = Some(e);
                    self.release();
                    el.exit();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if let Some((x, y)) = self.position() {
                    self.event(InputEvent::PointerAbsolute { x, y });
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if pressed
                    && button == MouseButton::Left
                    && !self.shared.state.lock().unwrap().controlling
                {
                    let eligible = {
                        let s = self.shared.state.lock().unwrap();
                        s.connected && s.interactive && !s.requesting
                    };
                    if eligible {
                        if let Some((x, y)) = self.position() {
                            self.shared.state.lock().unwrap().requesting = true;
                            if self
                                .commands
                                .try_send(Command::Acquire {
                                    x,
                                    y,
                                    epoch: *self.shared.release.borrow(),
                                    target: ukvm_protocol::display::WindowTarget {
                                        origin_known: self.image.as_ref().unwrap().origin_known,
                                        screen: self.image.as_ref().unwrap().screen.unwrap(),
                                        mapping_id: self.image.as_ref().unwrap().mapping_id.clone(),
                                    },
                                })
                                .is_err()
                            {
                                self.release();
                            }
                        }
                    }
                } else {
                    let code = match button {
                        MouseButton::Left => 272,
                        MouseButton::Right => 273,
                        MouseButton::Middle => 274,
                        MouseButton::Back => 275,
                        MouseButton::Forward => 276,
                        _ => return,
                    };
                    self.event(InputEvent::PointerButton {
                        button: code,
                        pressed,
                    });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (f64::from(x) * 15., f64::from(y) * 15.),
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
                };
                self.event(InputEvent::Scroll { dx: -dx, dy: -dy });
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.repeat || {
                    let s = self.shared.state.lock().unwrap();
                    !s.controlling && !s.requesting
                } {
                    return;
                }
                if let Some(code) = event
                    .physical_key
                    .to_scancode()
                    .filter(|c| *c > 0 && *c <= 0x2ff)
                {
                    let pressed = event.state == ElementState::Pressed;
                    if pressed {
                        self.keys.insert(code);
                    } else {
                        self.keys.remove(&code);
                    }
                    if pressed
                        && code == 1
                        && (emergency(&self.keys)
                            || (self.modifiers.control_key()
                                && self.modifiers.alt_key()
                                && self.modifiers.shift_key()))
                    {
                        self.release();
                        return;
                    }
                    self.event(InputEvent::Key {
                        code: KeyCode::LinuxEvdev(code),
                        pressed,
                    });
                }
            }
            _ => {}
        }
    }
}
// One bounded scaled image per decoded frame/window size. Status redraws can
// copy it without repeating interpolation work when no new video arrived.
struct Scaled {
    window: (u32, u32),
    x: u32,
    y: u32,
    width: u32,
    pixels: Vec<u32>,
}
fn scale_image(image: &Image, window: (u32, u32)) -> Option<Scaled> {
    let v = Viewport::fit(
        (window.0, window.1.saturating_sub(48)),
        (image.width, image.height),
    )?;
    let width = v.width as u32;
    let height = v.height as u32;
    if width == 0 || height == 0 {
        return None;
    }
    let horizontal: Vec<_> = (0..width)
        .map(|x| sample_axis(x, v.width, image.width))
        .collect();
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        let vertical = sample_axis(y, v.height, image.height);
        for x in &horizontal {
            pixels.push(sample_pixel(image, *x, vertical));
        }
    }
    Some(Scaled {
        window,
        x: v.x as u32,
        y: v.y as u32 + 48,
        width,
        pixels,
    })
}

#[derive(Clone, Copy)]
struct SampleAxis {
    low: usize,
    high: usize,
    weight: u32,
}
fn sample_axis(position: u32, destination: f64, source: u32) -> SampleAxis {
    let center = ((f64::from(position) + 0.5) * f64::from(source) / destination - 0.5)
        .clamp(0., f64::from(source - 1));
    let low = center.floor() as usize;
    SampleAxis {
        low,
        high: (low + 1).min(source as usize - 1),
        weight: ((center - low as f64) * 256.).round() as u32,
    }
}
fn sample_pixel(image: &Image, x: SampleAxis, y: SampleAxis) -> u32 {
    let row = image.width as usize;
    let pixels = [
        image.pixels[y.low * row + x.low],
        image.pixels[y.low * row + x.high],
        image.pixels[y.high * row + x.low],
        image.pixels[y.high * row + x.high],
    ];
    let weights = [
        (256 - x.weight) * (256 - y.weight),
        x.weight * (256 - y.weight),
        (256 - x.weight) * y.weight,
        x.weight * y.weight,
    ];
    let mut result = 0;
    for shift in [0, 8, 16] {
        let channel: u32 = pixels
            .iter()
            .zip(weights)
            .map(|(pixel, weight)| ((pixel >> shift) & 255) * weight)
            .sum();
        result |= ((channel + 32768) / 65536) << shift;
    }
    result
}
fn emergency(keys: &HashSet<u32>) -> bool {
    (keys.contains(&29) || keys.contains(&97))
        && (keys.contains(&56) || keys.contains(&100))
        && (keys.contains(&42) || keys.contains(&54))
}
fn decode(frame: Frame) -> Result<Image, String> {
    frame.validate()?;
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(&frame.jpeg));
    decoder.set_max_decoding_buffer_size(1920 * 1080 * 4);
    decoder
        .read_info()
        .map_err(|_| "invalid display JPEG header")?;
    let info = decoder.info().ok_or("missing JPEG metadata")?;
    if u32::from(info.width) != frame.width
        || u32::from(info.height) != frame.height
        || info.pixel_format != jpeg_decoder::PixelFormat::RGB24
    {
        return Err("JPEG metadata differs from bounded display frame".into());
    }
    let rgb = decoder.decode().map_err(|_| "display JPEG decode failed")?;
    if rgb.len() != (frame.width * frame.height * 3) as usize {
        return Err("invalid JPEG pixel count".into());
    }
    let pixels = rgb
        .chunks_exact(3)
        .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
        .collect();
    Ok(Image {
        width: frame.width,
        height: frame.height,
        screen: frame.screen,
        origin_known: frame.origin_known,
        mapping_id: frame.mapping_id,
        pixels,
    })
}
fn draw_text(pixels: &mut [u32], width: u32, height: u32, text: &str, top: u32) {
    use font8x8::UnicodeFonts;
    for (index, ch) in text.chars().enumerate() {
        let x = 8 + index as u32 * 8;
        if x + 8 >= width {
            break;
        }
        if let Some(glyph) = font8x8::BASIC_FONTS.get(ch) {
            for (y, row) in glyph.iter().enumerate() {
                for bit in 0..8 {
                    if row & (1 << bit) != 0 && y as u32 + top < height {
                        pixels[((y as u32 + top) * width + x + bit) as usize] = 0xe4e9f2;
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scaling_interpolates_colors_and_keeps_native_pixels_exact() {
        let image = Image {
            width: 2,
            height: 1,
            screen: None,
            origin_known: false,
            mapping_id: None,
            pixels: vec![0xff0000, 0x0000ff],
        };
        assert_eq!(
            sample_pixel(&image, sample_axis(0, 2., 2), sample_axis(0, 1., 1)),
            0xff0000
        );
        assert_eq!(
            sample_pixel(&image, sample_axis(1, 2., 2), sample_axis(0, 1., 1)),
            0x0000ff
        );
        assert_eq!(
            sample_pixel(&image, sample_axis(0, 1., 2), sample_axis(0, 1., 1)),
            0x800080
        );
        assert_eq!(sample_axis(0, 4., 2).low, 0);
        assert_eq!(sample_axis(3, 4., 2).high, 1);
        // Cached rows must remain inside the resized surface and outside its
        // status header, including portrait/letterboxed and one-pixel content.
        assert!(scale_image(&image, (1, 49)).is_none());
        for window in [(17, 61), (31, 75), (11, 96), (2, 49)] {
            let scaled = scale_image(&image, window).unwrap();
            assert_eq!(scaled.window, window);
            assert!(scaled.y >= 48);
            for (y, row) in scaled
                .pixels
                .chunks_exact(scaled.width as usize)
                .enumerate()
            {
                let start = (scaled.y as usize + y) * window.0 as usize + scaled.x as usize;
                assert!(start + row.len() <= (window.0 * window.1) as usize);
                assert!(scaled.x + row.len() as u32 <= window.0);
            }
        }
    }
    #[test]
    fn redraw_wakeups_do_not_create_a_busy_loop() {
        let now = Instant::now();
        let mut clock = RedrawClock::new(now);
        assert!(clock.due(now));
        for _ in 0..1000 {
            assert!(!clock.due(now));
        }
        assert!(!clock.due(now + Duration::from_millis(32)));
        assert!(clock.due(now + Duration::from_millis(33)));
        // After a stalled/minimized window, skip missed frames instead of
        // issuing a burst of catch-up redraws.
        let later = now + Duration::from_secs(10);
        assert!(clock.due(later));
        assert!(!clock.due(later));
        assert_eq!(clock.next, later + Duration::from_millis(33));
    }
    #[test]
    fn shortcut_requires_all_modifiers() {
        assert!(emergency(&HashSet::from([29, 56, 42])));
        assert!(emergency(&HashSet::from([97, 100, 54])));
        assert!(!emergency(&HashSet::from([29, 56])));
    }
    #[test]
    fn jpeg_header_cannot_override_frame_bounds() {
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, 65)
            .encode(&[0; 12], 2, 2, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        let f = Frame {
            sequence: 0,
            width: 1,
            height: 1,
            screen: None,
            origin_known: false,
            mapping_id: None,
            jpeg,
        };
        assert!(decode(f).is_err());
    }
    #[test]
    fn release_bypasses_full_input_queue_and_clears_held_keys() {
        let shared = Arc::new(Shared::default());
        shared.state.lock().unwrap().controlling = true;
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(Command::Event {
            event: InputEvent::PointerButton {
                button: 272,
                pressed: true,
            },
            epoch: 0,
        })
        .unwrap();
        let mut release = shared.release.subscribe();
        let mut app = App {
            peer: "peer".into(),
            shared: shared.clone(),
            commands: tx,
            window: None,
            surface: None,
            image: None,
            scaled: None,
            cursor: (0., 0.),
            keys: HashSet::from([42]),
            modifiers: winit::keyboard::ModifiersState::empty(),
            error: None,
            last_frame: Instant::now(),
            redraw: RedrawClock::new(Instant::now()),
        };
        app.release();
        assert_eq!(*release.borrow_and_update(), 1);
        assert!(!shared.state.lock().unwrap().controlling);
        assert!(app.keys.is_empty());
        assert!(matches!(
            rx.try_recv().unwrap(),
            Command::Event { epoch: 0, .. }
        ));
        assert!(app.error.is_none());
    }
}
