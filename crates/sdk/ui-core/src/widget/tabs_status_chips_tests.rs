//! The bar's mark of a window an agent opened.

use super::super::tests::lay_out_at_width;
use super::super::{TabBar, TabDescriptor};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use automerge::AutoCommit;
use tilo_desktop_model::ownership::OpenedBy;
use vello::Scene;
use vello::kurbo::Affine;

fn run(title: &str) -> OpenedBy {
    OpenedBy {
        run: "agent-run:7".into(),
        label: "Jane Doe\u{2019}s agent".into(),
        title: title.into(),
    }
}

fn draw(bar: &mut TabBar, tr: &mut TextRenderer, theme: &Theme) {
    bar.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        tr,
        theme,
        &PresenceCtx::default(),
    );
}

/// A window an agent opened names the agent after the pills, only while it
/// is in front; its stop says the run that opened it, not the agent's name
/// again, and a press by pointer or assistive technology hands the host
/// that run.
#[test]
fn a_window_an_agent_opened_names_the_agent_and_hands_its_run_to_the_host() {
    let theme = Theme::light();
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new(
        "opened",
        vec![
            TabDescriptor::new(1, "Application.pdf"),
            TabDescriptor::new(2, "Notes"),
        ],
        1,
    );
    lay_out_at_width(&mut bar, 1200.0);
    assert!(bar.opened_by_chip(&mut tr, &theme).is_none());
    let opened = run("Fill Jane Doe\u{2019}s visa application");
    assert!(bar.set_opened_by(1, Some(opened.clone())));
    assert!(!bar.set_opened_by(1, Some(opened.clone())), "unchanged");
    let (rect, label) = bar
        .opened_by_chip(&mut tr, &theme)
        .expect("the mark stands after the pills");
    assert_eq!(label, "Jane Doe\u{2019}s agent");
    assert!(rect.x0 >= bar.tab_rects[1].x1, "after the pills");

    draw(&mut bar, &mut tr, &theme);
    let (stood, node) = bar.opened_by_stop().expect("the keyboard can stand on it");
    assert_eq!(stood, rect);
    assert_eq!(node.name.as_deref(), Some("Jane Doe\u{2019}s agent"));
    assert_eq!(
        node.description.as_deref(),
        Some("Fill Jane Doe\u{2019}s visa application"),
        "the run alone: the mark names the agent already"
    );

    assert!(bar.press_opened_by(rect.center()));
    assert_eq!(bar.take_opened_by_press(), Some((1, opened.run.clone())));
    assert_eq!(bar.take_opened_by_press(), None, "once");
    assert!(Widget::perform_semantic_action(
        &mut bar,
        super::OPENED_BY_TARGET,
        &crate::a11y::Action::Activate,
        &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT,
    ));
    assert_eq!(bar.take_opened_by_press(), Some((1, opened.run.clone())));

    // A run that goes by nothing but the agent's name says the agent opened
    // the window.
    assert!(bar.set_opened_by(1, Some(run("Jane Doe\u{2019}s agent"))));
    draw(&mut bar, &mut tr, &theme);
    let (_, node) = bar.opened_by_stop().expect("still marked");
    assert_eq!(
        node.description.as_deref(),
        Some("Jane Doe\u{2019}s agent opened this window")
    );

    bar.set_active(2);
    assert!(
        bar.opened_by_chip(&mut tr, &theme).is_none(),
        "only its own window"
    );
    bar.set_active(1);
    assert!(bar.set_opened_by(1, None));
    assert!(bar.opened_by_chip(&mut tr, &theme).is_none());
}

/// The mark stands after the window's search control, never under it.
#[test]
fn the_agents_mark_stands_after_the_windows_search() {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new("search", vec![TabDescriptor::new(1, "Letter")], 1);
    bar.set_search_control(Some(240.0));
    lay_out_at_width(&mut bar, 900.0);
    assert!(bar.set_opened_by(1, Some(run(""))));
    let search_x1 = bar.tabs_end()
        + crate::widget::window_controls::CONTROL_GAP
        + crate::widget::window_controls::CONTROL_PX;
    let (rect, _) = bar
        .opened_by_chip(&mut tr, &theme)
        .expect("room for the mark");
    assert!(
        rect.x0 >= search_x1,
        "{rect:?} after the search at {search_x1}"
    );
}

/// In a window whose app put its slots in the bar, the mark stands right
/// after what the location shows, on its line, before the app's actions.
#[test]
fn the_agents_mark_follows_an_apps_location_in_its_bar() {
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
    assert!(bar.set_opened_by(1, Some(run("File the receipts"))));
    for _ in 0..2 {
        super::super::tests::lay_out_at_width_with(&mut bar, 1200.0, &theme);
        draw(&mut bar, &mut tr, &theme);
    }
    let (rect, _) = bar
        .opened_by_chip(&mut tr, &theme)
        .expect("the mark shows beside the app's slots");
    let (_, _, location, actions) = bar.toolbar_rects();
    assert!(location.area() > 0.0, "the app's location stands");
    assert!(
        (location.x0..location.x1).contains(&rect.x0),
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
}

/// The mark follows the word after an edited document's location, not just its label: "Edited" and
/// the agent's name never stand on each other.
#[test]
fn the_agents_mark_follows_the_edited_word() {
    use crate::window_toolbar::{ToolbarAction, ToolbarLocation, WindowToolbar};
    let _settled = crate::motion::testing::settled();
    let theme = Theme::light().with_reduce_motion(true);
    let mut tr = TextRenderer::new();
    let mut bar = TabBar::new("edited", vec![TabDescriptor::new(1, "Letter")], 1);
    bar.set_window_controls(true);
    assert!(bar.set_toolbar(
        1,
        Some(WindowToolbar {
            location: Some(ToolbarLocation {
                label: "Hello from the agent".into(),
                edited: true,
                ..ToolbarLocation::default()
            }),
            actions: vec![ToolbarAction::new("new", "plus", "New page")],
            ..WindowToolbar::default()
        })
    ));
    assert!(bar.set_opened_by(1, Some(run("Write a paragraph"))));
    for _ in 0..2 {
        super::super::tests::lay_out_at_width_with(&mut bar, 1200.0, &theme);
        draw(&mut bar, &mut tr, &theme);
    }
    let (rect, _) = bar
        .opened_by_chip(&mut tr, &theme)
        .expect("the mark shows beside the app's slots");
    let location = bar
        .painted_labels()
        .into_iter()
        .find(|label| label.widget == "BarLocation")
        .expect("the location is painted");
    let edited = crate::i18n::I18nManager::lookup_plain("window-toolbar-edited");
    assert!(
        location.text.ends_with(&edited),
        "the edited word is shown: {location:?}"
    );
    let [x, _, w, _] = location.rect;
    assert!(
        rect.x0 >= x + w,
        "the mark {rect:?} stands after the location and its edited word, which end at {}",
        x + w
    );
}
