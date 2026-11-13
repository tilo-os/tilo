//! A menu or a card the bar opens is one frame, exactly its own: outside its panel only its shadow
//! paints, and a shadow only darkens what it falls on. A surface sized past its panel that paints a
//! second, lighter frame there (at the shadow's reach, say), or a panel drawn on a square ground
//! that shows at its rounded corners, fails here, in both themes.
//!
//! It holds for each way a frame reaches the screen: with the material's blurred backdrop, and
//! without it, as a page draws (it runs no blur pass) and as any renderer whose blur pass failed
//! leaves it, on the GPU and on the CPU a page draws with on a software adapter, at the fractional
//! densities of a zoomed page or a scaled display.

use std::time::{Duration, Instant};

use tilo_desktop::profile_card::{ProfileCard, ProfileModel};
use tilo_desktop::workspace_menu::{WorkspaceAction, WorkspaceMenu, WorkspaceMenuModel};
use ui::headless::{FrameHarness, Framed};
use ui::state::State;
use ui::theme::Theme;
use vello::kurbo::{Affine, Point, Rect};

const SIZE: (u32, u32) = (1280, 720);
/// Where the bar's workspace name and the person's avatar stand.
const NAME: Rect = Rect::new(52.0, 4.0, 136.0, 28.0);
const AVATAR: Rect = Rect::new(1197.0, 0.0, 1229.0, 32.0);
/// How much lighter than the bare ground a pixel may read and still be the ground: rounding.
const TOLERANCE: i32 = 2;

fn viewport() -> Rect {
    Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1))
}

/// Draw frames until what opened rests, so the picture is the surface a person sees, not one caught
/// on its way in.
fn settle(harness: &mut FrameHarness) {
    harness.relayout();
    let deadline = Instant::now() + Duration::from_secs(3);
    while harness.poll_frame().expect("a frame draws") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(16));
    }
}

/// The pixel at `(column, row)`'s luminance, in 0..=255, on a frame `width` pixels wide.
fn luma(rgba: &[u8], width: u32, (column, row): (u32, u32)) -> i32 {
    let at = ((row * width + column) * 4) as usize;
    let [red, green, blue] = [rgba[at], rgba[at + 1], rgba[at + 2]].map(i32::from);
    (red * 2126 + green * 7152 + blue * 722) / 10_000
}

/// The pixel `at` lies in.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a point on the frame is a small non-negative pixel position"
)]
fn pixel(at: Point) -> (u32, u32) {
    (at.x.floor() as u32, at.y.floor() as u32)
}

/// `open` is `bare` with one surface whose panel is `panel`, both drawn `scale` device pixels per
/// logical pixel: every pixel outside the panel is the bare ground or the shadow on it, darker, never
/// lighter, and nothing changed past the shadow's reach. The panel's corner pixels, outside its
/// rounded outline, are ground too.
fn assert_one_frame(
    what: &str,
    (bare, open): (&[u8], &[u8]),
    panel: Rect,
    scale: f64,
    theme: &Theme,
) {
    assert_eq!(bare.len(), open.len(), "{what}: both frames are drawn");
    assert!(panel.area() > 0.0, "{what}: the surface is open: {panel:?}");
    let to_device = |rect: Rect| Affine::scale(scale).transform_rect_bbox(rect);
    let reach =
        to_device(ui::widget::floating_surface::painted_bounds(panel, theme)).inflate(1.0, 1.0);
    let (width, height) = device_size(scale);
    // The pixels the panel's own edge covers in part, at a density that does not land it on whole
    // pixels, are the panel's.
    let device_panel = to_device(panel);
    let inside = device_panel.inflate(1.0, 1.0);
    let mut changed = Rect::ZERO;
    for y in 0..height {
        for x in 0..width {
            let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            if inside.contains(Point::new(px, py)) {
                continue;
            }
            let (was, is) = (luma(bare, width, (x, y)), luma(open, width, (x, y)));
            assert!(
                is <= was + TOLERANCE,
                "{what}: at {x},{y}, outside the panel {device_panel:?}, the open surface reads \
                 lighter ({is}) than the ground under it ({was}): something past the panel paints \
                 a frame of its own"
            );
            if is != was {
                changed = if changed == Rect::ZERO {
                    Rect::new(px, py, px, py)
                } else {
                    changed.union_pt(Point::new(px, py))
                };
            }
        }
    }
    assert!(
        changed == Rect::ZERO || reach.contains_rect(changed),
        "{what}: the surface changed {changed:?}, past its shadow's reach {reach:?}"
    );
    for at in [
        Point::new(device_panel.x0, device_panel.y0),
        Point::new(device_panel.x1 - 1.0, device_panel.y0),
        Point::new(device_panel.x0, device_panel.y1 - 1.0),
        Point::new(device_panel.x1 - 1.0, device_panel.y1 - 1.0),
    ] {
        let corner = pixel(at);
        let (was, is) = (luma(bare, width, corner), luma(open, width, corner));
        assert!(
            is <= was + TOLERANCE,
            "{what}: the panel's corner pixel {corner:?} reads {is} over a ground of {was}: the \
             panel stands on a square ground"
        );
    }
}

/// The frame in device pixels at `scale`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a small positive frame size times a density of a few"
)]
fn device_size(scale: f64) -> (u32, u32) {
    (
        (f64::from(SIZE.0) * scale).ceil() as u32,
        (f64::from(SIZE.1) * scale).ceil() as u32,
    )
}

/// What the page draws of `frame` at `scale`, on the GPU and on the CPU: the scene over the window
/// ground, no blur pass run, so each material shows the opaque ground its placeholder holds.
fn drawn_as_a_page(frame: &FrameHarness, scale: f64, theme: &Theme) -> [Vec<u8>; 2] {
    let (width, height) = device_size(scale);
    let mut scene = vello::Scene::new();
    scene.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.content,
        None,
        &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
    );
    scene.append(frame.scene(), Some(Affine::scale(scale)));
    let mut gpu = ui::offscreen::OffscreenRenderer::new(width, height);
    gpu.set_backdrop(None);
    let on_gpu = gpu.render_resilient(&scene, theme.tokens.colors.surfaces.content);
    let mut cpu = ui::offscreen::CpuSurface::new(width, height, 4).expect("a CPU rasterizer");
    cpu.draw(&scene, None);
    [on_gpu, cpu.pixels().to_vec()]
}

fn menu_model() -> WorkspaceMenuModel {
    WorkspaceMenuModel {
        host: Some("tilo.sh".into()),
        actions: vec![WorkspaceAction {
            id: "join-workspace".into(),
            title: "Join a workspace".into(),
            icon: None,
            destructive: false,
        }],
        can_rename: true,
        can_add_strip: true,
        ..WorkspaceMenuModel::default()
    }
}

fn own_card() -> ProfileModel {
    ProfileModel {
        name: "Jo Doe".into(),
        identity: "jo".into(),
        can_rename: true,
        offers_presence: true,
        control_hints: Some(true),
        ..ProfileModel::default()
    }
}

/// The bare desk, the workspace menu open over it and the person's own card open over it, in one
/// theme, each drawn once.
struct Surfaces {
    bare: Framed<WorkspaceMenu>,
    menu: Framed<WorkspaceMenu>,
    card: Framed<ProfileCard>,
}

impl Surfaces {
    fn new(theme: &Theme) -> Self {
        let mut bare = Framed::new(State::new(), WorkspaceMenu::new(), SIZE, theme.clone());
        settle(Framed::harness_mut(&mut bare));
        let mut menu = WorkspaceMenu::new();
        menu.toggle(&menu_model(), NAME, viewport());
        let mut menu = Framed::new(State::new(), menu, SIZE, theme.clone());
        settle(Framed::harness_mut(&mut menu));
        let mut card = ProfileCard::new();
        card.toggle(&own_card(), AVATAR, viewport());
        let mut card = Framed::new(State::new(), card, SIZE, theme.clone());
        settle(Framed::harness_mut(&mut card));
        Self { bare, menu, card }
    }

    /// Each open surface: what it is, its frame and its panel.
    fn open(&self) -> [(&'static str, &FrameHarness, Rect); 2] {
        [
            (
                "the workspace menu",
                Framed::harness(&self.menu),
                self.menu.panel_rect().expect("the workspace menu is open"),
            ),
            (
                "the own profile card",
                Framed::harness(&self.card),
                self.card.rect(),
            ),
        ]
    }
}

#[test]
fn the_bars_menu_and_card_are_each_one_frame_exactly_their_own() {
    ui::i18n::I18nManager::init("en-US");
    for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
        let surfaces = Surfaces::new(&theme);
        let bare = Framed::harness(&surfaces.bare).render_rgba();
        for (what, frame, panel) in surfaces.open() {
            let open = frame.render_rgba();
            assert_one_frame(
                &format!("{what} ({name})"),
                (&bare, &open),
                panel,
                1.0,
                &theme,
            );
        }
    }
}

/// Without the blurred backdrop — a page, which runs no blur pass, or a renderer whose pass failed —
/// each surface is its own rounded panel on its opaque ground, at every density a page is drawn at.
#[test]
fn without_their_backdrop_the_bars_menu_and_card_are_each_their_own_rounded_panel() {
    ui::i18n::I18nManager::init("en-US");
    for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
        let surfaces = Surfaces::new(&theme);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let bare = drawn_as_a_page(Framed::harness(&surfaces.bare), scale, &theme);
            for (what, frame, panel) in surfaces.open() {
                let open = drawn_as_a_page(frame, scale, &theme);
                for (renderer, (bare, open)) in ["GPU", "CPU"].iter().zip(bare.iter().zip(&open)) {
                    assert_one_frame(
                        &format!("{what} ({name}, {renderer}, at {scale}x, without its backdrop)"),
                        (bare, open),
                        panel,
                        scale,
                        &theme,
                    );
                }
            }
        }
    }
}
