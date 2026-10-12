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
    pub frames: Latest<Received>,
    pub options: ukvm_protocol::display::Options,
    pub performance: Mutex<Performance>,
    prepared: Latest<Result<Prepared, String>>,
    size: Mutex<(u32, u32)>,
    wake_pending: std::sync::atomic::AtomicBool,
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
            options: Default::default(),
            performance: Mutex::new(Performance::default()),
            prepared: Latest::default(),
            size: Mutex::new((1280, 768)),
            wake_pending: std::sync::atomic::AtomicBool::new(false),
            release: tokio::sync::watch::channel(0).0,
        }
    }
}
pub struct Received {
    pub frame: Frame,
    pub received_us: u64,
    pub received: Instant,
}
impl Received {
    fn timing(&self) -> Timing {
        let telemetry = self.frame.telemetry;
        let transit_ms = telemetry.clock.map(|clock| {
            (i128::from(self.received_us)
                - i128::from(telemetry.sent_us)
                - i128::from(clock.offset_us)) as f64
                / 1000.
        });
        Timing {
            received: self.received,
            source_age_us: telemetry.source_age_us,
            decode_ms: 0.,
            encode_ms: telemetry.encode_us as f64 / 1000.,
            decode_queue_ms: self.received.elapsed().as_secs_f64() * 1000.,
            transit_ms,
        }
    }
}
struct Prepared {
    image: Arc<Image>,
    scaled: Option<Scaled>,
    scale_ms: f64,
    ready: Instant,
}
#[derive(Default)]
pub struct Performance {
    pub received: u64,
    pub bytes: u64,
    pub presented: u64,
    pub source: ukvm_protocol::display::Telemetry,
    pub source_resolution: (u32, u32),
    pub rendered_resolution: (u32, u32),
    first_received: Option<Instant>,
    last_presented: Option<Instant>,
    samples: std::collections::VecDeque<[f64; 9]>,
}
impl Performance {
    fn sample(&mut self, values: [f64; 9]) {
        if self.samples.len() == 300 {
            self.samples.pop_front();
        }
        self.samples.push_back(values);
    }
    pub fn receive(&mut self, frame: &Frame) {
        self.first_received.get_or_insert_with(Instant::now);
        self.received += 1;
        self.bytes += frame.jpeg.len() as u64;
        self.source = frame.telemetry;
        self.source_resolution = (frame.width, frame.height);
    }
    pub fn report(&self, elapsed: Duration, queues: [usize; 2], dropped: [u64; 2]) -> String {
        let seconds = self
            .first_received
            .map(|t| t.elapsed())
            .unwrap_or(elapsed)
            .as_secs_f64()
            .max(0.001);
        let mut means = [f64::NAN; 9];
        let mut p95 = [f64::NAN; 9];
        for i in 0..9 {
            let mut values: Vec<_> = self
                .samples
                .iter()
                .map(|s| s[i])
                .filter(|v| v.is_finite())
                .collect();
            values.sort_by(f64::total_cmp);
            if !values.is_empty() {
                means[i] = values.iter().sum::<f64>() / values.len() as f64;
                p95[i] = values[(values.len() - 1) * 95 / 100];
            }
        }
        format!("remote-performance capture_fps={:.1} receive_fps={:.1} submit_fps={:.1} mbps={:.1} source={}x{} encoded={}x{} rendered={}x{} encode_ms={:.2} decode_ms={:.2} scale_ms={:.2} submit_ms={:.2} age_lower_ms={:.2} age_lower_p95_ms={:.2} previous_write_ack_ms={:.2} transit_est_ms={} age_est_ms={} age_est_p95_ms={} clock_uncertainty_ms={:.2} capture_copy_ms={:.2} encode_queue_ms={:.2} send_queue_ms={:.2} decode_queue_ms={:.2} render_queue_ms={:.2} scanout=unmeasured drops_pipewire={} capture_throttled={} drops_raw={} drops_encoded={} drops_decode={} drops_render={} queues_raw_encoded_decode_render={}/{}/{}/{}",
            self.source.capture_fps, self.received as f64/seconds, self.presented as f64/seconds, self.bytes as f64*8./seconds/1_000_000., self.source.source_width,self.source.source_height,self.source_resolution.0,self.source_resolution.1,self.rendered_resolution.0,self.rendered_resolution.1, means[4], means[0],means[1],means[2],means[3],p95[3],self.source.previous_ack_us as f64/1000.,
            metric(means[5]),metric(means[6]),metric(p95[6]),self.source.clock.map(|c|c.uncertainty_us as f64/1000.).unwrap_or(0.),self.source.copy_us as f64/1000.,self.source.encode_queue_us as f64/1000.,self.source.source_age_us.saturating_sub(self.source.copy_us).saturating_sub(self.source.encode_queue_us).saturating_sub(self.source.encode_us) as f64/1000.,means[7],means[8],self.source.pipewire_dropped,self.source.capture_throttled,self.source.raw_dropped,self.source.encoded_dropped,dropped[0],dropped[1],self.source.raw_depth,self.source.encoded_depth,queues[0],queues[1])
    }
}
fn metric(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.2}")
    } else {
        "unavailable".into()
    }
}
impl Shared {
    pub fn with_options(options: ukvm_protocol::display::Options) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }
    pub fn report(&self, elapsed: Duration) -> String {
        let report = self.performance.lock().unwrap().report(
            elapsed,
            [self.frames.depth(), self.prepared.depth()],
            [self.frames.dropped(), self.prepared.dropped()],
        );
        format!(
            "{report} mode={} target_fps={} quality={} ceiling_mbps={}",
            if self.options.baseline {
                "baseline"
            } else {
                "simd-native"
            },
            self.options.fps,
            self.options.quality,
            self.options.mbps
        )
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
#[derive(Clone, Copy)]
struct VideoReady;
pub fn run(
    peer: String,
    shared: Arc<Shared>,
    commands: mpsc::Sender<Command>,
) -> Result<(), String> {
    let event_loop = EventLoop::<VideoReady>::with_user_event()
        .with_wayland()
        .build()
        .map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let worker_shared = shared.clone();
    let worker_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = worker_stop.clone();
    let worker = std::thread::Builder::new()
        .name("ukvm-display-decode-scale".into())
        .spawn(move || {
            let mut previous: Option<Arc<Image>> = None;
            let mut previous_size = (0, 0);
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                if worker_shared.options.baseline {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                let next = worker_shared.frames.wait_take(Duration::from_millis(20));
                let size = *worker_shared.size.lock().unwrap();
                if let Some(received) = next {
                    let started = Instant::now();
                    let timing = received.timing();
                    let frame = received.frame;
                    let decoded = if worker_shared.options.baseline {
                        decode_baseline(frame)
                    } else {
                        decode(frame)
                    };
                    match decoded {
                        Ok(mut image) => {
                            image.timing = Some(Timing {
                                decode_ms: started.elapsed().as_secs_f64() * 1000.,
                                ..timing
                            });
                            previous = Some(Arc::new(image));
                        }
                        Err(error) => {
                            worker_shared.prepared.replace(Err(error));
                            break;
                        }
                    }
                } else if size == previous_size {
                    continue;
                }
                if let Some(image) = previous.as_ref() {
                    let started = Instant::now();
                    let scaled = scale_image(image, size);
                    worker_shared.prepared.replace(Ok(Prepared {
                        image: image.clone(),
                        scaled,
                        scale_ms: started.elapsed().as_secs_f64() * 1000.,
                        ready: Instant::now(),
                    }));
                    previous_size = size;
                    // Coalesce notifications as well as frames: a minimized or
                    // stalled UI cannot accumulate an unbounded event backlog.
                    if !worker_shared
                        .wake_pending
                        .swap(true, std::sync::atomic::Ordering::AcqRel)
                        && proxy.send_event(VideoReady).is_err()
                    {
                        break;
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let redraw = RedrawClock {
        next: Instant::now(),
        interval: if shared.options.baseline {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(100)
        },
    };
    let mut app = App {
        peer,
        shared,
        commands,
        shortcuts: None,
        focused: false,
        window: None,
        surface: None,
        image: None,
        scaled: None,
        cursor: (0., 0.),
        keys: HashSet::new(),
        modifiers: winit::keyboard::ModifiersState::empty(),
        error: None,
        last_frame: Instant::now(),
        redraw,
    };
    let result = event_loop.run_app(&mut app).map_err(|e| e.to_string());
    app.release();
    app.shortcuts = None;
    worker_stop.store(true, std::sync::atomic::Ordering::Release);
    let _ = worker.join();
    result?;
    if let Some(error) = app.error {
        Err(error)
    } else {
        Ok(())
    }
}
struct Timing {
    encode_ms: f64,
    decode_queue_ms: f64,
    transit_ms: Option<f64>,
    received: Instant,
    source_age_us: u64,
    decode_ms: f64,
}
struct Image {
    timing: Option<Timing>,
    width: u32,
    height: u32,
    screen: Option<ScreenRect>,
    origin_known: bool,
    mapping_id: Option<String>,
    pixels: Arc<Vec<u32>>,
}
struct App {
    peer: String,
    shared: Arc<Shared>,
    commands: mpsc::Sender<Command>,
    shortcuts: Option<super::shortcuts::Shortcuts>,
    focused: bool,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    image: Option<Arc<Image>>,
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
    interval: Duration,
}
impl RedrawClock {
    #[cfg(test)]
    fn new(now: Instant) -> Self {
        Self {
            next: now,
            interval: Duration::from_millis(16),
        }
    }
    fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + self.interval;
        true
    }
}
impl App {
    fn release(&mut self) {
        if let Some(shortcuts) = &mut self.shortcuts {
            shortcuts.release();
        }
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
            format!(
                "Controlling {} — {}",
                self.peer,
                self.shortcuts
                    .as_ref()
                    .map_or("local shortcuts unavailable", |s| s.status())
            )
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
        if self.shared.options.baseline {
            if let Some(received) = self.shared.frames.take() {
                let timing = received.timing();
                let start = Instant::now();
                let mut image = decode_baseline(received.frame)?;
                image.timing = Some(Timing {
                    decode_ms: start.elapsed().as_secs_f64() * 1000.,
                    ..timing
                });
                self.shared.prepared.replace(Ok(Prepared {
                    image: Arc::new(image),
                    scaled: None,
                    scale_ms: 0.,
                    ready: Instant::now(),
                }));
            }
        }
        let mut new_frame = false;
        let mut scale_ms = 0.;
        let mut render_queue_ms = 0.;
        if let Some(prepared) = self.shared.prepared.take() {
            let prepared = prepared?;
            new_frame = self
                .image
                .as_ref()
                .is_none_or(|image| !Arc::ptr_eq(image, &prepared.image));
            if self.image.is_none() && !self.shared.options.baseline {
                if let Some(window) = self.window.as_ref() {
                    let limit = window
                        .current_monitor()
                        .map(|m| m.size())
                        .unwrap_or(winit::dpi::PhysicalSize::new(1920, 1080));
                    let width = prepared.image.width.min(limit.width.saturating_mul(9) / 10);
                    let height =
                        (prepared.image.height + 48).min(limit.height.saturating_mul(9) / 10);
                    let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(width, height));
                }
            }
            self.image = Some(prepared.image);
            self.scaled = prepared.scaled;
            scale_ms = prepared.scale_ms;
            render_queue_ms = prepared.ready.elapsed().as_secs_f64() * 1000.;
            self.last_frame = Instant::now();
        }
        let render_started = Instant::now();
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
        *self.shared.size.lock().unwrap() = (size.width, size.height);
        if self.shared.options.baseline
            && (new_frame
                || self
                    .scaled
                    .as_ref()
                    .is_none_or(|s| s.window != (size.width, size.height)))
        {
            let start = Instant::now();
            self.scaled = self
                .image
                .as_ref()
                .and_then(|image| scale_image(image, (size.width, size.height)));
            scale_ms = start.elapsed().as_secs_f64() * 1000.;
        }
        // Resizing/scaling is serviced by the worker; never block input callbacks.
        if self
            .scaled
            .as_ref()
            .is_some_and(|s| s.window != (size.width, size.height))
        {
            self.scaled = None;
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
        buffer.present().map_err(|e| e.to_string())?;
        if self.scaled.is_some() {
            if let Some(image) = self.image.as_ref() {
                if let Some(timing) = image.timing.as_ref() {
                    let mut stats = self.shared.performance.lock().unwrap();
                    if stats.last_presented == Some(timing.received) {
                        return Ok(());
                    }
                    stats.last_presented = Some(timing.received);
                    stats.presented += 1;
                    if let Some(scaled) = self.scaled.as_ref() {
                        stats.rendered_resolution =
                            (scaled.width, scaled.pixels.len() as u32 / scaled.width);
                    }
                    stats.sample([
                        timing.decode_ms,
                        scale_ms,
                        render_started.elapsed().as_secs_f64() * 1000.,
                        timing.source_age_us as f64 / 1000.
                            + timing.received.elapsed().as_secs_f64() * 1000.,
                        timing.encode_ms,
                        timing.transit_ms.unwrap_or(f64::NAN),
                        timing
                            .transit_ms
                            .map(|t| {
                                (timing.source_age_us as f64 / 1000.
                                    + t
                                    + timing.received.elapsed().as_secs_f64() * 1000.)
                                    .max(0.)
                            })
                            .unwrap_or(f64::NAN),
                        timing.decode_queue_ms,
                        render_queue_ms,
                    ]);
                }
            }
        }
        Ok(())
    }
}
impl ApplicationHandler<VideoReady> for App {
    fn user_event(&mut self, _el: &ActiveEventLoop, _event: VideoReady) {
        self.shared
            .wake_pending
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.release();
        // Tear down guest Wayland objects before winit closes its display.
        self.shortcuts = None;
    }
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
            match super::shortcuts::Shortcuts::new(window.clone()) {
                Ok(shortcuts) => self.shortcuts = Some(shortcuts),
                Err(error) => eprintln!("UKVM: window shortcut inhibition unavailable: {error}"),
            }
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
        let inhibit = {
            let state = self.shared.state.lock().unwrap();
            super::shortcuts::wanted(
                state.connected,
                state.interactive,
                state.controlling,
                self.focused,
            )
        };
        if let Some(shortcuts) = &mut self.shortcuts {
            if let Err(error) = shortcuts.sync(inhibit) {
                eprintln!("UKVM: window shortcut inhibition failed: {error}");
                self.release();
                self.shortcuts = None;
            }
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
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused {
                    self.release();
                }
            }
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
    pixels: Arc<Vec<u32>>,
}
fn scale_image(image: &Image, window: (u32, u32)) -> Option<Scaled> {
    if u64::from(window.0) * u64::from(window.1) > 16_777_216 {
        return None;
    }
    let v = Viewport::fit(
        (window.0, window.1.saturating_sub(48)),
        (image.width, image.height),
    )?;
    let width = v.width as u32;
    let height = v.height as u32;
    if width == 0 || height == 0 {
        return None;
    }
    if width == image.width && height == image.height {
        return Some(Scaled {
            window,
            x: v.x as u32,
            y: v.y as u32 + 48,
            width,
            pixels: image.pixels.clone(),
        });
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
        pixels: Arc::new(pixels),
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
    let mut decoder =
        turbojpeg::Decompressor::new().map_err(|_| "TurboJPEG initialization failed")?;
    decoder
        .set_scan_limit(32)
        .map_err(|_| "JPEG scan limit failed")?;
    let header = decoder
        .read_header(&frame.jpeg)
        .map_err(|_| "invalid display JPEG header")?;
    if header.width != frame.width as usize || header.height != frame.height as usize {
        return Err("JPEG metadata differs from bounded display frame".into());
    }
    let mut pixels = vec![0u32; (frame.width * frame.height) as usize];
    let format = if cfg!(target_endian = "little") {
        turbojpeg::PixelFormat::BGRX
    } else {
        turbojpeg::PixelFormat::XRGB
    };
    decoder
        .decompress(
            &frame.jpeg,
            turbojpeg::Image {
                pixels: bytemuck::cast_slice_mut(&mut pixels),
                width: frame.width as usize,
                pitch: frame.width as usize * 4,
                height: frame.height as usize,
                format,
            },
        )
        .map_err(|_| "display JPEG decode failed")?;
    for pixel in &mut pixels {
        *pixel &= 0xffffff;
    }
    Ok(Image {
        timing: None,
        width: frame.width,
        height: frame.height,
        screen: frame.screen,
        origin_known: frame.origin_known,
        mapping_id: frame.mapping_id,
        pixels: Arc::new(pixels),
    })
}
fn decode_baseline(frame: Frame) -> Result<Image, String> {
    frame.validate()?;
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(frame.jpeg.as_slice()));
    decoder.set_max_decoding_buffer_size(16_777_216 * 4);
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
        timing: None,
        width: frame.width,
        height: frame.height,
        screen: frame.screen,
        origin_known: frame.origin_known,
        mapping_id: frame.mapping_id,
        pixels: Arc::new(pixels),
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
pub(super) fn profile_frame(frame: Frame, baseline: bool) -> (f64, f64) {
    let start = Instant::now();
    let image = if baseline {
        decode_baseline(frame)
    } else {
        decode(frame)
    }
    .unwrap();
    let decode = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    std::hint::black_box(scale_image(&image, (1280, 768)));
    (decode, start.elapsed().as_secs_f64() * 1000.)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_are_bounded_and_do_not_invent_transit_or_scanout() {
        let mut stats = Performance::default();
        for n in 0..1000 {
            stats.sample([n as f64, 1., 2., 3., 4., f64::NAN, f64::NAN, 0., 0.]);
        }
        assert_eq!(stats.samples.len(), 300);
        let report = stats.report(Duration::from_secs(5), [1, 1], [10, 20]);
        assert!(report.contains("transit_est_ms=unavailable"));
        assert!(report.contains("scanout=unmeasured"));
        assert!(report.contains("drops_decode=10 drops_render=20"));
    }
    #[test]
    fn native_scaling_shares_pixels_and_bounds_resize() {
        let image = Image {
            timing: None,
            width: 2,
            height: 1,
            screen: None,
            origin_known: false,
            mapping_id: None,
            pixels: Arc::new(vec![0xff0000, 0x00ff00]),
        };
        let scaled = scale_image(&image, (2, 49)).unwrap();
        assert!(Arc::ptr_eq(&scaled.pixels, &image.pixels));
        assert!(scale_image(&image, (u32::MAX, u32::MAX)).is_none());
    }
    #[test]
    fn scaling_interpolates_colors_and_keeps_native_pixels_exact() {
        let image = Image {
            timing: None,
            width: 2,
            height: 1,
            screen: None,
            origin_known: false,
            mapping_id: None,
            pixels: Arc::new(vec![0xff0000, 0x0000ff]),
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
        assert!(!clock.due(now + Duration::from_millis(15)));
        assert!(clock.due(now + Duration::from_millis(16)));
        // After a stalled/minimized window, skip missed frames instead of
        // issuing a burst of catch-up redraws.
        let later = now + Duration::from_secs(10);
        assert!(clock.due(later));
        assert!(!clock.due(later));
        assert_eq!(clock.next, later + Duration::from_millis(16));
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
            telemetry: Default::default(),
            width: 1,
            height: 1,
            screen: None,
            origin_known: false,
            mapping_id: None,
            jpeg: Arc::new(jpeg),
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
            shortcuts: None,
            focused: false,
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
