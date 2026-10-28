//! The bar's status chips: "Controls your desktop", "Dev build by …" and
//! "Shared with …".

use super::super::tests::lay_out_at_width;
use super::super::{TabBar, TabDescriptor};
use super::{SharedBy, SharedWith};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use automerge::AutoCommit;
use vello::Scene;
use vello::kurbo::Affine;

/// A window whose app may control the desktop says so beside its pill,
/// and a press on the
/// chip — by pointer or by an assistive technology — reaches the host.
#[test]
fn a_window_that_controls_the_desktop_says_so_and_opens_on_press() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new("terminal", vec![TabDescriptor::new(1, "Terminal")], 1);
    lay_out_at_width(&mut bar, 900.0);
    assert!(bar.desktop_control_chip(&mut tr, &theme).is_none());
    assert!(bar.set_desktop_control(1, true));
    let (rect, label) = bar
        .desktop_control_chip(&mut tr, &theme)
        .expect("the chip stands beside the pill");
    assert_eq!(label, "Controls your desktop");
    assert!(
        rect.x0 >= bar.tab_rects[0].x1,
        "{rect:?} after {:?}",
        bar.tab_rects[0]
    );
    let mut scene = Scene::new();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::default(),
    );
    assert!(bar.press_desktop_control(rect.center()));
    assert_eq!(bar.take_desktop_control_press(), Some((1, rect)));
    assert_eq!(bar.take_desktop_control_press(), None, "once");
    assert!(Widget::perform_semantic_action(
        &mut bar,
        super::DESKTOP_CONTROL_TARGET,
        &crate::a11y::Action::Activate,
        &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT,
    ));
    assert_eq!(bar.take_desktop_control_press(), Some((1, rect)));
    // The window's search control stands right after the tabs; the
    // chips stand after it, never under it.
    bar.set_search_control(Some(240.0));
    lay_out_at_width(&mut bar, 900.0);
    let search_x1 = bar.tabs_end()
        + crate::widget::window_controls::CONTROL_GAP
        + crate::widget::window_controls::CONTROL_PX;
    let (rect, _) = bar
        .desktop_control_chip(&mut tr, &theme)
        .expect("room for the chip");
    assert!(
        rect.x0 >= search_x1,
        "{rect:?} after the search at {search_x1}"
    );
    assert!(bar.set_desktop_control(1, false));
    assert!(bar.desktop_control_chip(&mut tr, &theme).is_none());
}

/// A window running a developer's own build says whose, after the
/// "Controls your desktop" chip when that shows, and only while in front.
#[test]
fn a_window_running_a_dev_build_says_who_installed_it() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new(
        "chat",
        vec![
            TabDescriptor::new(1, "Chat"),
            TabDescriptor::new(2, "Notes"),
        ],
        1,
    );
    lay_out_at_width(&mut bar, 1200.0);
    assert!(bar.dev_build_chip(&mut tr, &theme).is_none());
    assert!(bar.set_dev_build(1, Some("Ada".into())));
    assert!(!bar.set_dev_build(1, Some("Ada".into())), "unchanged");
    let (alone, label) = bar.dev_build_chip(&mut tr, &theme).expect("the mark shows");
    assert_eq!(label, "Dev build by Ada");
    assert!(alone.x0 >= bar.tab_rects[1].x1, "after the pills");
    bar.set_desktop_control(1, true);
    let (control, _) = bar.desktop_control_chip(&mut tr, &theme).unwrap();
    let (beside, _) = bar.dev_build_chip(&mut tr, &theme).unwrap();
    assert!(beside.x0 >= control.x1, "{beside:?} after {control:?}");
    let mut scene = Scene::new();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::default(),
    );
    assert_eq!(
        bar.dev_build_access().and_then(|node| node.name),
        Some("Dev build by Ada".to_owned())
    );
    // `TILO_SHOT_OUT=<file.png>` keeps a picture of the bar to look at.
    if let Some(out) = std::env::var_os("TILO_SHOT_OUT") {
        bar.set_desktop_control(1, false);
        crate::launch::render_widget_png(
            &mut bar,
            (900, 48),
            Theme::dark(),
            std::path::Path::new(&out),
        )
        .unwrap();
    }
    bar.set_active(2);
    assert!(
        bar.dev_build_chip(&mut tr, &theme).is_none(),
        "only its own window"
    );
    assert!(bar.set_dev_build(1, None));
}

/// A window shared with its workspace says with which, and shows the
/// faces of the people who see it after the other status chips, only
/// while it is in front and only while it is shared.
#[test]
fn a_shared_window_names_who_sees_it_with_their_faces() {
    use crate::permissions::UserId;
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new(
        "shared",
        vec![
            TabDescriptor::new(1, "Terminal"),
            TabDescriptor::new(2, "Notes"),
        ],
        1,
    );
    lay_out_at_width(&mut bar, 1200.0);
    assert!(bar.shared_chip(&mut tr, &theme).is_none());
    let lounge = SharedWith {
        place: "Lounge".into(),
        people: vec![
            (UserId("ana".into()), "Ana Lima".into()),
            (UserId("ben".into()), "Ben Okafor".into()),
        ],
    };
    assert!(bar.set_shared(1, Some(lounge.clone())));
    assert!(!bar.set_shared(1, Some(lounge.clone())), "unchanged");
    let (alone, label) = bar.shared_chip(&mut tr, &theme).expect("the mark shows");
    assert_eq!(label, "Shared with Lounge");
    assert!(alone.x0 >= bar.tab_rects[1].x1, "after the pills");
    let chip_only = crate::widget::ChipFace::intrinsic_width(
        &label,
        crate::widget::ChipLead::None,
        false,
        &mut tr,
        &theme,
    );
    assert!(
        alone.width() >= chip_only + crate::presence::FaceStack::WINDOW.width(2),
        "the faces of both stand after the chip: {alone:?}"
    );
    bar.set_dev_build(1, Some("Ada".into()));
    let (dev, _) = bar.dev_build_chip(&mut tr, &theme).unwrap();
    let (beside, _) = bar.shared_chip(&mut tr, &theme).unwrap();
    assert!(beside.x0 >= dev.x1, "{beside:?} after {dev:?}");
    bar.set_dev_build(1, None);

    let mut scene = Scene::new();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::default(),
    );
    let node = bar.shared_access().expect("a reader meets the mark");
    assert_eq!(node.name.as_deref(), Some("Shared with Lounge"));
    assert_eq!(node.description.as_deref(), Some("Ana Lima, Ben Okafor"));

    bar.set_active(2);
    assert!(
        bar.shared_chip(&mut tr, &theme).is_none(),
        "only its own window"
    );
    bar.set_active(1);
    assert!(bar.set_shared(1, None));
    assert!(
        bar.shared_chip(&mut tr, &theme).is_none(),
        "a share that ended is no longer marked"
    );
}

/// A window someone else shared with the workspace reads as theirs in its
/// bar: "Ana Lima’s window" with her face, said to a reader too, only while
/// it is in front and only while it is shared.
#[test]
fn a_window_someone_shared_names_whose_it_is_with_their_face() {
    use crate::permissions::UserId;
    let theme = Theme::light();
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new(
        "shared-by",
        vec![
            TabDescriptor::new(1, "Agenda"),
            TabDescriptor::new(2, "Notes"),
        ],
        1,
    );
    lay_out_at_width(&mut bar, 1200.0);
    let ana = SharedBy {
        sharer: (UserId("ana".into()), "Ana Lima".into()),
    };
    assert!(bar.set_shared_by(1, Some(ana.clone())));
    assert!(!bar.set_shared_by(1, Some(ana.clone())), "unchanged");
    assert_eq!(bar.shared_by(1), Some(&ana));
    assert_eq!(bar.shared_with(1), None, "shared with this person, not by");
    assert!(
        !bar.set_shared(1, None),
        "no mark of the other kind to clear"
    );
    assert_eq!(bar.shared_by(1), Some(&ana));
    let (rect, label) = bar.shared_chip(&mut tr, &theme).expect("the mark shows");
    assert_eq!(label, "Ana Lima\u{2019}s window");
    assert!(rect.x0 >= bar.tab_rects[1].x1, "after the pills");
    let chip_only = crate::widget::ChipFace::intrinsic_width(
        &label,
        crate::widget::ChipLead::None,
        false,
        &mut tr,
        &theme,
    );
    assert!(
        rect.width() >= chip_only + crate::presence::FaceStack::WINDOW.width(1),
        "her face stands after the chip: {rect:?}"
    );

    let mut scene = Scene::new();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::default(),
    );
    let node = bar.shared_access().expect("a reader meets the mark");
    assert_eq!(node.name.as_deref(), Some("Ana Lima\u{2019}s window"));
    assert_eq!(node.description, None, "the name says whose it is");

    bar.set_active(2);
    assert!(
        bar.shared_chip(&mut tr, &theme).is_none(),
        "only its own window"
    );
    bar.set_active(1);
    assert!(bar.set_shared_by(1, None));
    assert!(
        bar.shared_chip(&mut tr, &theme).is_none(),
        "a share that ended is no longer marked"
    );
}

/// In a window whose app put its slots in the bar, the share mark stands
/// right after what the location shows, on its line, before the app's
/// actions: "Agenda", then "Ana Lima’s window" with her face.
#[test]
fn the_share_mark_follows_an_apps_location_in_its_bar() {
    use crate::permissions::UserId;
    use crate::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};
    let _settled = crate::motion::testing::settled();
    let theme = Theme::light().with_reduce_motion(true);
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new("slots", vec![TabDescriptor::new(1, "Agenda")], 1);
    bar.set_window_controls(true);
    assert!(bar.set_toolbar(
        1,
        Some(WindowToolbar {
            location: Some(ToolbarLocation {
                label: "Agenda".into(),
                ..ToolbarLocation::default()
            }),
            actions: vec![ToolbarAction::new("new", "plus", "New note")],
            ..WindowToolbar::default()
        })
    ));
    assert!(bar.set_shared_by(
        1,
        Some(SharedBy {
            sharer: (UserId("ana".into()), "Ana Lima".into()),
        })
    ));
    let mut scene = Scene::new();
    for _ in 0..2 {
        super::super::tests::lay_out_at_width_with(&mut bar, 1200.0, &theme);
        bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
    }
    let (rect, label) = bar
        .shared_chip(&mut tr, &theme)
        .expect("the mark shows beside the app's slots");
    assert_eq!(label, "Ana Lima\u{2019}s window");
    let (_, _, location, actions) = bar.toolbar_rects();
    assert!(location.area() > 0.0, "the app's location stands");
    assert!(
        rect.x0 > location.x0,
        "right after what the location shows: {rect:?} in {location:?}"
    );
    assert!(
        rect.x0 < location.x1,
        "right after what the location shows: {rect:?} in {location:?}"
    );
    assert!(
        (rect.center().y - location.center().y).abs() < 0.5,
        "on the location's line"
    );
    assert!(
        actions.iter().all(|action| rect.x1 <= action.x0),
        "before the app's actions: {rect:?} {actions:?}"
    );
    let node = bar.shared_access().expect("a reader meets the mark");
    assert_eq!(node.name.as_deref(), Some("Ana Lima\u{2019}s window"));
}
