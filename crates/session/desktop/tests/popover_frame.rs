//! A menu or a card the bar opens is one frame, exactly its own: outside its panel only its shadow
//! paints, and a shadow only darkens what it falls on. A surface sized past its panel that paints a
//! second, lighter frame there (at the shadow's reach, say), or a panel drawn on a square ground
//! that shows at its rounded corners, fails here, in both themes.

use std::time::{Duration, Instant};

use tilo_desktop::profile_card::{ProfileCard, ProfileModel};
use tilo_desktop::workspace_menu::{WorkspaceAction, WorkspaceMenu, WorkspaceMenuModel};
use ui::headless::{FrameHarness, Framed};
use ui::state::State;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};

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

fn rendered<W: Widget + 'static>(root: W, theme: &Theme) -> Vec<u8> {
    let mut frame = Framed::new(State::new(), root, SIZE, theme.clone());
    settle(Framed::harness_mut(&mut frame));
    Framed::harness(&frame).render_rgba()
}

/// The pixel at `(column, row)`'s luminance, in 0..=255.
fn luma(rgba: &[u8], (column, row): (u32, u32)) -> i32 {
    let at = ((row * SIZE.0 + column) * 4) as usize;
    let [red, green, blue] = [rgba[at], rgba[at + 1], rgba[at + 2]].map(i32::from);
    (red * 2126 + green * 7152 + blue * 722) / 10_000
}

/// The pixel `at` lies in, on a frame of [`SIZE`].
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a point on the frame is a small non-negative pixel position"
)]
fn pixel(at: Point) -> (u32, u32) {
    (at.x.floor() as u32, at.y.floor() as u32)
}

/// `open` is `bare` with one surface whose panel is `panel`: every pixel outside the panel is the
/// bare ground or the shadow on it, darker, never lighter, and nothing changed past the shadow's
/// reach. The panel's corner pixels, outside its rounded outline, are ground too.
fn assert_one_frame(what: &str, bare: &[u8], open: &[u8], panel: Rect, theme: &Theme) {
    assert_eq!(bare.len(), open.len(), "{what}: both frames are drawn");
    assert!(panel.area() > 0.0, "{what}: the surface is open: {panel:?}");
    let reach = ui::widget::floating_surface::painted_bounds(panel, theme).inflate(1.0, 1.0);
    let mut changed = Rect::ZERO;
    for y in 0..SIZE.1 {
        for x in 0..SIZE.0 {
            let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            if panel.contains(Point::new(px, py)) {
                continue;
            }
            let (was, is) = (luma(bare, (x, y)), luma(open, (x, y)));
            assert!(
                is <= was + TOLERANCE,
                "{what}: at {x},{y}, outside the panel {panel:?}, the open surface reads lighter \
                 ({is}) than the ground under it ({was}): something past the panel paints a frame \
                 of its own"
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
        Point::new(panel.x0, panel.y0),
        Point::new(panel.x1 - 1.0, panel.y0),
        Point::new(panel.x0, panel.y1 - 1.0),
        Point::new(panel.x1 - 1.0, panel.y1 - 1.0),
    ] {
        let corner = pixel(at);
        let (was, is) = (luma(bare, corner), luma(open, corner));
        assert!(
            is <= was + TOLERANCE,
            "{what}: the panel's corner pixel {corner:?} reads {is} over a ground of {was}: the \
             panel stands on a square ground"
        );
    }
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

#[test]
fn the_bars_menu_and_card_are_each_one_frame_exactly_their_own() {
    ui::i18n::I18nManager::init("en-US");
    for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
        let bare = rendered(WorkspaceMenu::new(), &theme);

        let mut menu = WorkspaceMenu::new();
        menu.toggle(&menu_model(), NAME, viewport());
        let mut frame = Framed::new(State::new(), menu, SIZE, theme.clone());
        settle(Framed::harness_mut(&mut frame));
        let panel = frame.panel_rect().expect("the workspace menu is open");
        let open = Framed::harness(&frame).render_rgba();
        assert_one_frame(
            &format!("the workspace menu ({name})"),
            &bare,
            &open,
            panel,
            &theme,
        );

        let mut card = ProfileCard::new();
        card.toggle(&own_card(), AVATAR, viewport());
        let mut frame = Framed::new(State::new(), card, SIZE, theme.clone());
        settle(Framed::harness_mut(&mut frame));
        let panel = frame.rect();
        let open = Framed::harness(&frame).render_rgba();
        assert_one_frame(
            &format!("the own profile card ({name})"),
            &bare,
            &open,
            panel,
            &theme,
        );
    }
}
