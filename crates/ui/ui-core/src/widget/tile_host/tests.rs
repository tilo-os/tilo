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
    let mut host = TileHost::new(TileContent::App(Box::new(ScopeProbe(seen.clone()))));
    host.set_panic_scope(41);
    let mut taffy = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    host.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
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
