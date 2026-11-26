use super::*;
use crate::tokens::TextRole;
use crate::widget::{AppKind, TaffyTreeExt};

struct Page {
    rect: Rect,
}

impl Widget for Page {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    crate::rect_geometry!();

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &self.rect,
        );
        let style = theme.text(TextRole::Body);
        for line in 0..30u32 {
            let y = self.rect.y0 + 8.0 + f64::from(line) * 24.0;
            if y > self.rect.y1 - 24.0 {
                break;
            }
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine `*` composes floating-point transform matrices"
            )]
            let at = transform * Affine::translate((self.rect.x0 + 24.0, y));
            tr.draw_style(
                scene,
                &format!("{line:02}  The quick brown fox jumps over the lazy dog."),
                theme.tokens.colors.text_primary,
                at,
                None,
                &style,
            );
        }
    }

    fn name(&self) -> &'static str {
        "Page"
    }
}

/// The shell's top bar the maximized window sits under, in logical px.
const TOP: f64 = 36.0;
const W: f64 = 1280.0;
const H: f64 = 800.0;

struct PageApp(Page);

impl Widget for PageApp {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.0.build_layout(taffy, theme, node_map)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.0.apply_layout(taffy, node, origin);
    }
    fn get_rect(&self) -> Rect {
        self.0.get_rect()
    }
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        self.0.draw(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            tr,
            theme,
            env.clone(),
        ));
    }
    fn name(&self) -> &'static str {
        "PageApp"
    }
}

impl crate::widget::app_widget::AppWidget for PageApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Terminal
    }
    fn app_title(&self) -> &'static str {
        "Terminal"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("T")
    }
}

/// Lay the group out as the work area under the shell's top bar, at
/// `scale` (the layout is logical; the scale only matters for paint).
fn lay_out(g: &mut WindowGroup, theme: &Theme) {
    for _ in 0..2 {
        let _ = g.take_structure_dirty();
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, theme, &mut map);
        let mut style = taffy.style(node).cloned().unwrap_or_default();
        style.size = Size {
            width: Dimension::length(f32_of_f64(W)),
            height: Dimension::length(f32_of_f64(H - TOP)),
        };
        let _ = taffy.set_style(node, style);
        let available = Size {
            width: taffy::AvailableSpace::Definite(f32_of_f64(W)),
            height: taffy::AvailableSpace::Definite(f32_of_f64(H - TOP)),
        };
        let mut text = TextRenderer::new();
        let mut bindings = crate::widget::bound_nodes::BoundNodeMap::default();
        // SAFETY: `map` describes the tree just built from `g`, which this
        // function holds exclusively.
        unsafe {
            bindings.rebuild(g, &taffy, node, &map);
            crate::widget::layout::compute_widget_layout(
                &mut taffy, node, available, &map, &bindings, &mut text,
            )
            .unwrap();
        }
        g.apply_layout(&taffy, node, Point::new(0.0, TOP));
    }
}

fn parts() -> crate::window_toolbar::WindowToolbar {
    use crate::window_toolbar::{ToolbarLocation, ToolbarNavigation, WindowToolbar};
    WindowToolbar {
        sidebar: None,
        navigation: Some(ToolbarNavigation {
            back: true,
            forward: true,
        }),
        location: Some(ToolbarLocation {
            label: "duckduckgo.com".into(),
            value: "https://duckduckgo.com/".into(),
            editable: true,
            ..ToolbarLocation::default()
        }),
        actions: Vec::new(),
        ..WindowToolbar::default()
    }
}

fn pages(count: u64) -> WindowGroup {
    let tiles = (1..=count)
        .map(|id| {
            (
                TabDescriptor::new(id, format!("Page {id}")),
                TileContent::App(Box::new(PageApp(Page { rect: Rect::ZERO })) as Box<_>),
            )
        })
        .collect();
    WindowGroup::new(10, tiles, 1, WindowWidth::Normal)
}

#[test]
fn an_apps_parts_live_in_the_bar_and_the_content_starts_under_it() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark().with_reduce_motion(true);
    for count in [1, 2] {
        let mut g = pages(count);
        assert!(g.set_tab_toolbar(1, Some(parts())));
        lay_out(&mut g, &theme);
        let bar = g.bar_rect();
        let expected = if count == 1 {
            crate::widget::tabs::TAB_BAR_HEIGHT
        } else {
            crate::widget::tabs::TAB_BAR_HEIGHT + crate::widget::tabs::TOOLBAR_ROW_HEIGHT
        };
        assert!(
            (bar.height() - expected).abs() < 0.5,
            "{count} tabs: {bar:?}"
        );
        let content = g.active_host().map(Widget::get_rect).unwrap_or(Rect::ZERO);
        assert!(
            content.y0 >= bar.y1 - 0.5,
            "{count} tabs: the bar {bar:?} is over the content {content:?}"
        );
        let location = g.bar_for_tests().location_rect();
        assert!(bar.contains(location.center()), "{count} tabs: in the bar");
        assert_eq!(
            g.bar_grab_hit(location.center()),
            count == 1,
            "{count} tabs: a window of one tab is lifted by its whole bar, a bar of several keeps its presses"
        );
        assert!(g.edit_location() && g.editing_location());
    }
}

/// The name a window's bar shows: the location its app names, or the
/// window's own title while the app names none. A blank label names
/// nothing — an app whose window has nothing open to name — so the bar
/// says what the window is instead of being empty.
#[test]
fn a_window_whose_app_names_no_location_shows_its_own_name() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark().with_reduce_motion(true);
    for count in [1, 2] {
        for label in [None, Some(String::new()), Some("   ".to_owned())] {
            let mut g = pages(count);
            let mut toolbar = parts();
            match label.clone() {
                Some(label) => toolbar.location.as_mut().unwrap().label = label,
                None => toolbar.location = None,
            }
            assert!(g.set_tab_toolbar(1, Some(toolbar)));
            lay_out(&mut g, &theme);
            let shown = g
                .bar_for_tests()
                .toolbar_row()
                .toolbar()
                .and_then(|toolbar| toolbar.location.as_ref())
                .map(|location| location.label.clone());
            assert_eq!(
                shown.as_deref(),
                Some("Page 1"),
                "{count} tab(s), label {label:?}: the window says what it is"
            );
        }
    }
}

#[test]
fn a_fullscreen_window_shows_its_bar_over_its_content_only_while_reached_for() {
    let theme = Theme::dark();
    let mut g = pages(1);
    g.set_fullscreen_presentation(true);
    lay_out(&mut g, &theme);
    let content = g.active_host().map(Widget::get_rect);
    assert!(
        !g.navigation_bar_visible(),
        "covering its display, it shows none"
    );
    // Reached for, it stands under the shell's bar, over the content, which keeps its size.
    g.set_fullscreen_bar(Some(32.0));
    lay_out(&mut g, &theme);
    let bar = g.bar_over_content_rect().expect("over the content");
    assert!((bar.y0 - g.get_rect().y0 - 32.0).abs() < 0.5, "{bar:?}");
    assert_eq!(
        g.active_host().map(Widget::get_rect),
        content,
        "nothing moved"
    );
    // Its control gives the display back, and fills nothing.
    g.press_resize();
    assert!(g.take_pending_exit_fullscreen());
    assert_eq!(g.take_pending_maximize_change(), None);
    g.set_fullscreen_bar(None);
    lay_out(&mut g, &theme);
    assert!(g.bar_over_content_rect().is_none() && !g.navigation_bar_visible());
}

#[test]
fn a_pointer_on_a_tabs_pill_names_the_whole_title() {
    let theme = Theme::dark();
    let title = "Groceries and the whole week's list";
    let mut g = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, title),
            TileContent::App(Box::new(PageApp(Page { rect: Rect::ZERO }))),
        )],
        1,
        WindowWidth::Normal,
    );
    lay_out(&mut g, &theme);
    let pill = g.bar.tab_rects()[0];
    assert!(pill.width() > 0.0, "the tab stands in the bar");
    assert_eq!(
        g.tooltip_target_at(pill.center()),
        Some((pill, title.to_owned())),
        "the tab under the pointer names its whole title"
    );
    assert_eq!(
        g.tooltip_target_at(Point::new(g.rect.x0 - 40.0, g.rect.y0 - 40.0)),
        None,
        "a point off the window names nothing"
    );
}

/// An app whose window is the one a person's shell draws: its shell declares what its header
/// row holds as the window's bar's parts, unless the row holds something the bar cannot draw (a
/// catch-up, a popover), when the shell keeps the row.
struct ShellApp {
    shell: crate::widget::NavShell,
}

impl Widget for ShellApp {
    crate::testing::delegate_widget_methods!(
        shell: sync, build_layout, apply_layout, get_rect
    );

    fn name(&self) -> &str {
        "ShellApp"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.shell]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.shell]
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.shell.update(state, path)
    }
}

impl crate::widget::app_widget::AppWidget for ShellApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Shell"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("S")
    }
}

fn shell_window(
    shell: crate::widget::NavShell,
) -> (
    crate::app::driver::Driver,
    crate::hosted_window::HostedWindow,
) {
    let mut g = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Design"),
            TileContent::App(Box::new(ShellApp { shell })),
        )],
        1,
        WindowWidth::Normal,
    );
    let window = crate::hosted_window::HostedWindow::new();
    if let Some(host) = g.host_for_tab_mut(1) {
        host.set_hosted_window(window.clone());
    }
    let mut frame = crate::app::driver::Driver::for_root(
        crate::State::new(),
        g,
        (u32_of(W), u32_of(H)),
        Theme::dark().with_reduce_motion(true),
    );
    for _ in 0..3 {
        frame.relayout();
    }
    (frame, window)
}

fn frame_of(
    frame: &mut crate::app::driver::Driver,
    parts: &mut crate::window_parts::WindowParts,
    window: &crate::hosted_window::HostedWindow,
) {
    frame.relayout();
    {
        let mut root = frame.root_mut::<WindowGroup>();
        parts.sync_hosted(&[(1, window.clone())], [&mut *root]);
    }
    frame.relayout();
}

fn row_of(frame: &mut crate::app::driver::Driver) -> f64 {
    let mut root = frame.root_mut::<WindowGroup>();
    let g: &mut WindowGroup = &mut root;
    g.active_host_mut()
        .and_then(TileHost::app_mut)
        .and_then(|app| crate::widget::window_header::root_header_row(app as &dyn Widget))
        .map_or(0.0, crate::widget::WindowHeader::header_row_height)
}

/// A header row that cannot stand as the window's bar's parts (it holds a popover) is the window's
/// bar: the app draws it at the window's top, the window draws no bar of its own above it, and its
/// controls stand at the row's trailing end. A header row that stands as the parts draws none and
/// leaves the window its bar.
#[test]
fn a_header_row_that_holds_a_popover_is_the_window_bar() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, _window) = shell_window(
        crate::widget::NavShell::new()
            .content(Box::new(crate::widget::Text::new("Body")))
            .content_header(
                "Design",
                vec![Box::new(
                    crate::widget::Button::new("catch-up", "Mia changed 3 messages")
                        .with_popover(crate::widget::Text::new("What changed")),
                )],
            ),
    );
    let row = row_of(&mut frame);
    let (held, content, window, draws) = {
        let g = frame.root_mut::<WindowGroup>();
        (
            g.held_bar_for_tests(),
            g.active_host().map(Widget::get_rect).unwrap_or(Rect::ZERO),
            g.get_rect(),
            g.draws_its_bar_for_tests(),
        )
    };
    assert!(
        !draws,
        "the row is the window's bar, not a second row under one"
    );
    assert!(row > 0.0, "the app keeps its header row: {row}px");
    assert!(
        (content.y0 - window.y0).abs() < 0.5,
        "the row is the window's top: {content:?} in {window:?}"
    );
    let held = held.expect("the app's row is the window's bar");
    assert!(
        held.controls.height() > 0.0
            && held.controls.y0 >= content.y0 - 0.5
            && held.controls.y1 <= content.y0 + row + 0.5,
        "the window's controls stand in the row: {held:?} in {content:?}"
    );
    assert!(
        content.x1 - held.controls.x1 < 40.0,
        "at its trailing end: {held:?} in {content:?}"
    );

    // An app that declares its toolbar in the window's bar draws no row of its own: the window
    // keeps its bar, and its content stands under it.
    let (mut plain, _window) = shell_window(
        crate::widget::NavShell::new()
            .with_window_toolbar()
            .content(Box::new(crate::widget::Text::new("Body")))
            .content_header(
                "Design",
                vec![Box::new(
                    crate::widget::Button::icon_only("new", icons::Icon::Plus, "New")
                        .on_click(|| {}),
                )],
            ),
    );
    assert_eq!(row_of(&mut plain), 0.0, "the app draws no row of its own");
    let (kept, held, content, window) = {
        let g = plain.root_mut::<WindowGroup>();
        (
            g.draws_its_bar_for_tests(),
            g.held_bar_for_tests(),
            g.active_host().map(Widget::get_rect).unwrap_or(Rect::ZERO),
            g.get_rect(),
        )
    };
    assert!(kept, "the window keeps its bar");
    assert!(held.is_none(), "no row holds it");
    assert!(
        content.y0 > window.y0 + 1.0,
        "the content stands under the bar: {content:?} in {window:?}"
    );
}

#[test]
fn a_row_that_holds_the_bar_keeps_the_windows_functions() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, _window) = shell_window(
        crate::widget::NavShell::new()
            .content(Box::new(crate::widget::Text::new("Body")))
            .content_header(
                "Design",
                vec![Box::new(
                    crate::widget::Button::new("catch-up", "Mia changed 3 messages")
                        .with_popover(crate::widget::Text::new("What changed")),
                )],
            ),
    );
    let (row, kept, controls, search, new_tab, window, grab_ground) = {
        let g = frame.root_mut::<WindowGroup>();
        let part = g
            .held_bar_for_tests()
            .expect("the app's row is the window's bar");
        let search = g.window_search().anchor();
        let grab = Point::new(part.row.x0 + 40.0, part.row.center().y);
        (
            part.row,
            part.controls,
            crate::widget::window_controls::WindowControls::lay_out(part.controls),
            search,
            g.new_tab_rect(),
            g.get_rect(),
            grab,
        )
    };
    assert!(
        window.contains(row.center()) && row.height() > 0.0,
        "{row:?} in {window:?}"
    );
    assert!(
        controls.pill.width() > 0.0 && controls.pill.x1 <= kept.x1 && kept.x1 <= row.x1,
        "the window's controls end the row: {controls:?} in {kept:?} of {row:?}"
    );
    assert!(
        search.area() > 0.0 && row.contains(search.center()),
        "the window's search stands in the row: {search:?}"
    );
    assert!(
        new_tab.area() > 0.0 && row.contains(new_tab.center()),
        "so does its new-tab api: {new_tab:?}"
    );
    assert!(
        new_tab.x1 <= search.x0 && search.x1 <= controls.pill.x0,
        "the API, the search, then the window's controls, as the bar shows them: {new_tab:?}          {search:?} {controls:?}"
    );
    for (name, rect) in [("search", search), ("new tab", new_tab)] {
        assert!(
            (rect.center().y - row.center().y).abs() < 0.5,
            "the {name} stands on the row's centre line: {rect:?} in {row:?}"
        );
    }
    let g = frame.root_mut::<WindowGroup>();
    assert!(g.bar_grab_hit(grab_ground), "the row moves the window");
    assert!(
        !g.bar_grab_hit(new_tab.center()),
        "the new-tab api is a control, not a handle"
    );
    assert!(
        !g.bar_grab_hit(search.center()),
        "the search is a control, not a handle"
    );
    assert!(
        !g.bar_grab_hit(controls.close.center()),
        "close is a control, not a handle"
    );
}

fn holding_row_shell() -> crate::widget::NavShell {
    crate::widget::NavShell::new()
        .content(Box::new(crate::widget::Text::new("Body")))
        .content_header(
            "Design",
            vec![Box::new(
                crate::widget::Button::new("catch-up", "Mia changed 3 messages")
                    .with_popover(crate::widget::Text::new("What changed")),
            )],
        )
}

fn window_control_name(key: &str) -> String {
    crate::i18n::I18nManager::lookup_plain(key)
}

struct MenuApp {
    content: crate::widget::Column,
    toolbar: std::rc::Rc<std::cell::RefCell<crate::window_toolbar::WindowToolbar>>,
    heard: std::rc::Rc<std::cell::RefCell<Vec<crate::window_toolbar::ToolbarEvent>>>,
}

fn menu_parts() -> crate::window_toolbar::WindowToolbar {
    crate::window_toolbar::WindowToolbar {
        actions: vec![
            crate::window_toolbar::ToolbarAction::new("sort", "arrow-up-down", "Sort")
                .labelled()
                .with_items(vec![
                    crate::window_toolbar::ToolbarAction::new("sort~row-0", "", "Everything"),
                    crate::window_toolbar::ToolbarAction::new("sort~row-1", "", "Title"),
                ]),
        ],
        ..crate::window_toolbar::WindowToolbar::default()
    }
}

impl crate::widget::app_widget::AppWidget for MenuApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Notes"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("N")
    }
}

impl crate::widget::Decorator for MenuApp {
    type Inner = crate::widget::Column;

    fn inner(&self) -> &Self::Inner {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.content
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.heard
            .borrow_mut()
            .extend(crate::window_toolbar::take_events());
        crate::window_toolbar::declare(Some(self.toolbar.borrow().clone()));
        self.content.sync(state, path);
    }
}

fn menu_window() -> (
    crate::app::driver::Driver,
    crate::hosted_window::HostedWindow,
    std::rc::Rc<std::cell::RefCell<crate::window_toolbar::WindowToolbar>>,
    std::rc::Rc<std::cell::RefCell<Vec<crate::window_toolbar::ToolbarEvent>>>,
) {
    menu_window_with(menu_parts(), W)
}

fn menu_window_with(
    parts: crate::window_toolbar::WindowToolbar,
    width: f64,
) -> (
    crate::app::driver::Driver,
    crate::hosted_window::HostedWindow,
    std::rc::Rc<std::cell::RefCell<crate::window_toolbar::WindowToolbar>>,
    std::rc::Rc<std::cell::RefCell<Vec<crate::window_toolbar::ToolbarEvent>>>,
) {
    crate::window_toolbar::set_hosted(true);
    let toolbar = std::rc::Rc::new(std::cell::RefCell::new(parts));
    let heard = std::rc::Rc::default();
    let app = MenuApp {
        content: crate::widget::Column::new().push(Box::new(crate::widget::Text::new("Body"))),
        toolbar: std::rc::Rc::clone(&toolbar),
        heard: std::rc::Rc::clone(&heard),
    };
    let mut g = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Notes"),
            TileContent::App(Box::new(app)),
        )],
        1,
        WindowWidth::Normal,
    );
    let window = crate::hosted_window::HostedWindow::new();
    if let Some(host) = g.host_for_tab_mut(1) {
        host.set_hosted_window(window.clone());
    }
    let mut frame = crate::app::driver::Driver::for_root(
        crate::State::new(),
        g,
        (u32_of(width), u32_of(H)),
        Theme::dark().with_reduce_motion(true),
    );
    for _ in 0..2 {
        frame.relayout();
    }
    (frame, window, toolbar, heard)
}

fn sort_button(frame: &mut crate::app::driver::Driver) -> Point {
    let g = frame.root_mut::<WindowGroup>();
    g.bar
        .toolbar_row()
        .toolbar()
        .and_then(|toolbar| {
            toolbar
                .actions
                .iter()
                .position(|action| action.id == "sort")
        })
        .and_then(|index| g.bar.toolbar_row().parts().actions.get(index).copied())
        .expect("the bar lists the sort menu where its button stands")
        .center()
}

fn menu(frame: &mut crate::app::driver::Driver) -> (bool, Vec<String>) {
    let g = frame.root_mut::<WindowGroup>();
    let row = g.bar.toolbar_row();
    (row.popup_open(), row.menu_labels())
}

fn menu_row(frame: &mut crate::app::driver::Driver, label: &str) -> Rect {
    let g = frame.root_mut::<WindowGroup>();
    g.bar
        .toolbar_row()
        .menu_row(label)
        .unwrap_or_else(|| panic!("the menu lists {label}"))
}

#[test]
fn a_menu_in_the_bar_opens_on_one_click_and_stays_open() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, window, toolbar, heard) = menu_window();
    let mut parts = crate::window_parts::WindowParts::default();
    frame_of(&mut frame, &mut parts, &window);
    let sort = sort_button(&mut frame);

    frame.click_at(sort.x, sort.y);
    frame_of(&mut frame, &mut parts, &window);
    let (open, labels) = menu(&mut frame);
    assert!(open, "one click opens the sort menu");
    assert_eq!(labels, ["Everything", "Title"], "the menu's rows are up");
    assert!(
        heard.borrow().is_empty(),
        "opening the menu runs none of its rows"
    );

    for _ in 0..3 {
        frame_of(&mut frame, &mut parts, &window);
    }
    toolbar.borrow_mut().actions.insert(
        0,
        crate::window_toolbar::ToolbarAction::new("archive", "archive", "Archive"),
    );
    frame_of(&mut frame, &mut parts, &window);
    let (open, _) = menu(&mut frame);
    assert!(
        open,
        "the menu stays open while it is read and the app declares its parts again"
    );

    let row = menu_row(&mut frame, "Title");
    frame.click_at(row.center().x, row.center().y);
    frame_of(&mut frame, &mut parts, &window);
    assert_eq!(
        heard.borrow().as_slice(),
        [crate::window_toolbar::ToolbarEvent::Action {
            id: "sort~row-1".into()
        }],
        "the row chosen runs for the button the menu belongs to"
    );
    let (open, _) = menu(&mut frame);
    assert!(!open, "choosing a row closes the menu");

    let sort = sort_button(&mut frame);
    frame.click_at(sort.x, sort.y);
    frame_of(&mut frame, &mut parts, &window);
    assert!(menu(&mut frame).0, "the button opens it again");
    frame.click_at(sort.x, sort.y);
    frame_of(&mut frame, &mut parts, &window);
    assert!(
        !menu(&mut frame).0,
        "a second click on the button closes it"
    );
}

#[test]
fn a_press_on_a_menu_survives_the_app_declaring_its_parts_again() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, window, toolbar, _heard) = menu_window();
    let mut parts = crate::window_parts::WindowParts::default();
    frame_of(&mut frame, &mut parts, &window);
    let sort = sort_button(&mut frame);

    frame.send(&crate::synthetic_input::cursor_moved(sort.x, sort.y));
    frame.send(&crate::synthetic_input::mouse_press());
    // The app declares another command before the menu: the row packs its buttons from its
    // trailing end, so the sort menu keeps its place under the pointer and is no longer the
    // first of the row's actions.
    toolbar.borrow_mut().actions.insert(
        0,
        crate::window_toolbar::ToolbarAction::new("archive", "archive", "Archive"),
    );
    frame_of(&mut frame, &mut parts, &window);
    frame.send(&crate::synthetic_input::mouse_release());
    frame_of(&mut frame, &mut parts, &window);
    let (open, labels) = menu(&mut frame);
    assert!(open, "the press ends on the command it began on");
    assert_eq!(labels, ["Everything", "Title"], "the sort menu opened");
}

fn leading_menu_parts() -> crate::window_toolbar::WindowToolbar {
    crate::window_toolbar::WindowToolbar {
        leading: vec![
            crate::window_toolbar::ToolbarAction::new("ownership", "bot", "Visitor window")
                .with_items(vec![crate::window_toolbar::ToolbarAction::new(
                    "ownership~open",
                    "",
                    "Open in your window",
                )]),
        ],
        ..crate::window_toolbar::WindowToolbar::default()
    }
}

fn overflow_button(frame: &mut crate::app::driver::Driver) -> Point {
    let g = frame.root_mut::<WindowGroup>();
    let rect = g.bar.toolbar_row().parts().overflow;
    assert!(rect.area() > 0.0, "the bar has an overflow button");
    rect.center()
}

/// A leading menu the row cannot fit lists its items in the overflow menu, under its name, and
/// a row chosen there runs for it: never one dead row for the button.
#[test]
fn a_folded_leading_menu_lists_its_items_in_the_overflow_menu() {
    let _settled = crate::motion::testing::settled();
    let mut declared = leading_menu_parts();
    declared.location = Some(crate::window_toolbar::ToolbarLocation {
        label: "tilo.coffee".into(),
        value: "https://tilo.coffee/".into(),
        editable: true,
        ..crate::window_toolbar::ToolbarLocation::default()
    });
    let (mut frame, window, _toolbar, heard) = menu_window_with(declared, 160.0);
    let mut parts = crate::window_parts::WindowParts::default();
    frame_of(&mut frame, &mut parts, &window);
    {
        let g = frame.root_mut::<WindowGroup>();
        let leading = g.bar.toolbar_row().parts().leading.clone();
        assert_eq!(
            leading.first().map(|rect| rect.area()),
            Some(0.0),
            "no area: the leading menu is in the overflow menu"
        );
    }

    let more = overflow_button(&mut frame);
    frame.click_at(more.x, more.y);
    frame_of(&mut frame, &mut parts, &window);
    let (open, labels) = menu(&mut frame);
    assert!(open, "the overflow menu opens");
    assert_eq!(
        labels,
        ["Visitor window", "Open in your window"],
        "the folded menu's name, then its rows"
    );

    let row = menu_row(&mut frame, "Open in your window");
    frame.click_at(row.center().x, row.center().y);
    frame_of(&mut frame, &mut parts, &window);
    assert_eq!(
        heard.borrow().as_slice(),
        [crate::window_toolbar::ToolbarEvent::Action {
            id: "ownership~open".into()
        }],
        "the row chosen runs for the folded menu"
    );
}

fn focused_stop(frame: &crate::app::driver::Driver) -> Option<String> {
    frame.focused_access_node().and_then(|node| node.name)
}

/// The pixels of the frame that wear the toolkit's focus ring around `rect`, as a renderer draws
/// the frame the layout encoded. The ring stands just outside what it rings, so a face painted
/// inside `rect` in a colour near the ring's (an agent's mark) is not taken for it.
#[cfg(not(target_arch = "wasm32"))]
fn ring_pixels(frame: &crate::app::driver::Driver, rect: Rect) -> usize {
    let ring = Theme::dark().border_focus;
    let wanted = [0, 1, 2]
        .map(|channel| crate::num::i32_of_f64(f64::from(ring.components[channel]) * 255.0));
    let width = u32_of(W);
    let rgba = frame.render_rgba();
    let area = rect.inflate(3.0, 3.0);
    let mut found = 0;
    for y in u32_of(area.y0)..u32_of(area.y1) {
        for x in u32_of(area.x0)..u32_of(area.x1) {
            if rect.contains(vello::kurbo::Point::new(
                f64::from(x) + 0.5,
                f64::from(y) + 0.5,
            )) {
                continue;
            }
            let at = crate::num::usize_of_f64(f64::from(y * width + x) * 4.0);
            let near = rgba.get(at..at + 3).is_some_and(|pixel: &[u8]| {
                pixel
                    .iter()
                    .zip(wanted)
                    .all(|(found, want)| (i32::from(*found) - want).abs() < 70)
            });
            found += usize::from(near);
        }
    }
    found
}

#[test]
fn a_row_that_holds_the_bar_puts_the_furniture_in_the_windows_tab_order() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, _window) = shell_window(holding_row_shell());
    let names = [
        crate::widget::tabs::new_tab_name(),
        window_control_name("host-window-maximize"),
        window_control_name("host-window-close"),
    ];
    let stops: Vec<Option<String>> = frame
        .focus_access_nodes()
        .into_iter()
        .map(|node| node.name)
        .collect();
    let at = |name: &str| {
        stops
            .iter()
            .position(|stop| stop.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{name:?} is a stop of the window's Tab order: {stops:?}"))
    };
    let (new_tab, maximize, close) = (at(&names[0]), at(&names[1]), at(&names[2]));
    assert_eq!(
        (new_tab + 1, new_tab + 2),
        (maximize, close),
        "the new-tab api, then restore/maximize, then close: {stops:?}"
    );
    let own = stops
        .get(new_tab.saturating_sub(1))
        .and_then(Clone::clone)
        .unwrap_or_else(|| panic!("a control of the app's own precedes the window's: {stops:?}"));
    assert!(
        new_tab >= 2,
        "the app's own header controls come before the window's furniture: {stops:?}"
    );
    let close_rect = {
        let mut g = frame.root_mut::<WindowGroup>();
        let part = g
            .held_bar_for_tests()
            .expect("the app's row is the window's bar");
        g.set_focused(true);
        assert!(
            g.active_host().is_some_and(|host| host.focused),
            "the app's own tile has the window's focus"
        );
        assert!(
            g.active_host()
                .is_some_and(|host| !Widget::scopes_keyboard_focus(host)),
            "and lets the window carry the keyboard"
        );
        assert!(
            Widget::scopes_keyboard_focus(&*g),
            "the focused window carries the keyboard"
        );
        crate::widget::window_controls::WindowControls::lay_out(part.controls).close
    };

    let mut reached = false;
    for _ in 0..stops.len() {
        frame.key("Tab");
        if focused_stop(&frame) == Some(own.clone()) {
            reached = true;
            break;
        }
    }
    assert!(
        reached,
        "the app's own last header control takes the keyboard"
    );
    assert_eq!(
        ring_pixels(&frame, close_rect),
        0,
        "the window's close wears no ring while the app's control has the keyboard"
    );
    for name in &names {
        frame.key("Tab");
        assert_eq!(
            focused_stop(&frame).as_deref(),
            Some(name.as_str()),
            "Tab lands on {name:?}"
        );
    }
    assert!(
        ring_pixels(&frame, close_rect) > 0,
        "the control the keyboard is on wears the toolkit's focus ring"
    );
    frame.set_modifiers(&["shift".into()]);
    frame.key("Tab");
    frame.set_modifiers(&[]);
    assert_eq!(
        focused_stop(&frame).as_deref(),
        Some(names[1].as_str()),
        "Shift+Tab goes back to restore/maximize"
    );
    frame.key("Tab");
    assert_eq!(
        focused_stop(&frame),
        Some(names[2].clone()),
        "and forward again to close"
    );

    frame.key("Enter");
    assert!(
        frame.root_mut::<WindowGroup>().has_closing_host(1),
        "Enter on the focused close ends the window"
    );
}

/// `v` as a logical pixel count: the fixtures' sizes are small.
fn u32_of(v: f64) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a fixture's viewport: a small positive pixel count"
    )]
    let out = v as u32;
    out
}
