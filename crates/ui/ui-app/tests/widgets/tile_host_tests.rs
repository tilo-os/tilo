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

#[test]
fn a_tile_of_its_own_offers_its_apps_history() {
    let (_host, component) = component_with_history();
    let alone = harness(Box::new(TileHost::from_component(component)));
    assert!(alone.targets().contains_key("history-open"));
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
