//! Page controls use the real resource host, admission and confirmation paths.
//! The fixture records admitted jobs without starting package workers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::rc::Rc;
use store::{Store, StoreApp, StoreEffect};
use ui::framework::testing::TestApp;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceApp, ResourceSurface, ViewCx,
};
use ui::widget::{Decorator, Element, Widget};

struct PageSurface {
    app: StoreApp,
    admitted: Vec<StoreEffect>,
}

impl Decorator for PageSurface {
    type Inner = StoreApp;
    fn inner(&self) -> &StoreApp {
        &self.app
    }
    fn inner_mut(&mut self) -> &mut StoreApp {
        &mut self.app
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl ResourceSurface<Store, StoreEffect> for PageSurface {
    fn present(&mut self, state: &Rc<store::resource::StoreState>) {
        <StoreApp as ResourceSurface<Store, StoreEffect>>::present(&mut self.app, state);
    }
    // Deliberately do not call StoreApp's live attach: it starts the worker.
    // Canonical declarations, admission and local presentation remain real.
    fn effect(&mut self, effect: StoreEffect, _: &mut EffectCx<Store, Self>) -> EffectAnswer {
        match &effect {
            StoreEffect::ClearFilters | StoreEffect::RemoveRemote { .. } => {}
            _ => return Err("this fixture only executes page-control requests".into()),
        }
        self.app.admit(effect.clone())?;
        self.admitted.push(effect);
        Ok(Effected::done())
    }
    fn effects(&self) -> Vec<EffectStatus> {
        <StoreApp as ResourceSurface<Store, StoreEffect>>::effects(&self.app)
    }
    fn live_rows(&self) -> Vec<LiveRow> {
        <StoreApp as ResourceSurface<Store, StoreEffect>>::live_rows(&self.app)
    }
}

struct PageStore;
impl ResourceApp for PageStore {
    type Resource = Store;
    const ID: &'static str = "so.tilo.store";
    const TITLE: &'static str = "App Store";
    fn view(cx: &mut ViewCx<Store>) -> Element {
        cx.surface(PageSurface {
            app: store::fixture::seeded_app(),
            admitted: Vec::new(),
        })
    }
}

fn staged(scenario: &str) -> TestApp<PageStore> {
    let mut app = TestApp::<PageStore>::new().unwrap();
    app.harness()
        .with_widget_mut::<PageSurface, _>(|surface| {
            surface.app.set_flatpak_available(true);
            surface.app.stage_scene(Some(scenario));
        })
        .unwrap();
    app.settle();
    app
}

fn admitted(app: &mut TestApp<PageStore>) -> Vec<StoreEffect> {
    app.harness()
        .with_widget_mut::<PageSurface, _>(|surface| surface.admitted.clone())
        .unwrap()
}

fn assert_binding(app: &mut TestApp<PageStore>, control: &str, resource: &str, action: &str) {
    app.harness()
        .with_widget_mut::<PageSurface, _>(|surface| {
            let binding = ui::widget::controls::control_action_bindings(&surface.app)
                .into_iter()
                .find(|binding| binding.control_id == control)
                .expect("the rendered control declares its action binding");
            assert_eq!(
                binding.kind,
                ui::widget::ControlBindingKind::Action(ui::tilo_actions::ActionAddress::new(
                    resource, action,
                ))
            );
        })
        .unwrap();
}

fn assert_page_contains(app: &mut TestApp<PageStore>, point: vello::kurbo::Point) {
    app.harness()
        .with_widget_mut::<PageSurface, _>(|surface| {
            let children = surface.app.children();
            let viewport = children[0]
                .child_clip_rect()
                .expect("the Store page exposes its scrolling viewport");
            assert!(
                viewport.contains(point),
                "page control at {point:?} must be reachable inside {viewport:?}"
            );
        })
        .unwrap();
}

fn click_control(app: &mut TestApp<PageStore>, id: &str) {
    let bounds = app.harness().targets()[id];
    assert_page_contains(app, bounds.center());
    app.harness().click_named(id).unwrap();
}

fn click_button(app: &mut TestApp<PageStore>, label: &str) {
    let bounds = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == ui::accesskit::Role::Button && node.label() == Some(label))
        .and_then(|(_, node)| node.bounds())
        .unwrap_or_else(|| panic!("no native button bounds for {label:?}"));
    let center = vello::kurbo::Point::new(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    if label != "Confirm" {
        assert_page_contains(app, center);
    }
    app.harness().click_at(center.x, center.y);
    app.settle();
}

/// The actual row action reaches the published remove-remote effect after confirmation.
#[test]
fn a_remote_rows_action_invokes_the_remotes_own_remove_action() {
    let mut app = staged("remotes");
    assert!(app.harness().resources().iter().any(|resource| {
        resource.id.as_str() == "store.catalog"
            && resource
                .actions
                .iter()
                .any(|action| action.id == "remove-remote")
    }));
    // Hover the actual row, then use the shared row action's exported bounds.
    let row = app.harness().targets()["store.remote.flathub"];
    app.harness()
        .send(&ui::winit::event::WindowEvent::CursorMoved {
            device_id: ui::winit::event::DeviceId::dummy(),
            position: ui::winit::dpi::PhysicalPosition::new(row.center().x, row.center().y),
        });
    click_button(&mut app, "Remove remote");
    assert!(
        admitted(&mut app).is_empty(),
        "removal requires confirmation"
    );
    click_button(&mut app, "Confirm");
    assert!(
        admitted(&mut app).is_empty(),
        "an empty confirmation cannot authorize remote removal"
    );
    let field = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::TextInput && node.label() == Some("Confirmation")
        })
        .map(|(_, node)| node)
        .expect("the shared confirmation field stays open");
    assert_eq!(field.description(), Some("Type CONFIRM to confirm"));
    let bounds = field
        .bounds()
        .expect("the real confirmation field has bounds");
    app.harness().click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    app.harness().type_text("CONFIRM");
    app.settle();
    assert!(app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.role() == ui::accesskit::Role::TextInput
            && node.label() == Some("Confirmation")
            && node.value() == Some("CONFIRM")
    }));
    click_button(&mut app, "Confirm");
    assert_eq!(
        admitted(&mut app),
        [StoreEffect::RemoveRemote {
            name: "flathub".into()
        }]
    );
}

/// Clearing filters follows the typed action and restores this window's catalogue.
#[test]
fn a_narrowed_catalogue_offers_to_clear_its_filters() {
    let mut app = staged("search");
    assert!(app.harness().resources().iter().any(|resource| {
        resource.id.as_str() == "store.catalog"
            && resource
                .actions
                .iter()
                .any(|action| action.id == "clear-filters")
    }));
    assert_binding(
        &mut app,
        "store.filters.clear",
        "store.catalog",
        "clear-filters",
    );
    click_control(&mut app, "store.filters.clear");
    app.settle();
    assert_eq!(admitted(&mut app), [StoreEffect::ClearFilters]);
    app.harness()
        .with_widget_mut::<PageSurface, _>(|surface| {
            assert!(surface.app.query().is_empty());
            assert_eq!(surface.app.active_filter(), store::SourceFilter::All);
            assert_eq!(surface.app.active_category(), None);
        })
        .unwrap();
    assert!(!app.harness().targets().contains_key("store.filters.clear"));

    let mut plain = TestApp::<PageStore>::new().unwrap();
    assert!(
        !plain
            .harness()
            .targets()
            .contains_key("store.filters.clear"),
        "an unfiltered catalogue has nothing to clear"
    );
}

/// The magnifier opens the shared platform search instead of an app-owned field.
#[test]
fn the_catalogue_searches_through_the_platform_launcher() {
    let mut app = TestApp::<PageStore>::new().unwrap();
    assert!(!app.harness().targets().contains_key("action-palette"));
    assert_binding(&mut app, "search-open", "tilo.host:search", "open");
    click_control(&mut app, "search-open");
    app.settle();
    assert!(app.harness().targets().contains_key("action-palette"));
    assert!(app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.role() == ui::accesskit::Role::TextInput && node.label() == Some("Search")
    }));
    assert!(
        admitted(&mut app).is_empty(),
        "the platform owns the search request"
    );
}
