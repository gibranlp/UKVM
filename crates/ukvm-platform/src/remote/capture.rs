//! Consent-scoped ScreenCast/PipeWire. No compositor IPC or capture subprocesses.
use crate::linux_wayland::authorization::{Authorization, Context, Kind};
use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType, Streams},
    PersistMode, Session,
};
use pipewire::{self as pw, properties::properties, spa};
use std::{
    os::fd::OwnedFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use ukvm_core::remote::Latest;
use ukvm_protocol::display::{Frame, ScreenRect};
pub struct Capture {
    pub frames: Arc<Latest<Frame>>,
    pub error: Arc<std::sync::Mutex<Option<String>>>,
    pub session: Session<Screencast>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    _authorization: Authorization,
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
impl Capture {
    pub async fn open(connection: zbus::Connection) -> Result<Self, String> {
        let portal = Screencast::with_connection(connection.clone())
            .await
            .map_err(|e| e.to_string())?;
        let mut authorization = Authorization::begin(
            Kind::ScreenCast,
            Context::on_bus(&connection, portal.version()).await?,
        )?;
        let session = portal
            .create_session(Default::default())
            .await
            .map_err(|_| "ScreenCast CreateSession failed")?;
        let response = authorize_monitor(&portal, &session, &mut authorization).await?;
        let stream = &response.streams()[0];
        let mapping_id = stream.mapping_id().map(str::to_owned);
        let origin_known = stream.position().is_some();
        let screen = stream
            .size()
            .or_else(|| mapping_id.as_ref().map(|_| (1, 1)))
            .and_then(|(w, h)| {
                let (x, y) = stream.position().unwrap_or((0, 0));
                let rect = ScreenRect {
                    x,
                    y,
                    width: u32::try_from(w).ok()?,
                    height: u32::try_from(h).ok()?,
                };
                rect.validate().ok()?;
                Some(rect)
            });
        let node = stream.pipe_wire_node_id();
        let fd = portal
            .open_pipe_wire_remote(&session, Default::default())
            .await
            .map_err(|e| e.to_string())?;
        let raw = Arc::new(Latest::<Raw>::default());
        let frames = Arc::new(Latest::default());
        let stop = Arc::new(AtomicBool::new(false));
        let error = Arc::new(std::sync::Mutex::new(None));
        let worker_raw = raw.clone();
        let worker_stop = stop.clone();
        let worker_error = error.clone();
        let capture = std::thread::Builder::new()
            .name("ukvm-pipewire".into())
            .spawn(move || {
                if let Err(e) = pipewire(fd, node, worker_raw, worker_stop) {
                    *worker_error.lock().unwrap() = Some(e);
                }
            })
            .map_err(|e| e.to_string())?;
        let worker_frames = frames.clone();
        let worker_stop = stop.clone();
        let worker_error = error.clone();
        let encoder = std::thread::Builder::new()
            .name("ukvm-display-encode".into())
            .spawn(move || {
                let mut sequence = 0;
                while !worker_stop.load(Ordering::Acquire) {
                    let started = Instant::now();
                    if let Some(raw) = raw.take() {
                        match encode(raw, screen, origin_known, mapping_id.clone(), sequence) {
                            Ok(frame) => {
                                worker_frames.replace(frame);
                                sequence += 1;
                            }
                            Err(e) => {
                                *worker_error.lock().unwrap() = Some(e);
                                break;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(33).saturating_sub(started.elapsed()));
                }
            });
        match encoder {
            Ok(encoder) => Ok(Self {
                frames,
                error,
                session,
                stop,
                threads: vec![capture, encoder],
                _authorization: authorization,
            }),
            Err(e) => {
                stop.store(true, Ordering::Release);
                let _ = capture.join();
                Err(e.to_string())
            }
        }
    }
}
// Credential-bearing requests use fixed errors: portal responses may include
// private restore state and must never be formatted into logs or status.
async fn authorize_monitor(
    portal: &Screencast,
    session: &Session<Screencast>,
    authorization: &mut Authorization,
) -> Result<Streams, String> {
    let restoring = portal.version() >= 4 && authorization.restore_token().is_some();
    let mut options = SelectSourcesOptions::default()
        .set_sources(Some(SourceType::Monitor.into()))
        .set_multiple(false)
        .set_cursor_mode(CursorMode::Embedded);
    if portal.version() >= 4 {
        options = options
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(authorization.restore_token());
    }
    portal.select_sources(session, options).await
        .and_then(|request| request.response())
        .map_err(|_| "ScreenCast SelectSources denied or failed; retry normal portal approval (any restore token was consumed)")?;
    let response = portal.start(session, None, Default::default()).await
        .and_then(|request| request.response())
        .map_err(|_| "ScreenCast Start denied or failed; retry normal portal approval (any restore token was consumed)")?;
    if response.streams().len() != 1 {
        return Err("ScreenCast must supply exactly one monitor".into());
    }
    if response.streams()[0]
        .source_type()
        .is_some_and(|kind| kind != SourceType::Monitor)
    {
        return Err("ScreenCast did not grant the requested monitor".into());
    }
    authorization.granted(if portal.version() >= 4 {
        response.restore_token()
    } else {
        None
    })?;
    eprintln!(
        "[wayland] ScreenCast authorized; restore_attempted={restoring} remembered={} (restoration remains portal-controlled)",
        portal.version() >= 4 && response.restore_token().is_some()
    );
    Ok(response)
}

struct Raw {
    width: u32,
    height: u32,
    stride: usize,
    bgr: bool,
    pixels: Vec<u8>,
}
fn encode(
    raw: Raw,
    screen: Option<ScreenRect>,
    origin_known: bool,
    mapping_id: Option<String>,
    sequence: u64,
) -> Result<Frame, String> {
    let scale = (1920. / f64::from(raw.width))
        .min(1080. / f64::from(raw.height))
        .min(1.);
    let width = (f64::from(raw.width) * scale).floor().max(1.) as u32;
    let height = (f64::from(raw.height) * scale).floor().max(1.) as u32;
    let mut rgb = vec![0; (width * height * 3) as usize];
    // Average every source pixel represented by a destination pixel. Point
    // sampling used to discard thin glyph strokes on high-DPI/4K desktops.
    for y in 0..height {
        let top = y * raw.height / height;
        let bottom = (y + 1) * raw.height / height;
        for x in 0..width {
            let left = x * raw.width / width;
            let right = (x + 1) * raw.width / width;
            let mut sum = [0u32; 3];
            for sy in top..bottom {
                for sx in left..right {
                    let source = sy as usize * raw.stride + sx as usize * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += u32::from(raw.pixels[source + channel]);
                    }
                }
            }
            let count = (bottom - top) * (right - left);
            let dest = ((y * width + x) * 3) as usize;
            for channel in 0..3 {
                let source_channel = if raw.bgr { 2 - channel } else { channel };
                rgb[dest + channel] = ((sum[source_channel] + count / 2) / count) as u8;
            }
        }
    }
    let jpeg = encode_jpeg(&rgb, width, height)?;
    let frame = Frame {
        sequence,
        width,
        height,
        screen,
        origin_known,
        mapping_id,
        jpeg,
    };
    frame.validate()?;
    Ok(frame)
}
fn encode_jpeg(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    // Full chroma resolution matters for small colored text. Only lower JPEG
    // quantization quality when a noisy image would exceed the wire bound.
    for quality in [92, 80, 65, 45] {
        let mut jpeg = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut jpeg, quality);
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
        encoder
            .encode(
                rgb,
                width as u16,
                height as u16,
                jpeg_encoder::ColorType::Rgb,
            )
            .map_err(|_| "display JPEG encoding failed")?;
        if jpeg.len() <= ukvm_protocol::display::MAX_JPEG {
            return Ok(jpeg);
        }
    }
    Err("display JPEG exceeds frame bound after quality reduction".into())
}

struct Data {
    format: spa::param::video::VideoInfoRaw,
    last: Instant,
    initial_size: Option<(u32, u32)>,
    raw: Arc<Latest<Raw>>,
    error: Arc<std::sync::Mutex<Option<String>>>,
}
fn pipewire(
    fd: OwnedFd,
    node: u32,
    raw: Arc<Latest<Raw>>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    // The portal FD restricts access to the user-selected monitor.
    let core = context.connect_fd_rc(fd, None).map_err(|e| e.to_string())?;
    let stream=pw::stream::StreamBox::new(&core,"UKVM screen sharing",properties! {
        *pw::keys::MEDIA_TYPE=>"Video", *pw::keys::MEDIA_CATEGORY=>"Capture", *pw::keys::MEDIA_ROLE=>"Screen"
    }).map_err(|e|e.to_string())?;
    let failure = Arc::new(std::sync::Mutex::new(None));
    let failed = failure.clone();
    let _listener = stream
        .add_local_listener_with_user_data(Data {
            format: Default::default(),
            last: Instant::now() - Duration::from_secs(1),
            initial_size: None,
            raw,
            error: failure.clone(),
        })
        .state_changed(move |_, _, _, state| {
            if let pw::stream::StreamState::Error(e) = state {
                *failed.lock().unwrap() = Some(e.to_string());
            }
        })
        .param_changed(|_, data, id, param| {
            if id == spa::param::ParamType::Format.as_raw() {
                if let Some(param) = param {
                    if data.format.parse(param).is_err() {
                        *data.error.lock().unwrap() =
                            Some("unsupported PipeWire video format".into());
                    } else {
                        let size = data.format.size();
                        let size = (size.width, size.height);
                        if data.initial_size.is_some_and(|previous| previous != size) {
                            *data.error.lock().unwrap() =
                                Some("capture geometry changed; reopen the remote session".into());
                        }
                        data.initial_size = Some(size);
                    }
                }
            }
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if data.last.elapsed() < Duration::from_millis(33) {
                return;
            }
            let size = data.format.size();
            let format = data.format.format();
            if size.width == 0 || size.height == 0 {
                return;
            }
            if size.width > 8192
                || size.height > 8192
                || u64::from(size.width) * u64::from(size.height) > 16_777_216
            {
                *data.error.lock().unwrap() =
                    Some("capture source exceeds 64 MiB pixel bound".into());
                return;
            }
            if ![
                spa::param::video::VideoFormat::BGRx,
                spa::param::video::VideoFormat::BGRA,
                spa::param::video::VideoFormat::RGBx,
                spa::param::video::VideoFormat::RGBA,
            ]
            .contains(&format)
            {
                *data.error.lock().unwrap() =
                    Some("capture requires packed RGB or BGR memory buffers".into());
                return;
            }
            let Some(plane) = buffer.datas_mut().first_mut() else {
                return;
            };
            let chunk = plane.chunk();
            let offset = chunk.offset() as usize;
            let len = chunk.size() as usize;
            let Ok(stride) = usize::try_from(chunk.stride()) else {
                *data.error.lock().unwrap() = Some("negative video stride unsupported".into());
                return;
            };
            let row = size.width as usize * 4;
            let required = stride
                .checked_mul(size.height as usize - 1)
                .and_then(|n| n.checked_add(row));
            let Some(required) =
                required.filter(|n| *n <= 64 * 1024 * 1024 && stride >= row && *n <= len)
            else {
                return;
            };
            let Some(bytes) = plane.data() else {
                *data.error.lock().unwrap() = Some("PipeWire did not provide mapped memory".into());
                return;
            };
            let Some(end) = offset
                .checked_add(required)
                .filter(|end| *end <= bytes.len())
            else {
                return;
            };
            data.raw.replace(Raw {
                width: size.width,
                height: size.height,
                stride,
                bgr: matches!(
                    format,
                    spa::param::video::VideoFormat::BGRx | spa::param::video::VideoFormat::BGRA
                ),
                pixels: bytes[offset..end].to_vec(),
            });
            data.last = Instant::now();
        })
        .register()
        .map_err(|e| e.to_string())?;
    let values = video_format()?;
    let pod = spa::pod::Pod::from_bytes(&values).ok_or("video format invalid")?;
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )
        .map_err(|e| e.to_string())?;
    while !stop.load(Ordering::Acquire) {
        mainloop.loop_().iterate(Duration::from_millis(20));
        if let Some(e) = failure.lock().unwrap().take() {
            return Err(e);
        }
    }
    Ok(())
}

// Include the required size range, as in PipeWire's video capture tutorial.
// The source selects its native size; the encoder handles downscaling.
fn video_format() -> Result<Vec<u8>, String> {
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::RGBA
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            // Some portal sources advertise the monitor's fixed refresh rate.
            // Accept it and throttle buffer copies/encoding locally to ~30 FPS.
            spa::utils::Fraction {
                num: 1000,
                denom: 1
            }
        )
    );
    Ok(spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|_| "video format encoding failed")?
    .0
    .into_inner())
}

pub type PortalConnection = zbus::Connection;
pub async fn portal_connection() -> Result<PortalConnection, String> {
    zbus::Connection::session().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_color_resolution_preserves_small_colored_text_edges() {
        let pixels: Vec<u8> = (0..64 * 16)
            .flat_map(|x| {
                if x % 2 == 0 {
                    [255, 0, 0, 0]
                } else {
                    [0, 0, 255, 0]
                }
            })
            .collect();
        let frame = encode(
            Raw {
                width: 64,
                height: 16,
                stride: 64 * 4,
                bgr: false,
                pixels: pixels.clone(),
            },
            None,
            false,
            None,
            0,
        )
        .unwrap();
        let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(frame.jpeg));
        let rgb = decoder.decode().unwrap();
        let expected: Vec<u8> = pixels
            .chunks_exact(4)
            .flat_map(|p| p[..3].iter().copied())
            .collect();
        let error: u64 = rgb
            .iter()
            .zip(&expected)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        assert!(
            error / (rgb.len() as u64) < 12,
            "fine color edges must remain distinct"
        );
    }
    #[test]
    fn downscaling_preserves_thin_details_instead_of_discarding_them() {
        let pixels = [0, 0, 0, 0, 255, 255, 255, 0].repeat(1920 * 2);
        let frame = encode(
            Raw {
                width: 3840,
                height: 2,
                stride: 3840 * 4,
                bgr: false,
                pixels,
            },
            None,
            false,
            None,
            0,
        )
        .unwrap();
        let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(frame.jpeg));
        let rgb = decoder.decode().unwrap();
        assert!(rgb.iter().all(|value| value.abs_diff(128) < 4));
    }
    #[test]
    fn noisy_full_hd_frames_keep_the_wire_bound_and_dimensions() {
        let mut state = 0x123456789abcdefu64;
        let rgb: Vec<u8> = (0..1920 * 1080 * 3)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let jpeg = encode_jpeg(&rgb, 1920, 1080).unwrap();
        assert!(jpeg.len() <= ukvm_protocol::display::MAX_JPEG);
        let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
        decoder.read_info().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (1920, 1080));
    }
    #[test]
    fn capture_offer_accepts_native_resolution_and_high_refresh_sources() {
        use spa::{
            param::format::FormatProperties,
            pod::{deserialize::PodDeserializer, ChoiceValue, Value},
            utils::ChoiceEnum,
        };
        let bytes = video_format().unwrap();
        let (_, Value::Object(object)) = PodDeserializer::deserialize_any_from(&bytes).unwrap()
        else {
            panic!("format object expected")
        };
        let property = |key: FormatProperties| {
            &object
                .properties
                .iter()
                .find(|p| p.key == key.as_raw())
                .unwrap()
                .value
        };
        let Value::Choice(ChoiceValue::Rectangle(size)) = property(FormatProperties::VideoSize)
        else {
            panic!("size range required for negotiation")
        };
        let ChoiceEnum::Range { min, max, .. } = &size.1 else {
            panic!("size range expected")
        };
        assert_eq!((min.width, min.height), (1, 1));
        assert!(max.width >= 3840 && max.height >= 2160);
        let Value::Choice(ChoiceValue::Fraction(rate)) = property(FormatProperties::VideoFramerate)
        else {
            panic!("refresh range expected")
        };
        let ChoiceEnum::Range { min, max, .. } = &rate.1 else {
            panic!("refresh range expected")
        };
        // Zero is used for variable/damage-only streams; 60/144 Hz are common
        // fixed source rates and must not be excluded by the encoding target.
        assert_eq!(min.num, 0);
        assert!(max.num / max.denom >= 144);
    }
    #[test]
    fn encoding_preserves_aspect_ratio_and_bgr_channels() {
        let raw = Raw {
            width: 3840,
            height: 2,
            stride: 3840 * 4,
            bgr: true,
            pixels: [0, 0, 255, 0].repeat(3840 * 2),
        };
        let frame = encode(raw, None, false, None, 1).unwrap();
        assert_eq!((frame.width, frame.height), (1920, 1));
        let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(frame.jpeg));
        let rgb = decoder.decode().unwrap();
        assert!(rgb[0] > 240 && rgb[1] < 10 && rgb[2] < 10);
    }
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod persistence_tests;
