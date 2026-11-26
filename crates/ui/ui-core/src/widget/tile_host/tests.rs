use super::*;

thread_local! {
    static SEEN: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::default();
}

#[test]
fn an_unresolved_tile_only_refuses_events_meant_for_it() {
    use winit::event::ElementState;
    use winit::keyboard::Key;
    // A tile with no record (still restoring, or starting) refuses its
    // own input, but must not swallow the pointer over a neighbour or keys
    // for the focused window beside it.
    let mut host = TileHost::new(TileContent::Stub {
        kind: AppKind::Editor,
        label: String::new(),
    });
    host.rect = Rect::new(0.0, 0.0, 100.0, 100.0);
    host.owner_lifetime.active.set(false);
    let mut taffy = TaffyTree::new();
    let mut node_map = std::collections::HashMap::new();
    let node = host.build_layout(&mut taffy, &Theme::default(), &mut node_map);
    let send = |host: &mut TileHost, event: &WindowEvent| {
        crate::testing::Dispatch::new()
            .layout(&taffy, node)
            .send(host, event)
            .handled
    };
    let key =
        tilo_ui_base::synthetic_input::keyboard_input(tilo_ui_base::synthetic_input::key_event(
            Key::Character("a".into()),
            None,
            ElementState::Pressed,
        ));
    assert!(!send(
        &mut host,
        &tilo_ui_base::synthetic_input::cursor_moved(250.0, 50.0)
    ));
    assert!(!send(
        &mut host,
        &tilo_ui_base::synthetic_input::mouse_press()
    ));
    assert!(send(
        &mut host,
        &tilo_ui_base::synthetic_input::cursor_moved(50.0, 50.0)
    ));
    assert!(send(
        &mut host,
        &tilo_ui_base::synthetic_input::mouse_press()
    ));
    assert!(!send(&mut host, &key));
    host.set_focused(true);
    assert!(send(&mut host, &key));
}

struct OwnHistoryProbe(std::rc::Rc<std::cell::Cell<Option<bool>>>);

impl Widget for OwnHistoryProbe {
    tilo_ui_base::provides!(dyn crate::widget::WindowHistory);
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "OwnHistoryProbe"
    }
}

impl crate::widget::WindowHistory for OwnHistoryProbe {
    fn set_window_presents_history(&mut self, tab_presents: bool) {
        self.0.set(Some(tab_presents));
    }
    fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
        Some(crate::widget::HistoryControl::Closed)
    }
    fn invoke_window_history_control(
        &mut self,
        _control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
        Ok(())
    }
}

impl crate::widget::app_widget::AppWidget for OwnHistoryProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Own history probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("H")
    }
}

#[test]
fn an_app_that_replaces_a_tiles_content_hears_the_window_presents_its_controls() {
    let first = std::rc::Rc::new(std::cell::Cell::new(None));
    let mut host = TileHost::new(TileContent::App(Box::new(OwnHistoryProbe(
        std::rc::Rc::clone(&first),
    ))));
    host.set_window_presents_history(true);
    assert_eq!(first.get(), Some(true));
    let second = std::rc::Rc::new(std::cell::Cell::new(None));
    host.replace_content(TileContent::App(Box::new(OwnHistoryProbe(
        std::rc::Rc::clone(&second),
    ))));
    assert_eq!(
        second.get(),
        Some(true),
        "the new app is told what the old one was"
    );
}

/// Records the window a panic hook would name while it lays out.
struct ScopeProbe(std::rc::Rc<std::cell::RefCell<Vec<Option<u64>>>>);

impl Widget for ScopeProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.0
            .borrow_mut()
            .push(super::super::panic_scope::current());
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "ScopeProbe"
    }
}

impl crate::widget::app_widget::AppWidget for ScopeProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Scope probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("S")
    }
}

#[test]
fn a_windows_application_runs_under_its_tab_for_a_panic_hook() {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let host = TileHost::new(TileContent::App(Box::new(ScopeProbe(seen.clone()))));
    let mut group = crate::widget::WindowGroup::from_hosts(
        3,
        vec![(crate::widget::TabDescriptor::new(41, "Probe"), host)],
        41,
        crate::widget::WindowWidth::Fraction(500),
    );
    *group.host_for_tab_mut(41).unwrap() =
        TileHost::new(TileContent::App(Box::new(ScopeProbe(seen.clone()))));
    let mut taffy = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    for child in group.children_mut() {
        child.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    }
    assert_eq!(*seen.borrow(), [Some(41)]);
    assert_eq!(super::super::panic_scope::current(), None);
}

struct Starting {
    rect: Rect,
    drawn: std::rc::Rc<std::cell::Cell<bool>>,
    presented: bool,
}

impl Widget for Starting {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &Theme,
        _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _tr, _th, mut env) = ctx.parts();
        self.drawn.set(true);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            STARTING_FLOOD,
            None,
            &self.rect,
        );
    }
    fn name(&self) -> &'static str {
        "Starting"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn presented(&self) -> bool {
        self.presented
    }
}

impl crate::widget::app_widget::AppWidget for Starting {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Starting"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("S")
    }
}

struct TransformProbe {
    rect: Rect,
    drawn_with: std::rc::Rc<std::cell::Cell<Option<Affine>>>,
}

impl Widget for TransformProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &Theme,
        _m: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_scene, transform, _tr, _th, mut env) = ctx.parts();
        self.drawn_with.set(Some(transform));
    }
    fn name(&self) -> &'static str {
        "TransformProbe"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl crate::widget::app_widget::AppWidget for TransformProbe {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Transform probe"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("T")
    }
}

#[test]
fn a_moved_tile_draws_its_app_where_the_tile_went() {
    let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
    let drawn_with = std::rc::Rc::new(std::cell::Cell::new(None));
    let mut host = TileHost::new(TileContent::App(Box::new(TransformProbe {
        rect,
        drawn_with: std::rc::Rc::clone(&drawn_with),
    })));
    host.rect = rect;
    let theme = Theme::default();
    let _ = paint(&mut host, &theme);
    assert_eq!(drawn_with.get(), Some(Affine::IDENTITY));

    host.offset_layout(40.0);
    assert_eq!(host.rect, rect + vello::kurbo::Vec2::new(40.0, 0.0));
    let _ = paint(&mut host, &theme);
    assert_eq!(
        drawn_with.get(),
        Some(Affine::translate((40.0, 0.0))),
        "the app keeps its layout rect and is drawn translated with the tile"
    );

    host.offset_layout(-40.0);
    assert_eq!(host.rect, rect);
    let _ = paint(&mut host, &theme);
    assert_eq!(drawn_with.get(), Some(Affine::IDENTITY));
}

/// Loud and unmistakable, so a single sampled pixel says whether the
/// app painted.
const STARTING_FLOOD: vello::peniko::Color = vello::peniko::Color::from_rgb8(0xFF, 0x00, 0x00);

pub(super) fn starting_host(presented: bool) -> (TileHost, std::rc::Rc<std::cell::Cell<bool>>) {
    let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
    let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut host = TileHost::new(TileContent::App(Box::new(Starting {
        rect,
        drawn: std::rc::Rc::clone(&drawn),
        presented,
    })));
    host.rect = rect;
    (host, drawn)
}

pub(super) fn paint(host: &mut TileHost, theme: &Theme) -> Scene {
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    host.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
    ));
    scene
}

#[test]
fn a_tile_holds_its_place_opaquely_until_the_app_can_be_shown() {
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let theme = Theme::default();
        let rect = Rect::new(50.0, 50.0, 150.0, 150.0);
        let mut host = TileHost::new(TileContent::Notice {
            title: "Calculator".into(),
            detail: "Restoring this window…".into(),
        });
        host.rect = rect;
        assert!(Widget::presented(&host));
        let _ = paint(&mut host, &theme);
        let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
        host.replace_content(TileContent::App(Box::new(Starting {
            rect,
            drawn: std::rc::Rc::clone(&drawn),
            presented: false,
        })));

        assert!(
            Widget::presented(&host),
            "an already visible tab must not disappear"
        );
        assert!(
            host.is_arriving(),
            "pending replacement content stands as an arrival at once"
        );
        let _ = paint(&mut host, &theme);
        assert!(
            !drawn.get(),
            "unready replacement must not draw behind its placeholder"
        );
        let TileContent::App(app) = &mut host.content else {
            panic!("replacement app");
        };
        app.as_any_mut()
            .and_then(|app| app.downcast_mut::<Starting>())
            .expect("controlled replacement")
            .presented = true;
        assert!(
            !host.is_arriving(),
            "the arrival observes new content readiness without a parent presentation poll"
        );
        assert!(Widget::presented(&host));
        let _ = paint(&mut host, &theme);
        assert!(drawn.get(), "ready replacement must become visible");

        let TileContent::App(app) = &mut host.content else {
            panic!("replacement app");
        };
        let app = app
            .as_any_mut()
            .and_then(|app| app.downcast_mut::<Starting>())
            .expect("controlled replacement");
        app.presented = false;
        assert!(Widget::presented(&host));
        assert!(
            !host.is_arriving(),
            "the same content resizing must not stand for an arrival again"
        );
    });

    {
        let theme = Theme::default();
        let (mut host, drawn) = starting_host(false);
        let _ = paint(&mut host, &theme);
        assert!(
            !drawn.get(),
            "the app painted into a tile that is still holding its place; \
         its buffer is not the size of the tab yet, and putting it on \
         screen is the resize-after-open this check exists to prevent",
        );
    }

    {
        let theme = Theme::default();
        let (mut host, drawn) = starting_host(true);
        let _ = paint(&mut host, &theme);
        assert!(
            drawn.get(),
            "an app with a frame that fits its tab must be what the tile \
         paints; anything else is a placeholder access in front of a \
         window that is already there",
        );
    }

    // Case `a_static_tile_holding_a_place_does_not_pump_frames_forever`.
    // Holding a place is not itself an animation: the placeholder's steps
    // are deadlines, so a slow app's first frame never keeps the shell
    // awake.
    {
        let (mut host, _) = starting_host(false);
        let _ = paint(&mut host, &Theme::default());
        assert!(
            !host.needs_animation_frame(),
            "arriving content alone must not turn a slow app into a full-rate compositor loop",
        );
        assert!(
            host.next_wake_deadline().is_some(),
            "its next step is a deadline"
        );
    }

    // Case `a_tile_that_has_shown_a_window_never_holds_a_place_again`.
    // A tile that has ever had a window in it never goes back to holding
    // a place for one — without the latch, a drag on a group edge would
    // flash a placeholder over a window the person is using.
    {
        let (mut host, _) = starting_host(true);
        assert!(Widget::presented(&host), "precondition: it presented");
        let TileContent::App(app) = &mut host.content else {
            panic!("the app");
        };
        app.as_any_mut()
            .and_then(|app| app.downcast_mut::<Starting>())
            .expect("controlled app")
            .presented = false;
        assert!(Widget::presented(&host));
        assert!(
            !host.is_arriving(),
            "a tile that has presented must never stand for an arrival again — \
         a window the person is looking at would be replaced by a placeholder",
        );
    }

    {
        use tilo_ui_base::offscreen::OffscreenRenderer;
        let theme = Theme::default();
        let (mut host, _) = starting_host(false);
        let scene = paint(&mut host, &theme);
        let mut r = OffscreenRenderer::new(200, 200);
        // A ground colour that appears nowhere else, so a pixel of it in
        // the middle of the tile means the ground did not cover it.
        let rgba = r.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 0xFF));
        let (cr, cg, cb, _) = OffscreenRenderer::sample(&rgba, 200, 100, 100);
        assert!(
            !(cr < 40 && cg < 40 && cb > 200),
            "the shell showed through the middle of a window's own place \
         at (100,100) — got ({cr},{cg},{cb}), the backdrop colour. The \
         ground has to hold the space, or the window is a hole until it \
         arrives",
        );
        assert!(
            !(cr > 200 && cg < 80 && cb < 80),
            "the app painted after all; got ({cr},{cg},{cb})",
        );
    }
}

fn harness(root: Box<dyn Widget>) -> crate::app::driver::Driver {
    crate::app::driver::Driver::new_with_controlled_inputs(
        crate::State::new(),
        root,
        (900, 600),
        Theme::dark(),
        tilo_ui_base::font_catalog::fixture_renderer(),
        tilo_action_policy::MemberId::person("reader"),
    )
}

fn window_history_control(driver: &crate::app::driver::Driver) -> Option<bool> {
    fn find(node: &tilo_ui_base::a11y::AccessNode, target: &str) -> Option<bool> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node.state.checked == Some(true));
        }
        node.children.iter().find_map(|child| find(child, target))
    }
    fn walk(node: &crate::headless::InspectionNode, target: &str) -> Option<bool> {
        node.semantics
            .as_ref()
            .and_then(|access| find(access, target))
            .or_else(|| node.children.iter().find_map(|child| walk(child, target)))
    }
    walk(
        &driver.inspection().root,
        crate::widget::tabs::WINDOW_HISTORY_TARGET,
    )
}

#[test]
fn a_shell_window_presents_history_on_its_tab_not_in_a_band() {
    use crate::widget::{TabDescriptor, WindowGroup, WindowWidth};
    let history_tile = || {
        let (host, component) = crate::history_mode::integration_tests::component_with_history();
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
                    || key.ends_with(&format!("/{}", crate::widget::HISTORY_CONTROL_ID))))
    });
    assert!(
        band.is_none(),
        "a hosted tile draws no History band: {band:?} among {:?}",
        targets.keys().collect::<Vec<_>>()
    );
    assert!(
        targets.contains_key(&format!(
            "WindowGroup[0]/TabBar[0]/{}",
            crate::widget::tabs::WINDOW_HISTORY_TARGET
        )),
        "the window's tab offers History instead"
    );
    assert_eq!(window_history_control(&driver), Some(false));

    driver
        .perform_semantic_named(
            "tabs",
            Some(crate::widget::tabs::WINDOW_HISTORY_TARGET),
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
    let press_tab = |driver: &mut crate::app::driver::Driver| {
        driver
            .perform_semantic_named(
                "tabs",
                Some(crate::widget::tabs::WINDOW_HISTORY_TARGET),
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

impl crate::widget::app_widget::AppWidget for Restless {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Restless"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("R")
    }
}

#[test]
fn a_tile_whose_history_shows_a_version_leaves_the_hidden_apps_change_with_it() {
    let (_host, component) = crate::history_mode::integration_tests::component_with_history();
    let mut tile = TileHost::from_component(component);
    tile.content = TileContent::App(Box::new(Restless));
    let history = tile
        .history
        .as_mut()
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
