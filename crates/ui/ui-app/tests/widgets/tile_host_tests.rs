use crate::support::application::{APP, ProbeHarness, runner_permissions, runner_probe_definition};
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use taffy::Style;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::application::{AppComponent, LaunchTarget, RestoreInput};
use tilo_ui_app::headless::*;
use tilo_ui_app::history_mode::{HistoryDefinition, HistoryPresentation, HistoryScopeDefinition};
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;

use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

static BOUND: HistoryDefinition = HistoryDefinition {
    app_id: APP,
    scopes: &[HistoryScopeDefinition {
        key: "document",
        title: "Bound notebook",
        presentation: HistoryPresentation::Domain {
            create: tilo_ui_app::history_mode::testing::preview,
        },
    }],
};

/// A component that registers a History with its tile, as an app's launch does.
fn component_with_history() -> (ProbeHarness, AppComponent) {
    let host = ProbeHarness::new();
    let definition = runner_probe_definition();
    let context = host
        .prepare_with_permissions(definition, 950, &LaunchTarget::New, runner_permissions())
        .unwrap();
    let mut component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    component.register_document_history(&BOUND).unwrap();
    (host, component)
}

fn harness(root: Box<dyn Widget>) -> tilo_ui_app::app::driver::Driver {
    tilo_ui_app::app::driver::Driver::new_with_controlled_inputs(
        tilo_ui_app::State::new(),
        root,
        (900, 600),
        Theme::dark(),
        tilo_ui_base::font_catalog::fixture_renderer(),
        tilo_member_types::MemberId::person("reader"),
    )
}

fn window_history_control(driver: &tilo_ui_app::app::driver::Driver) -> Option<bool> {
    fn find(node: &tilo_ui_base::a11y::AccessNode, target: &str) -> Option<bool> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node.state.checked == Some(true));
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    fn walk(node: &tilo_ui_app::headless::InspectionNode, target: &str) -> Option<bool> {
        node.semantics
            .as_ref()
            .and_then(|access| find(access, target))
            .or_else(|| node.children.iter().find_map(|child| walk(child, target)))
    }
    walk(
        &driver.inspection().root,
        tilo_ui_app::widget::tabs::WINDOW_HISTORY_TARGET,
    )
}

#[test]
fn a_shell_window_presents_history_on_its_tab_not_in_a_band() {
    use tilo_ui_app::widget::{TabDescriptor, WindowGroup, WindowWidth};
    let history_tile = || {
        let (host, component) = component_with_history();
        (host, TileHost::from_component(component))
    };

    let (_alone_host, alone_tile) = history_tile();
    let alone = harness(Box::new(alone_tile));
    assert!(alone.targets().contains_key("history-open"));

    let (_host, tile) = history_tile();
    let group = WindowGroup::from_hosts(
        1,
        vec![(TabDescriptor::new(7, "Chat"), tile)],
        7,
        WindowWidth::Normal,
    );
    let mut driver = harness(Box::new(group));
    let targets = driver.targets();
    // The tab bar's own History press shares the band's id, so the band is looked for inside the
    // hosted tile: nothing there may offer History.
    let band = targets.keys().find(|key| {
        key.as_str() == "history-open"
            || (key.starts_with("WindowGroup[0]/TileHost")
                && (key.ends_with("/history-open")
                    || key.ends_with(&format!("/{}", tilo_ui_app::widget::HISTORY_CONTROL_ID))))
    });
    assert!(
        band.is_none(),
        "a hosted tile draws no History band: {band:?} among {:?}",
        targets.keys().collect::<Vec<_>>()
    );
    assert!(
        targets.contains_key(&format!(
            "WindowGroup[0]/TabBar[0]/{}",
            tilo_ui_app::widget::tabs::WINDOW_HISTORY_TARGET
        )),
        "the window's tab offers History instead"
    );
    assert_eq!(window_history_control(&driver), Some(false));

    driver
        .perform_semantic_named(
            "tabs",
            Some(tilo_ui_app::widget::tabs::WINDOW_HISTORY_TARGET),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .unwrap();
    driver.relayout();
    assert!(driver.targets().contains_key("activity_timeline"));
    assert_eq!(window_history_control(&driver), Some(true));
    assert!(
        !driver.targets().contains_key("history-open")
            && !driver.targets().contains_key("history-back-live"),
        "open, History draws no toggle or close of its own: the tab's closes it"
    );
    let press_tab = |driver: &mut tilo_ui_app::app::driver::Driver| {
        driver
            .perform_semantic_named(
                "tabs",
                Some(tilo_ui_app::widget::tabs::WINDOW_HISTORY_TARGET),
                &tilo_ui_base::a11y::Action::Activate,
            )
            .unwrap();
        driver.relayout();
    };
    press_tab(&mut driver);
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert_eq!(window_history_control(&driver), Some(false));

    press_tab(&mut driver);
    assert!(driver.targets().contains_key("activity_timeline"));
    driver.click_named("activity_timeline").unwrap();
    driver.send(&tilo_ui_base::synthetic_input::named_key(
        winit::keyboard::NamedKey::Escape,
    ));
    driver.relayout();
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert_eq!(window_history_control(&driver), Some(false));
}

struct Restless;

impl Widget for Restless {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_scene, _transform, _tr, _th, mut env) = ctx.parts();
    }
    fn name(&self) -> &'static str {
        "Restless"
    }
    fn get_rect(&self) -> Rect {
        Rect::ZERO
    }
    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        tilo_ui_base::widget::StructureChange::Children
    }
}

impl tilo_ui_app::widget::AppWidget for Restless {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Restless"
    }
    fn app_icon(&self) -> tilo_ui_app::widget::IconDescriptor {
        tilo_ui_app::widget::IconDescriptor::Glyph("R")
    }
}

#[test]
fn a_tile_whose_history_shows_a_version_leaves_the_hidden_apps_change_with_it() {
    let (_host, component) = component_with_history();
    let mut tile = TileHost::from_component(component);
    tile.test_set_content(TileContent::App(Box::new(Restless)));
    let history = tile
        .test_history_mut()
        .expect("the component registers a History with its tile");
    history.set_width(400.0);
    assert!(history.request_open(), "the History opens");
    history.refresh(&automerge::AutoCommit::new());
    assert!(
        !history.live_visible(),
        "the test needs the History to lay a version out in the app's place"
    );
    let _ = tile.take_structure_change();
    for turn in 0..3 {
        assert_eq!(
            tile.take_structure_change(),
            tilo_ui_base::widget::StructureChange::None,
            "the History lays a version out in the app's place, so nothing builds the \
             app (turn {turn}): a change taken from it asks for a composition the host \
             never builds the app into, pass after pass"
        );
    }
}
