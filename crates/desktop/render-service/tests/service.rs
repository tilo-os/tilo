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
use tilo_render_protocol::{
    FontId, ImageId, LayerId, Limits, PROTOCOL_VERSION, SurfaceId, WireError,
};
use tilo_render_service::broker::{FromService, ToService};
use tilo_render_service::{Config, FontFiles, Service, serve};
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
        font_catalog: Some(tilo_font_catalog::checkout_root()),
        limits,
        confine: false,
    };
    std::thread::spawn(move || serve(&config, &service));
    let ready = broker.recv::<FromService>().unwrap().message;
    assert!(
        matches!(ready, FromService::Ready { gpu: false, .. }),
        "no render node, so frames come back in shared memory: {ready:?}"
    );
    broker
}

/// How the test app names its fonts to the service.
enum FontsBy {
    /// The test font, by its path in the shared view.
    Path,
    /// Catalog files by their pinned SHA-256, keyed by the blob holding the file.
    Digest(HashMap<u64, String>),
    /// Every font's bytes, in a sealed memfd.
    Upload,
}

/// What an app's toolkit keeps: its channel, its ring, its registrations.
struct App {
    channel: Channel,
    ring: RingWriter,
    /// The catalog files the service's welcome listed.
    catalog: Vec<String>,
    fonts_by: FontsBy,
    /// Each face registered, by the blob holding its file and its index there.
    fonts: HashMap<(u64, u32), FontId>,
    images: HashMap<u64, ImageId>,
    sequence: u64,
    size: (u32, u32),
}

impl ResourceIds for App {
    fn font(&mut self, font: &FontData) -> Result<FontId, WireError> {
        if let Some(id) = self.fonts.get(&(font.data.id(), font.index)) {
            return Ok(*id);
        }
        let id = FontId(self.fonts.len() as u32);
        match &self.fonts_by {
            FontsBy::Path => self.channel.send(
                &AppMessage::RegisterFontFile {
                    font: id,
                    path: font_path().display().to_string(),
                    index: font.index,
                },
                &[],
            ),
            FontsBy::Digest(digests) => self.channel.send(
                &AppMessage::RegisterCatalogFont {
                    font: id,
                    sha256: digests[&font.data.id()].clone(),
                    index: font.index,
                },
                &[],
            ),
            FontsBy::Upload => {
                let fd = sealed_blob("test-font", font.data.data()).unwrap();
                self.channel.send(
                    &AppMessage::RegisterFontData {
                        font: id,
                        index: font.index,
                    },
                    &[fd.as_fd()],
                )
            }
        }
        .unwrap();
        self.fonts.insert((font.data.id(), font.index), id);
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
                workspace: "private".to_owned(),
            },
            &[service.as_fd()],
        )
        .unwrap();
    drop(service);
    let ring = RingWriter::create(1 << 20).unwrap();
    app.send(
        &AppMessage::Hello {
            version: PROTOCOL_VERSION,
        },
        &[ring.fd()],
    )
    .unwrap();
    let welcome = app.recv::<ServiceMessage>().unwrap().message;
    let ServiceMessage::Welcome {
        version: PROTOCOL_VERSION,
        renderer,
        catalog,
        ..
    } = welcome
    else {
        panic!("the service welcomes the app: {welcome:?}");
    };
    assert!(
        !renderer.name.is_empty(),
        "the app learns what draws it: {renderer:?}"
    );
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
        catalog,
        fonts_by: FontsBy::Path,
        fonts: HashMap::new(),
        images: HashMap::new(),
        sequence: 0,
        size: (W, H),
    }
}

impl App {
    /// Send `scene` as the surface's content and return the sequence.
    fn frame(&mut self, scene: &Scene, damage: Option<Rect>) -> u64 {
        self.send_frame(scene, damage, false)
    }

    /// Send `scene`, which holds only what `damage` needs, and return the sequence.
    fn damage_only(&mut self, scene: &Scene, damage: Rect) -> u64 {
        self.send_frame(scene, Some(damage), true)
    }

    fn send_frame(&mut self, scene: &Scene, damage: Option<Rect>, damage_only: bool) -> u64 {
        self.sequence += 1;
        let mut record = Vec::new();
        let mut writer = FrameWriter::new(
            &mut record,
            (SurfaceId(1), self.sequence),
            self.size,
            [0.0, 0.0, 0.0, 1.0],
            damage.is_none(),
        );
        if damage_only {
            writer.damage_only();
        }
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
    frames_come_back_with_the_app_s_pixels(&start(Limits::default()));
}

/// A machine where wgpu offers no adapter at all still shows its apps: the
/// service rasterizes every frame on the CPU, without a device, and the app
/// gets the same pixels.
#[test]
fn with_no_gpu_adapter_frames_are_rasterized_on_the_cpu() {
    let (broker, service) = Channel::pair().unwrap();
    let fonts = FontFiles::new(
        &[tilo_font_catalog::checkout_root()],
        Some(&tilo_font_catalog::checkout_root()),
    );
    std::thread::spawn(move || Service::new(None, fonts, Limits::default()).run(&service));
    let ready = broker.recv::<FromService>().unwrap().message;
    assert!(
        matches!(&ready, FromService::Ready { gpu: false, device } if device == "CPU rasterizer"),
        "no device, so the CPU draws into shared memory: {ready:?}"
    );
    frames_come_back_with_the_app_s_pixels(&broker);
}

fn frames_come_back_with_the_app_s_pixels(broker: &Channel) {
    let mut app = connect(broker, 1, "so.tilo.calculator");

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

/// A frame that lands in a buffer the app already showed is brought up to
/// date with the regions that changed, each copied where the buffer shows
/// it: a region that starts right of the buffer's left edge keeps its
/// pixels, and a row's padding — zeros the readback leaves past the row —
/// never reaches the buffer, where an opaque surface would show it as a
/// black block.
#[test]
fn a_partial_frame_lands_where_the_buffer_shows_it_and_paints_no_black() {
    let broker = start(Limits::default());
    let mut app = connect(&broker, 1, "so.tilo.calculator");
    let red = Color::from_rgba8(255, 0, 0, 255);
    let blue = Color::from_rgba8(0, 0, 255, 255);
    let green = Color::from_rgba8(0, 255, 0, 255);
    let yellow = Color::from_rgba8(255, 255, 0, 255);
    let magenta = Color::from_rgba8(255, 0, 255, 255);
    let ground = KRect::new(0.0, 0.0, f64::from(W), f64::from(H));
    let squares = [
        (blue, KRect::new(8.0, 8.0, 16.0, 16.0)),
        (green, KRect::new(40.0, 30.0, 48.0, 38.0)),
        (yellow, KRect::new(56.0, 4.0, 60.0, 8.0)),
        (magenta, KRect::new(20.0, 40.0, 24.0, 44.0)),
    ];
    // Every frame draws the whole page; only the square it adds is damage.
    let page = |upto: usize| {
        let mut scene = filled(red, ground);
        for &(color, rect) in squares.iter().take(upto) {
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
        }
        scene
    };
    let damage = |(_, rect): (Color, KRect)| {
        Some(Rect {
            x0: rect.x0 as f32,
            y0: rect.y0 as f32,
            x1: rect.x1 as f32,
            y1: rect.y1 as f32,
        })
    };

    // Three frames fill the three buffers, each of them whole.
    let first = app.frame(&page(1), None);
    let Received { message, fds } = app.recv();
    let ServiceMessage::Buffers {
        generation, kind, ..
    } = message
    else {
        panic!("buffers first, got {message:?}");
    };
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
        buffer: shown,
        ..
    } = app.recv().message
    else {
        panic!("frame ready");
    };
    assert_eq!(sequence, first);
    let mut shown = vec![shown];
    for (index, &square) in squares
        .iter()
        .enumerate()
        .skip(1)
        .take(buffers.len().saturating_sub(1))
    {
        let sequence = app.frame(&page(index + 1), damage(square));
        let ServiceMessage::FrameReady {
            sequence: ready,
            buffer,
            ..
        } = app.recv().message
        else {
            panic!("frame ready");
        };
        assert_eq!(ready, sequence);
        shown.push(buffer);
    }

    // The buffers the app is done with go back, so the next frame lands in
    // one that already showed a frame: only what changed since is copied.
    for &buffer in &shown {
        app.channel
            .send(
                &AppMessage::Released {
                    surface: SurfaceId(1),
                    generation,
                    buffer,
                },
                &[],
            )
            .unwrap();
    }
    let sequence = app.frame(&page(squares.len()), damage(squares[3]));
    let ServiceMessage::FrameReady {
        sequence: ready,
        buffer,
        ..
    } = app.recv().message
    else {
        panic!("frame ready");
    };
    assert_eq!(ready, sequence);
    assert!(shown.contains(&buffer), "the released buffer comes back");
    let now = &buffers[buffer as usize];
    assert_eq!(pixel(now, 1, 1), [0, 0, 255, 255], "the ground");
    assert_eq!(pixel(now, 12, 12), [255, 0, 0, 255], "the blue square");
    assert_eq!(pixel(now, 41, 31), [0, 255, 0, 255], "the green square");
    assert_eq!(pixel(now, 57, 5), [0, 255, 255, 255], "the yellow square");
    assert_eq!(pixel(now, 21, 41), [255, 0, 255, 255], "the magenta square");
}

/// An app that leaves out of its scene what the pixels it presented last still show sends the
/// frame as damage only: the service draws it over its last frame where the damage says and
/// nowhere else, so the buffer is pixel for pixel the one a whole scene makes; and where it would
/// have to draw the frame whole (it holds no frame yet), it refuses it and draws nothing, and the
/// app sends it whole.
#[test]
fn a_frame_drawn_for_its_damage_alone_matches_the_whole_one_or_is_refused() {
    let broker = start(Limits::default());
    let red = Color::from_rgba8(255, 0, 0, 255);
    let blue = Color::from_rgba8(0, 0, 255, 255);
    let ground = KRect::new(0.0, 0.0, f64::from(W), f64::from(H));
    let square = KRect::new(8.0, 8.0, 24.0, 20.0);
    let damage = Rect {
        x0: square.x0 as f32,
        y0: square.y0 as f32,
        x1: square.x1 as f32,
        y1: square.y1 as f32,
    };
    let whole = {
        let mut scene = filled(red, ground);
        scene.fill(Fill::NonZero, Affine::IDENTITY, blue, None, &square);
        scene
    };
    // What the damage needs: the square, and the ground under it.
    let culled = {
        let mut scene = filled(red, square);
        scene.fill(Fill::NonZero, Affine::IDENTITY, blue, None, &square);
        scene
    };
    // The buffers a connection's first frame brings, and the one that frame is in.
    let first_buffers = |app: &App| -> (Vec<Mapping>, u32) {
        let Received { message, fds } = app.recv();
        let ServiceMessage::Buffers { .. } = message else {
            panic!("buffers first, got {message:?}");
        };
        let buffers = fds
            .iter()
            .map(|fd| {
                Mapping::map_sealed(fd, false, (W * H * 4) as usize, (W * H * 4) as usize).unwrap()
            })
            .collect();
        let ServiceMessage::FrameReady { buffer, .. } = app.recv().message else {
            panic!("frame ready");
        };
        (buffers, buffer)
    };
    let ready = |app: &App| -> u32 {
        let ServiceMessage::FrameReady { buffer, .. } = app.recv().message else {
            panic!("frame ready");
        };
        buffer
    };

    // The culled frame.
    let mut culling = connect(&broker, 1, "so.tilo.notes");
    let refused = culling.damage_only(&culled, damage);
    let ServiceMessage::FrameDropped { sequence, why } = culling.recv().message else {
        panic!("a frame drawn for its damage with no frame under it is refused");
    };
    assert_eq!(
        (sequence, why),
        (refused, Refusal::DamageOnly),
        "with no frame under it, a damage-only frame would be drawn whole: it is refused"
    );
    culling.frame(&filled(red, ground), None);
    let (culled_buffers, _) = first_buffers(&culling);
    culling.damage_only(&culled, damage);
    let culled_buffer = &culled_buffers[ready(&culling) as usize];

    // Its twin, drawn whole.
    let mut whole_app = connect(&broker, 2, "so.tilo.notes");
    whole_app.frame(&filled(red, ground), None);
    let (whole_buffers, _) = first_buffers(&whole_app);
    whole_app.frame(&whole, Some(damage));
    let whole_buffer = &whole_buffers[ready(&whole_app) as usize];

    assert_eq!(pixel(culled_buffer, 12, 12), [255, 0, 0, 255], "the square");
    assert_eq!(
        pixel(culled_buffer, 40, 40),
        [0, 0, 255, 255],
        "the ground it left out"
    );
    let differing = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(culled_buffer, x, y) != pixel(whole_buffer, x, y))
        .count();
    assert_eq!(
        differing, 0,
        "the culled frame shows what the whole one does"
    );
}

/// Every face of the catalog files a line of CJK text with emoji is set in, each file read once
/// as the toolkit maps it, with the digest its manifest pins.
fn cjk_and_emoji_faces() -> Vec<(String, FontData)> {
    let manifest = tilo_font_catalog::manifest().unwrap();
    let root = tilo_font_catalog::checkout_root();
    let mut faces = Vec::new();
    for file in ["NotoSansCJK-VF.ttc", "Noto-COLRv1.ttf"] {
        let asset = manifest
            .font
            .iter()
            .find(|asset| asset.file == file)
            .unwrap();
        let blob = Blob::new(Arc::new(
            std::fs::read(asset.path_in(&root).unwrap()).unwrap(),
        ));
        for face in &asset.faces {
            faces.push((
                asset.sha256.clone(),
                FontData::new(blob.clone(), face.index),
            ));
        }
    }
    faces
}

/// White glyphs of every face in `faces`, one row each, on black.
fn glyph_rows(faces: &[(String, FontData)]) -> Scene {
    let mut scene = filled(
        Color::BLACK,
        KRect::new(0.0, 0.0, f64::from(W), f64::from(H)),
    );
    for (row, (_, font)) in faces.iter().enumerate() {
        scene
            .draw_glyphs(font)
            .font_size(16.0)
            .transform(Affine::translate((0.0, 14.0 + (row % 3) as f64 * 14.0)))
            .brush(Color::WHITE)
            .draw(
                Fill::NonZero,
                (0..4).map(|i| vello::Glyph {
                    id: 1200 + i,
                    x: i as f32 * 16.0,
                    y: 0.0,
                }),
            );
    }
    scene
}

/// An app whose text needs the catalog's CJK collection and its colour emoji names their files
/// by digest: the service maps its own copy, so not one byte of them is the app's to send or to
/// count, and the frame is drawn. The limit on uploaded font bytes is zero here, so any byte
/// the app sent would be refused.
#[test]
fn catalog_faces_are_named_by_digest_and_drawn_without_their_bytes() {
    let broker = start(Limits {
        font_bytes: 0,
        ..Limits::default()
    });
    let mut app = connect(&broker, 1, "so.tilo.gallery");
    let faces = cjk_and_emoji_faces();
    assert!(faces.len() > 2, "the CJK collection has a face per region");
    for (digest, _) in &faces {
        assert!(
            app.catalog.contains(digest),
            "the welcome lists the catalog file {digest}"
        );
    }
    app.fonts_by = FontsBy::Digest(
        faces
            .iter()
            .map(|(digest, font)| (font.data.id(), digest.clone()))
            .collect(),
    );
    let sequence = app.frame(&glyph_rows(&faces), None);
    let Received { message, fds } = app.recv();
    let ServiceMessage::Buffers { .. } = message else {
        panic!("every face registered and the frame drawn, got {message:?}");
    };
    let ServiceMessage::FrameReady {
        sequence: ready,
        buffer,
        ..
    } = app.recv().message
    else {
        panic!("frame ready");
    };
    assert_eq!(ready, sequence);
    let shown = Mapping::map_sealed(
        &fds[buffer as usize],
        false,
        (W * H * 4) as usize,
        (W * H * 4) as usize,
    )
    .unwrap();
    let inked = (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(&shown, x, y)[..3] != [0, 0, 0])
        .count();
    assert!(inked > 20, "the glyphs drew ink: {inked} pixels");
}

/// The same faces sent as bytes, as an app named them before catalog files had a digest on the
/// wire: the 32 MB collection counted once per face drawn from it, past the limit on what an app
/// uploads. The service refuses them, and says why.
#[test]
fn catalog_faces_sent_as_bytes_are_over_the_upload_limit() {
    let broker = start(Limits::default());
    let mut app = connect(&broker, 1, "so.tilo.gallery");
    app.fonts_by = FontsBy::Upload;
    let faces = cjk_and_emoji_faces();
    app.frame(&glyph_rows(&faces), None);
    let refused = std::iter::from_fn(|| match app.recv().message {
        ServiceMessage::Refused { why } => Some(why),
        _ => None,
    })
    .collect::<Vec<_>>();
    assert!(
        refused.contains(&Refusal::OverLimit("font bytes".to_owned())),
        "uploads past the limit are refused: {refused:?}"
    );
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
fn an_app_over_its_gpu_budget_waits_out_the_overrun_and_its_frame_is_drawn() {
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
    let sequence = app.frame(&scene, None);
    // Held while the app is over its budget, then drawn: the picture on screen is
    // the app's newest one, not the frame before it.
    loop {
        match app.recv().message {
            ServiceMessage::Buffers { .. } => {}
            ServiceMessage::FrameReady {
                sequence: ready, ..
            } => {
                assert_eq!(ready, sequence);
                break;
            }
            ServiceMessage::FrameDropped { why, .. } => {
                panic!("the held frame is drawn rather than dropped: {why:?}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
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
