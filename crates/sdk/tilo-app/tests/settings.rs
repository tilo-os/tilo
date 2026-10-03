#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Typed settings reach views and surfaces from their context, follow writes
//! made elsewhere, persist with the profile and never enter the document.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Log {
    pub title: String,
}

#[resource(kind = "test.log", label = "Log")]
impl Log {
    /// Retitle the log.
    #[action]
    pub fn retitle(&mut self, title: String) {
        self.title = title;
    }
}

/// Preferences that follow the person.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Prefs {
    confirm: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { confirm: true }
    }
}

impl SettingsSchema for Prefs {
    const SCOPE: Scope = Scope::Personal;
}

/// Where this device's window was last left.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Placement {
    column: u32,
}

impl SettingsSchema for Placement {
    const SCOPE: Scope = Scope::DeviceLocal;
}

/// The setting a surface received, shared with the test.
type Kept = Rc<RefCell<Option<Setting<Placement>>>>;

thread_local! {
    static SURFACE: RefCell<Option<Kept>> = const { RefCell::new(None) };
}

struct LogApp;

impl ResourceApp for LogApp {
    type Resource = Log;
    const ID: &'static str = "so.tilo.test-log";
    const TITLE: &'static str = "Log";

    fn view(cx: &mut ViewCx<Log>) -> Element {
        let prefs = cx.settings::<Prefs>();
        let toggle = prefs.clone();
        Box::new(
            Column::new()
                .push(Box::new(
                    Text::from_state(&prefs.state(), |prefs| {
                        format!("confirm {}", if prefs.confirm { "on" } else { "off" })
                    })
                    .with_id("confirm"),
                ))
                .push(Box::new(
                    Button::new("toggle", "Toggle confirmation", None).on_click(move || {
                        toggle
                            .update(|prefs| prefs.confirm = !prefs.confirm)
                            .unwrap();
                    }),
                ))
                .push(cx.surface(PlacementProbe::default())),
        )
    }
}

/// A custom surface that keeps a device-local setting from its context.
#[derive(Default)]
struct PlacementProbe {
    rect: Rect,
    placement: Kept,
}

impl Widget for PlacementProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "PlacementProbe"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<Log> for PlacementProbe {
    fn present(&mut self, _: &Rc<Log>) {}

    fn attach(&mut self, cx: SurfaceCx<Log>) {
        *self.placement.borrow_mut() = Some(cx.settings::<Placement>());
        SURFACE.with(|probe| *probe.borrow_mut() = Some(Rc::clone(&self.placement)));
    }
}

fn surface_placement() -> Setting<Placement> {
    SURFACE
        .with(|probe| probe.borrow().clone())
        .unwrap()
        .borrow()
        .clone()
        .unwrap()
}

#[test]
fn settings_follow_writes_from_elsewhere_and_persist_outside_the_document() {
    let mut app = TestApp::<LogApp>::new().unwrap();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("confirm on")
    );

    // A control writes through the setting: the view follows at once.
    app.harness().click_named("toggle").unwrap();
    app.harness().relayout();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("confirm off")
    );
    assert_eq!(app.settings::<Prefs>().get(), Prefs { confirm: false });

    // Another window or device writes it: the running view follows.
    app.settings::<Prefs>()
        .set(&Prefs { confirm: true })
        .unwrap();
    app.harness().relayout();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("confirm on")
    );

    // A surface keeps its device-local setting from its context.
    surface_placement().set(Placement { column: 3 }).unwrap();
    assert_eq!(app.settings::<Placement>().get(), Placement { column: 3 });

    // None of it is the document: no undo step, no change to the resource.
    assert_eq!(app.state(), Log::default());
    assert!(!app.undo());

    // Both persist with the profile across a relaunch.
    let app = app.reopen().unwrap();
    assert_eq!(app.settings::<Prefs>().get(), Prefs { confirm: true });
    assert_eq!(surface_placement().get(), Placement { column: 3 });
}

#[test]
fn surfaces_reach_the_apps_item_secrets() {
    struct Keeper;
    impl ResourceApp for Keeper {
        type Resource = Log;
        const ID: &'static str = "so.tilo.test-keeper";
        const TITLE: &'static str = "Keeper";

        fn view(cx: &mut ViewCx<Log>) -> Element {
            let secret = cx.storage().secrets().item("password", "profile-1");
            secret.put_blocking(b"hunter2").unwrap();
            assert_eq!(cx.storage().app_id(), "so.tilo.test-keeper");
            Box::new(Text::new(if secret.present_blocking() {
                "password kept here"
            } else {
                "no password here"
            }))
        }
    }
    let mut app = TestApp::<Keeper>::new().unwrap();
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("password kept here")
    );
}
