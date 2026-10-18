//! The service end to end, as an app and the broker see it, without a GPU:
//! frames go in through the ring, pixels come out in shared-memory buffers.
//! And what a hostile app can do: nothing beyond its own frames.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    reason = "end-to-end test: failures are the assertion"
)]

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;

use tilo_render_protocol::channel::{Channel, ChannelError, Received};
use tilo_render_protocol::control::{
    AlphaType, AppMessage, BufferKind, ImageFormat, Refusal, ServiceMessage,
};
use tilo_render_protocol::frame::{FrameWriter, LayerUpdate, Rect};
use tilo_render_protocol::scene::ResourceIds;
use tilo_render_protocol::shm::{Mapping, RingWriter, sealed_blob};
use tilo_render_protocol::{FontId, ImageId, LayerId, Limits, SurfaceId, WireError};
use tilo_render_service::broker::{FromService, ToService};
use tilo_render_service::{Config, serve};
use vello::Scene;
use vello::kurbo::{Affine, Rect as KRect};
use vello::peniko::{Blob, Color, Fill, FontData, ImageAlphaType, ImageData};

const W: u32 = 64;
const H: u32 = 48;

fn font_path() -> PathBuf {
    tilo_font_catalog::checkout_root().join("AdwaitaSans-Regular.ttf")
}

/// A service on a thread, without a GPU and without its sandbox (the test
/// process must live on), and the broker's end of its socket.
fn start(limits: Limits) -> Channel {
    let (broker, service) = Channel::pair().unwrap();
    let config = Config {
        render_node: None,
        font_roots: vec![tilo_font_catalog::checkout_root()],
        limits,
        confine: false,
    };
    std::thread::spawn(move || serve(&config, &service));
    let ready = broker.recv::<FromService>().unwrap().message;
    assert!(matches!(ready, FromService::Ready { gpu: false, .. }));
    broker
}

/// What an app's toolkit keeps: its channel, its ring, its registrations.
struct App {
    channel: Channel,
    ring: RingWriter,
    fonts: HashMap<u64, FontId>,
    images: HashMap<u64, ImageId>,
    sequence: u64,
    size: (u32, u32),
}

impl ResourceIds for App {
    fn font(&mut self, font: &FontData) -> Result<FontId, WireError> {
        if let Some(id) = self.fonts.get(&font.data.id()) {
            return Ok(*id);
        }
        let id = FontId(self.fonts.len() as u32);
        self.channel
            .send(
                &AppMessage::RegisterFontFile {
                    font: id,
                    path: font_path().display().to_string(),
                    index: font.index,
                },
                &[],
            )
            .unwrap();
        self.fonts.insert(font.data.id(), id);
        Ok(id)
    }

    fn image(&mut self, image: &ImageData) -> Result<ImageId, WireError> {
        if let Some(id) = self.images.get(&image.data.id()) {
            return Ok(*id);
        }
        let id = ImageId(self.images.len() as u32);
        let fd = sealed_blob("test-image", image.data.data()).unwrap();
        self.channel
            .send(
                &AppMessage::RegisterImage {
                    image: id,
                    width: image.width,
                    height: image.height,
                    format: ImageFormat::Rgba8,
                    alpha: AlphaType::Straight,
                },
                &[fd.as_fd()],
            )
            .unwrap();
        self.images.insert(image.data.id(), id);
        Ok(id)
    }
}

fn connect(broker: &Channel, connection: u64, app_id: &str) -> App {
    let (app, service) = Channel::pair().unwrap();
    broker
        .send(
            &ToService::Connect {
                connection,
                app_id: app_id.to_owned(),
                room: "private".to_owned(),
            },
            &[service.as_fd()],
        )
        .unwrap();
    drop(service);
    let ring = RingWriter::create(1 << 20).unwrap();
    app.send(&AppMessage::Hello { version: 1 }, &[ring.fd()])
        .unwrap();
    let welcome = app.recv::<ServiceMessage>().unwrap().message;
    assert!(matches!(
        welcome,
        ServiceMessage::Welcome { version: 1, .. }
    ));
    app.send(
        &AppMessage::CreateSurface {
            surface: SurfaceId(1),
            opaque: true,
            formats: Vec::new(),
        },
        &[],
    )
    .unwrap();
    App {
        channel: app,
        ring,
        fonts: HashMap::new(),
        images: HashMap::new(),
        sequence: 0,
        size: (W, H),
    }
}

impl App {
    /// Send `scene` as the surface's content and return the sequence.
    fn frame(&mut self, scene: &Scene, damage: Option<Rect>) -> u64 {
        self.sequence += 1;
        let mut record = Vec::new();
        let mut writer = FrameWriter::new(
            &mut record,
            (SurfaceId(1), self.sequence),
            self.size,
            [0.0, 0.0, 0.0, 1.0],
            damage.is_none(),
        );
        if let Some(rect) = damage {
            writer.damage(&[rect]).unwrap();
        }
        // Registrations go out on the channel before the frame names them.
        let content = scene.encoding().clone();
        writer
            .layer(&LayerUpdate::content(LayerId::ROOT, content), self)
            .unwrap();
        writer.finish().unwrap();
        let offset = self.ring.write(self.sequence, &record).unwrap();
        self.channel
            .send(
                &AppMessage::Frame {
                    sequence: self.sequence,
                    offset: offset as u64,
                    len: record.len() as u32,
                },
                &[],
            )
            .unwrap();
        self.sequence
    }

    fn recv(&self) -> Received<ServiceMessage> {
        self.channel.recv::<ServiceMessage>().unwrap()
    }
}

/// BGRA pixel at (x, y) of a shared-memory buffer.
fn pixel(buffer: &Mapping, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * W + x) * 4) as usize;
    buffer.as_ref()[at..at + 4].try_into().unwrap()
}

fn filled(color: Color, rect: KRect) -> Scene {
    let mut scene = Scene::new();
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    scene
}

#[test]
fn frames_come_back_as_buffers_with_the_app_s_pixels() {
    let broker = start(Limits::default());
    let mut app = connect(&broker, 1, "so.tilo.calculator");

    // A full frame: red everywhere, a picture and some text on it.
    let mut scene = filled(
        Color::from_rgba8(255, 0, 0, 255),
        KRect::new(0.0, 0.0, f64::from(W), f64::from(H)),
    );
    let picture = ImageData {
        data: Blob::new(Arc::new([0_u8, 255, 0, 255].repeat(16))),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 4,
        height: 4,
    };
    scene.draw_image(&picture, Affine::translate((40.0, 30.0)));
    let font = FontData::new(Blob::new(Arc::new(std::fs::read(font_path()).unwrap())), 0);
    scene
        .draw_glyphs(&font)
        .font_size(12.0)
        .transform(Affine::translate((2.0, 40.0)))
        .brush(Color::WHITE)
        .draw(
            Fill::NonZero,
            (0..3).map(|i| vello::Glyph {
                id: 40 + i,
                x: i as f32 * 8.0,
                y: 0.0,
            }),
        );
    let first = app.frame(&scene, None);

    let Received { message, fds } = app.recv();
    let ServiceMessage::Buffers {
        generation,
        width,
        height,
        kind,
        planes,
        ..
    } = message
    else {
        panic!("buffers first, got {message:?}");
    };
    assert_eq!((width, height, fds.len(), planes.len()), (W, H, 3, 3));
    assert!(
        matches!(kind, BufferKind::Shm { format: 1 }),
        "an opaque surface gets XRGB8888"
    );
    let buffers: Vec<Mapping> = fds
        .iter()
        .map(|fd| {
            Mapping::map_sealed(fd, false, (W * H * 4) as usize, (W * H * 4) as usize).unwrap()
        })
        .collect();
    let ServiceMessage::FrameReady {
        sequence,
        buffer,
        generation: g,
        damage,
        ..
    } = app.recv().message
    else {
        panic!("frame ready");
    };
    assert_eq!((sequence, g), (first, generation));
    assert_eq!(
        damage,
        vec![[0, 0, W.cast_signed(), H.cast_signed()]],
        "a full frame damages everything"
    );
    let shown = &buffers[buffer as usize];
    assert_eq!(
        pixel(shown, 1, 1),
        [0, 0, 255, 255],
        "red, in wl_shm byte order"
    );
    assert_eq!(
        pixel(shown, 41, 31),
        [0, 255, 0, 255],
        "the uploaded picture"
    );

    // A partial frame: only a blue square changed. The app still holds the
    // first buffer, so the frame lands in another, brought up to date with
    // both frames' damage.
    let mut scene = filled(
        Color::from_rgba8(255, 0, 0, 255),
        KRect::new(0.0, 0.0, f64::from(W), f64::from(H)),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(0, 0, 255, 255),
        None,
        &KRect::new(8.0, 8.0, 16.0, 16.0),
    );
    let second = app.frame(
        &scene,
        Some(Rect {
            x0: 8.0,
            y0: 8.0,
            x1: 16.0,
            y1: 16.0,
        }),
    );
    let ServiceMessage::FrameReady {
        sequence,
        buffer: next,
        damage,
        ..
    } = app.recv().message
    else {
        panic!("frame ready");
    };
    assert_eq!(sequence, second);
    assert_ne!(next, buffer, "the attached buffer is not drawn into");
    assert_eq!(
        damage,
        vec![[8, 8, 8, 8]],
        "the surface damage is the frame's"
    );
    let shown = &buffers[next as usize];
    assert_eq!(pixel(shown, 10, 10), [255, 0, 0, 255], "the blue square");
    assert_eq!(
        pixel(shown, 1, 1),
        [0, 0, 255, 255],
        "the rest came from the first frame"
    );
    assert_eq!(pixel(shown, 41, 31), [0, 255, 0, 255]);
}

#[test]
fn a_frame_that_breaks_the_protocol_is_dropped_and_the_service_carries_on() {
    let broker = start(Limits::default());
    let mut app = connect(&broker, 1, "so.tilo.hostile");
    // A record outside the ring.
    app.channel
        .send(
            &AppMessage::Frame {
                sequence: 7,
                offset: 1 << 40,
                len: 64,
            },
            &[],
        )
        .unwrap();
    let ServiceMessage::FrameDropped {
        sequence: 7,
        why: Refusal::Invalid(_),
    } = app.recv().message
    else {
        panic!("dropped");
    };
    // Garbage where a record should be.
    let offset = app.ring.write(8, &[0xEE; 256]).unwrap();
    app.channel
        .send(
            &AppMessage::Frame {
                sequence: 8,
                offset: offset as u64,
                len: 256,
            },
            &[],
        )
        .unwrap();
    assert!(matches!(
        app.recv().message,
        ServiceMessage::FrameDropped { sequence: 8, .. }
    ));
    // A font outside the shared font directories.
    app.channel
        .send(
            &AppMessage::RegisterFontFile {
                font: FontId(9),
                path: "/etc/passwd".to_owned(),
                index: 0,
            },
            &[],
        )
        .unwrap();
    assert!(matches!(app.recv().message, ServiceMessage::Refused { .. }));
    // An image memfd that is not sealed: the app could still change it.
    let fd: OwnedFd =
        rustix::fs::memfd_create("unsealed", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    rustix::fs::ftruncate(&fd, 64).unwrap();
    app.channel
        .send(
            &AppMessage::RegisterImage {
                image: ImageId(3),
                width: 4,
                height: 4,
                format: ImageFormat::Rgba8,
                alpha: AlphaType::Straight,
            },
            &[fd.as_fd()],
        )
        .unwrap();
    assert!(matches!(app.recv().message, ServiceMessage::Refused { .. }));
    // The app is still served.
    let sequence = app.frame(&filled(Color::WHITE, KRect::new(0.0, 0.0, 4.0, 4.0)), None);
    let _buffers = app.recv();
    let ServiceMessage::FrameReady {
        sequence: ready, ..
    } = app.recv().message
    else {
        panic!("served after the refusals");
    };
    assert_eq!(ready, sequence);
}

#[test]
fn an_app_cannot_draw_another_app_s_image() {
    let broker = start(Limits::default());
    let mut owner = connect(&broker, 1, "so.tilo.photos");
    let picture = ImageData {
        data: Blob::new(Arc::new(vec![9_u8; 16 * 4])),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 4,
        height: 4,
    };
    let mut scene = Scene::new();
    scene.draw_image(&picture, Affine::IDENTITY);
    owner.frame(&scene, None);
    let _ = owner.recv();
    assert!(matches!(
        owner.recv().message,
        ServiceMessage::FrameReady { .. }
    ));

    // Another app names the same image id without having registered it.
    let mut thief = connect(&broker, 2, "so.tilo.thief");
    thief.images.insert(picture.data.id(), ImageId(0));
    thief.frame(&scene, None);
    let ServiceMessage::FrameDropped {
        why: Refusal::Invalid(why),
        ..
    } = thief.recv().message
    else {
        panic!("dropped");
    };
    assert!(why.contains("image"), "{why}");
}

#[test]
fn an_app_over_its_gpu_budget_waits_out_the_overrun() {
    let broker = start(Limits {
        gpu_time_us: 0,
        ..Limits::default()
    });
    let mut app = connect(&broker, 1, "so.tilo.heavy");
    // A large surface: drawing it takes milliseconds, all of it over a zero
    // budget, so the app's next frame comes well inside the overrun.
    app.size = (2048, 1536);
    let scene = filled(Color::WHITE, KRect::new(0.0, 0.0, 2048.0, 1536.0));
    app.frame(&scene, None);
    let _ = app.recv();
    let ServiceMessage::FrameReady { gpu_us, .. } = app.recv().message else {
        panic!("the first frame is drawn");
    };
    assert!(gpu_us > 0, "the frame's time is measured");
    app.frame(&scene, None);
    assert!(matches!(
        app.recv().message,
        ServiceMessage::FrameDropped {
            why: Refusal::Throttled,
            ..
        }
    ));
}

#[test]
fn a_closed_broker_ends_the_service_and_a_closed_app_only_itself() {
    let broker = start(Limits::default());
    let app = connect(&broker, 1, "so.tilo.leaving");
    drop(app);
    let mut other = connect(&broker, 2, "so.tilo.staying");
    other.frame(&filled(Color::WHITE, KRect::new(0.0, 0.0, 2.0, 2.0)), None);
    let _ = other.recv();
    assert!(matches!(
        other.recv().message,
        ServiceMessage::FrameReady { .. }
    ));
    drop(broker);
    // The service exits: its end of the app's channel closes.
    assert!(matches!(
        other.channel.recv::<ServiceMessage>(),
        Err(ChannelError::Closed)
    ));
}
