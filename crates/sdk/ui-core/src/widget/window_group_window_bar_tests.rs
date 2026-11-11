use super::*;
use crate::tokens::TextRole;
use crate::widget::{AppKind, TaffyTreeExt};

/// A page of text that fills what it is given, so a bar over it shows.
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
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

/// The desktop's top bar the maximized window sits under, in logical px.
const TOP: f64 = 36.0;
const W: f64 = 1280.0;
const H: f64 = 800.0;

fn group(content: TileContent) -> WindowGroup {
    let mut g = WindowGroup::new(
        10,
        vec![(TabDescriptor::new(1, "Notes"), content)],
        1,
        WindowWidth::Normal,
    );
    g.maximized = true;
    g
}

fn page_group() -> WindowGroup {
    group(TileContent::App(Box::new(PageApp(Page {
        rect: Rect::ZERO,
    }))))
}

/// A foreign-like app: no header row to merge with.
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
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.0.draw(scene, transform, tr, theme, presence);
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

/// Lay the group out as the work area under the desktop's top bar, at
/// `scale` (the layout is logical; the scale only matters for paint).
fn lay_out(g: &mut WindowGroup, theme: &Theme) {
    // Twice: a NavShell plans against the size it was last given.
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
        let mut bindings = crate::bound_dispatch::BoundNodeMap::default();
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

/// Maximized, the bar and its search stay the window's children, so a
/// host's walks (tooltips, accessibility, redraw) reach them as when
/// restored.
#[test]
fn maximized_the_bar_and_search_stay_children() {
    let theme = Theme::dark();
    let mut g = page_group();
    lay_out(&mut g, &theme);
    assert!(g.maximized, "the fixture is maximized");
    let children = g.children();
    let is = |w: &dyn Widget| {
        children
            .iter()
            .any(|c| std::ptr::addr_eq(*c as *const dyn Widget, w as *const dyn Widget))
    };
    assert!(is(&g.bar), "the bar is a child");
    assert!(is(&g.search), "the search is a child");
}

/// A maximized window keeps its bar in a band above its content: the
/// content starts where the bar ends, and nothing of the app is under it.
#[test]
fn a_maximized_window_keeps_its_bar_above_its_content() {
    let theme = Theme::dark();
    let mut g = page_group();
    lay_out(&mut g, &theme);
    let bar = g.bar_rect();
    let content = g.active_host().map(Widget::get_rect).unwrap_or(Rect::ZERO);
    assert!(bar.height() > 0.0, "the maximized window shows its bar");
    assert!(
        bar.y1 <= content.y0 + 0.5,
        "the bar {bar:?} is over the content {content:?}"
    );
}

/// Lay the group out as the strip does: a child of the work area, so an
/// offset the group asks for applies (the layout root ignores one).
fn lay_out_in_strip(g: &mut WindowGroup, theme: &Theme) {
    let mut taffy = TaffyTree::<()>::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = g.build_layout(&mut taffy, theme, &mut map);
    let strip = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(f32_of_f64(W)),
                    height: Dimension::length(f32_of_f64(H - TOP)),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    {
        use taffy::prelude::TaffyMaxContent;
        taffy
            .compute_layout(strip, taffy::Size::MAX_CONTENT)
            .unwrap();
    }
    g.apply_layout(&taffy, node, Point::new(0.0, TOP));
}

/// The desktop's bar shown over a window moves the window down rather
/// than covering the window's own bar, and moves it whole: no resize.
#[test]
fn the_desktop_bar_shown_over_a_window_moves_it_down_whole() {
    let mut theme = Theme::dark();
    theme.reduce_motion = true;
    let mut g = page_group();
    lay_out_in_strip(&mut g, &theme);
    let before = g.get_rect();
    g.set_top_overlay_inset(TOP);
    lay_out_in_strip(&mut g, &theme);
    let after = g.get_rect();
    assert!(
        (after.y0 - (before.y0 + TOP)).abs() < 0.5,
        "moved down by the desktop bar: {before:?} to {after:?}"
    );
    assert!(
        (after.height() - before.height()).abs() < 0.5,
        "moved, not resized: {before:?} to {after:?}"
    );
    g.set_top_overlay_inset(0.0);
    lay_out_in_strip(&mut g, &theme);
    assert!(
        (g.get_rect().y0 - before.y0).abs() < 0.5,
        "and back up when it hides"
    );
}

/// On a phone the desktop's bar shows over the window under an auto-hiding bar, and its clock reads
/// across the middle of the display: the window's own bar moves down whole, so its Back and the name
/// it shows stand below the clock's band and nothing of the window is left under it.
#[test]
fn a_phone_window_bar_stands_clear_of_the_bar_shown_over_it() {
    let mut theme = Theme::light();
    theme.reduce_motion = true;
    let mut g = page_group();
    assert!(g.set_tab_toolbar(1, Some(slots())));
    // The desk's bar over the window, as tall as the band its clock reads in.
    g.set_top_overlay_inset(TOP);
    lay_out_in_strip(&mut g, &theme);
    let bar = g.bar_rect();
    assert!(
        bar.y0 >= TOP - 0.5,
        "the window's bar stands below the one over it: {bar:?}"
    );
    let location = g.bar_for_tests().location_rect();
    let (back, _, _, _) = g.bar_for_tests().toolbar_rects();
    for control in [back, location] {
        assert!(
            control.area() > 0.0 && control.y0 >= TOP - 0.5,
            "every control on the window's bar stands below the clock's band: {control:?}"
        );
    }
    // A window bar too low for room beneath the clock gives way: the bar is the desk's, the
    // window's own follows it down and its content with it.
    let content = g.active_host().map(Widget::get_rect).unwrap_or(Rect::ZERO);
    assert!(
        content.y0 >= bar.y1 - 0.5,
        "the window's content stands below its own bar: {content:?} under {bar:?}"
    );
}

fn slots() -> crate::window_toolbar::WindowToolbar {
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
fn an_apps_slots_live_in_the_bar_and_the_content_starts_under_it() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark().with_reduce_motion(true);
    for count in [1, 2] {
        let mut g = pages(count);
        assert!(g.set_tab_toolbar(1, Some(slots())));
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
        assert!(
            !g.bar_grab_hit(location.center()),
            "the location is no drag handle"
        );
        assert!(g.edit_location() && g.editing_location());
    }
}

/// The name a window's bar shows: the location its app names, or the
/// window's own title while the app names none. A blank label names
/// nothing — an app whose window has nothing open to name — so the bar
/// says what the window is instead of standing empty.
#[test]
fn a_window_whose_app_names_no_location_shows_its_own_name() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark().with_reduce_motion(true);
    for count in [1, 2] {
        for label in [None, Some(String::new()), Some("   ".to_owned())] {
            let mut g = pages(count);
            let mut toolbar = slots();
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
fn a_fullscreen_windows_bar_fades_over_the_content_and_reduced_motion_cuts() {
    let start = crate::time::now();
    let at = |ms: u64, f: &mut dyn FnMut()| {
        crate::time::testing::with_monotonic_time(start + Duration::from_millis(ms), f);
    };
    for reduced in [false, true] {
        let theme = Theme::dark().with_reduce_motion(reduced);
        let mut g = pages(1);
        g.set_fullscreen_presentation(true);
        let mut content = Rect::ZERO;
        at(0, &mut || {
            lay_out(&mut g, &theme);
            content = g.active_host().map(Widget::get_rect).unwrap_or(Rect::ZERO);
            assert!(!g.navigation_bar_visible(), "fullscreen, the bar hides");
            g.set_fullscreen_bar_revealed(true, crate::tokens::MotionSource::Pointer);
            lay_out(&mut g, &theme);
        });
        let drawn =
            |g: &mut WindowGroup| (g.bar_over_content(), g.active_host().map(Widget::get_rect));
        at(1000, &mut || {
            assert_eq!(
                drawn(&mut g),
                (true, Some(content)),
                "over it, nothing moved"
            );
            g.set_fullscreen_bar_revealed(false, crate::tokens::MotionSource::Pointer);
        });
        at(1030, &mut || {
            lay_out(&mut g, &theme);
            assert_eq!(
                drawn(&mut g),
                (!reduced, Some(content)),
                "reduced motion {reduced}: it fades out, or is gone at once"
            );
        });
        at(3000, &mut || {
            lay_out(&mut g, &theme);
            assert_eq!(drawn(&mut g), (false, Some(content)), "gone, nothing moved");
            assert!(!g.navigation_bar_visible(), "nor laid out to take input");
        });
    }
}

/// The motions of a fullscreen window's bar still asking for frames.
fn bar_wishes(now: crate::time::Instant) -> Vec<String> {
    crate::motion::frames_wanted_by(now)
        .into_iter()
        .filter(|at| at.contains("window_group_fullscreen_bar"))
        .collect()
}

/// A key that shows or hides a fullscreen window's bar (entering fullscreen
/// by Super+F, Super alone) lands it at once: drawn whole on the next frame,
/// gone on the next, and nothing asks for a frame after.
#[test]
fn a_fullscreen_windows_bar_shown_by_a_key_lands_at_once() {
    let theme = Theme::dark();
    let mut g = pages(1);
    g.set_fullscreen_presentation(true);
    lay_out(&mut g, &theme);
    let now = crate::time::now();
    g.set_fullscreen_bar_revealed(true, crate::tokens::MotionSource::Keyboard);
    lay_out(&mut g, &theme);
    assert!(g.bar_over_content(), "shown over the content");
    assert!(
        (g.fullscreen_bar.fade.progress(now) - 1.0).abs() < f64::EPSILON,
        "whole at once"
    );
    assert!(!g.fullscreen_bar_fading(), "nothing left to fade");
    assert_eq!(bar_wishes(now), Vec::<String>::new());
    g.set_fullscreen_bar_revealed(false, crate::tokens::MotionSource::Keyboard);
    lay_out(&mut g, &theme);
    assert!(!g.bar_over_content(), "gone at once");
    assert!(!g.fullscreen_bar_fading());
    assert_eq!(bar_wishes(now), Vec::<String>::new());
}

/// A window's bar wears what the shell told the window of its workspace: two
/// temperatures paint it differently, and the person's own colour is the
/// ring on the tab that has the keyboard.
#[test]
fn a_window_bar_wears_its_workspace_s_temperature_and_the_person_s_colour() {
    use crate::theme::WindowAppearance;
    use crate::tokens::PresenceHue;

    let _settled = crate::motion::testing::settled();
    let colour = |c: vello::peniko::Color| c.premultiply().to_rgba8().to_u32();
    // The bar alone, at rest but for the keyboard, as the window the shell
    // dressed it shows it.
    let paint = |appearance: &WindowAppearance| {
        let theme = Theme::light().in_window(appearance);
        let mut g = pages(2);
        lay_out(&mut g, &theme);
        Widget::set_focus(&mut g.bar, true);
        let mut scene = Scene::new();
        g.bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::empty(),
        );
        (theme, scene.encoding().draw_data.clone())
    };
    let room = WindowAppearance {
        workspace_hue: Some(40.0),
        your_colour: Some(PresenceHue::Magenta),
        corner_radius: Some(12.0),
    };

    // The room's temperature tints the bar's material: a warm room and a cool
    // one are not painted the same.
    let (_, warm) = paint(&room);
    let (_, cool) = paint(&WindowAppearance {
        workspace_hue: Some(220.0),
        ..room
    });
    assert_ne!(
        warm, cool,
        "the bar takes the temperature of the workspace it stands in"
    );

    // The person's colour is the ring of the tab with the keyboard, and
    // another colour rings it in another.
    let (mine, mine_paint) = paint(&room);
    let (theirs, theirs_paint) = paint(&WindowAppearance {
        your_colour: Some(PresenceHue::Green),
        ..room
    });
    let my_ring = colour(mine.tokens.colors.intent.accent_ink);
    let their_ring = colour(theirs.tokens.colors.intent.accent_ink);
    assert_ne!(my_ring, their_ring, "two colours are two inks");
    assert!(
        mine_paint.contains(&my_ring),
        "the keyboard's tab is ringed in the person's own colour"
    );
    assert!(
        !mine_paint.contains(&their_ring),
        "and never in another person's"
    );
    assert!(
        theirs_paint.contains(&their_ring),
        "the colour chosen reaches the bar"
    );
}

/// The bar is part of the window: a pointer resting on a tab's pill names
/// the whole of its title, which the pill cuts to fit, and a point off the
/// bar names nothing.
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

/// An app whose window is the one a person's desktop draws: its shell declares what its header
/// row holds as the window's bar's slots, unless the row holds something the bar cannot draw (a
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

/// A window of `shell` laid out the way a desktop lays one out each frame: the app of the
/// window's one tab is told what the bar hosts and runs against it, and its own window handle
/// comes back with it, where the desktop reads what its bar shows and hands back what the person
/// did there.
fn shell_window(
    shell: crate::widget::NavShell,
) -> (
    crate::headless::FrameHarness,
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
    let mut frame = crate::headless::FrameHarness::for_root(
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

/// A frame as a live desktop runs the window it shows: the window lays out (its app declares what
/// its bar holds), the desktop hands that declaration to the bar of its tab and hands the app
/// what the person did in it, and the window lays out again with both.
fn frame_of(
    frame: &mut crate::headless::FrameHarness,
    slots: &mut crate::window_slots::WindowSlots,
    window: &crate::hosted_window::HostedWindow,
) {
    frame.relayout();
    {
        let mut root = frame.root_mut::<WindowGroup>();
        slots.sync_hosted(&[(1, window.clone())], [&mut *root]);
    }
    frame.relayout();
}

/// The header row's height the window's app draws of its own.
fn row_of(frame: &mut crate::headless::FrameHarness) -> f64 {
    let mut root = frame.root_mut::<WindowGroup>();
    let g: &mut WindowGroup = &mut root;
    g.active_host_mut()
        .and_then(TileHost::app_mut)
        .and_then(|app| crate::widget::window_header::root_header_row(app as &dyn Widget))
        .map_or(0.0, crate::widget::WindowHeader::header_row_height)
}

/// A header row that cannot stand as the window's bar's slots (it holds a popover) is the window's
/// bar: the app draws it at the window's top, the window draws no bar of its own above it, and its
/// controls stand at the row's trailing end. A header row that stands as the slots draws none and
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

    // A header row that stands as the window's bar's slots draws none of its own: the window
    // keeps its bar, and its content stands under it.
    let (mut plain, _window) = shell_window(
        crate::widget::NavShell::new()
            .content(Box::new(crate::widget::Text::new("Body")))
            .content_header(
                "Design",
                vec![Box::new(
                    crate::widget::Button::icon_only("new", icons::Icon::Plus, "New")
                        .on_effect(|| {}),
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

/// A row that is the window's bar keeps every function the bar has: the window's search and its
/// new-tab door stand at the row's trailing end beside its close/restore controls (History is the
/// row's own, the tab row being hidden: `nav_shell::tests::the_window_bar_merges_into_the_header_row`
/// proves it stands there), and a press on the row's own ground moves the window, as one on the
/// bar's does.
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
        let slot = g
            .held_bar_for_tests()
            .expect("the app's row is the window's bar");
        let search = g.window_search().anchor();
        let grab = Point::new(slot.row.x0 + 40.0, slot.row.center().y);
        (
            slot.row,
            slot.controls,
            crate::widget::window_controls::WindowControls::lay_out(slot.controls),
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
        "so does its new-tab door: {new_tab:?}"
    );
    assert!(
        new_tab.x1 <= search.x0 && search.x1 <= controls.pill.x0,
        "the door, the search, then the window's controls, as the bar shows them: {new_tab:?}          {search:?} {controls:?}"
    );
    for (name, rect) in [("search", search), ("new tab", new_tab)] {
        assert!(
            (rect.center().y - row.center().y).abs() < 0.5,
            "the {name} stands on the row's centre line: {rect:?} in {row:?}"
        );
    }
    // The window's controls are the window's, and a press on the row's own ground is the
    // window's to move it by — the drag the bar's pill gives.
    let g = frame.root_mut::<WindowGroup>();
    assert!(g.bar_grab_hit(grab_ground), "the row moves the window");
    assert!(
        !g.bar_grab_hit(new_tab.center()),
        "the new-tab door is a control, not a handle"
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

/// An app whose header row holds a popover: the row is the window's bar.
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

/// The name assistive technology reads for the window's control named by `key`.
fn window_control_name(key: &str) -> String {
    crate::i18n::I18nManager::lookup_plain(key)
}

/// The bounds the tree reports for the node named `name`, when it reads there.
fn node_bounds(frame: &crate::headless::FrameHarness, name: &str) -> Option<accesskit::Rect> {
    let id = frame.a11y_node_named(name)?;
    frame
        .a11y_nodes()
        .into_iter()
        .find(|(node, _)| *node == id)
        .and_then(|(_, node)| node.bounds())
}

/// Where the node named `name` stands in the tree, in reading order.
fn node_order(frame: &crate::headless::FrameHarness, name: &str) -> Option<usize> {
    let id = frame.a11y_node_named(name)?;
    frame.a11y_nodes().iter().position(|(node, _)| *node == id)
}

/// Every name the tree reads: what a failure lists instead of the one it wants.
fn tree_names(frame: &crate::headless::FrameHarness) -> Vec<String> {
    frame
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

/// The window's furniture in a row that holds the bar reaches assistive technology as the bar's
/// own does: the new-tab door, then restore/maximize and close, each where the row draws it, and
/// each performing what it shows when activated. They read after the app's own header controls, as
/// the bar's read after its tabs.
#[test]
fn a_row_that_holds_the_bar_publishes_the_windows_furniture() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, _window) = shell_window(holding_row_shell());
    let (row, new_tab, controls) = {
        let g = frame.root_mut::<WindowGroup>();
        let slot = g
            .held_bar_for_tests()
            .expect("the app's row is the window's bar");
        (
            slot.row,
            g.new_tab_rect(),
            crate::widget::window_controls::WindowControls::lay_out(slot.controls),
        )
    };
    let search = frame.root_mut::<WindowGroup>().window_search().anchor();
    let names = (
        crate::widget::tabs::new_tab_name(),
        window_control_name("host-window-maximize"),
        window_control_name("host-window-close"),
        crate::i18n::I18nManager::lookup_plain("window-search-name"),
    );
    let shown = |frame: &crate::headless::FrameHarness, name: &str| {
        node_bounds(frame, name).unwrap_or_else(|| {
            panic!(
                "no node bounds for {name:?}; the tree reads {:?}",
                tree_names(frame)
            )
        })
    };
    for (name, rect) in [
        (&names.0, new_tab),
        (&names.1, controls.maximize),
        (&names.2, controls.close),
        (&names.3, search),
    ] {
        let bounds = shown(&frame, name);
        assert!(
            (bounds.x0 - rect.x0).abs() < 0.5
                && (bounds.y0 - rect.y0).abs() < 0.5
                && (bounds.x1 - rect.x1).abs() < 0.5
                && (bounds.y1 - rect.y1).abs() < 0.5,
            "{name:?} reports the control's own rect: {bounds:?} for {rect:?}"
        );
        assert!(
            row.contains(rect.center()),
            "{name:?} stands in the row that holds the bar: {rect:?} in {row:?}"
        );
    }
    // The row's own controls come first: the window's furniture and search follow them.
    let own = node_order(&frame, "Mia changed 3 messages").expect("the row's own control is read");
    for name in [&names.0, &names.1, &names.2, &names.3] {
        let at = node_order(&frame, name).expect("read");
        assert!(
            at > own,
            "{name:?} reads after the app's own header controls: {at} after {own}"
        );
    }

    // Each performs what it shows.
    let activate = |frame: &mut crate::headless::FrameHarness, name: &str| {
        let id = frame
            .a11y_node_named(name)
            .unwrap_or_else(|| panic!("no node named {name:?}: {:?}", tree_names(frame)));
        frame
            .perform_a11y_action(id, &crate::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("{name:?}: {error}"));
    };
    activate(&mut frame, &names.0);
    assert_eq!(
        frame.root_mut::<WindowGroup>().take_pending_new_tab(),
        Some(10),
        "the new-tab door asks the shell for a place"
    );

    activate(&mut frame, &names.1);
    assert!(
        frame.root_mut::<WindowGroup>().maximized,
        "restore/maximize changes the window's place"
    );
    frame.relayout();
    assert!(
        node_bounds(&frame, &window_control_name("host-window-restore")).is_some(),
        "the control now shows the other way: {:?}",
        tree_names(&frame)
    );
    assert!(
        node_bounds(&frame, &names.1).is_none(),
        "and no longer reads as maximize"
    );

    activate(&mut frame, &window_control_name("host-window-close"));
    assert!(
        frame.root_mut::<WindowGroup>().has_closing_host(1),
        "close ends the window, as the bar's own does"
    );
}

/// A window whose row stands as the bar's slots keeps the same functions, so a person gets one
/// answer either way: the search and the window's controls stand at the bar's trailing end, and
/// the bar's ground moves the window.
#[test]
fn a_row_standing_as_slots_keeps_the_windows_functions() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, _window) = shell_window(
        crate::widget::NavShell::new()
            .content(Box::new(crate::widget::Text::new("Body")))
            .content_header(
                "Design",
                vec![Box::new(
                    crate::widget::Button::icon_only("new", icons::Icon::Plus, "New")
                        .on_effect(|| {}),
                )],
            ),
    );
    let (bar, search, controls) = {
        let g = frame.root_mut::<WindowGroup>();
        assert!(g.draws_its_bar_for_tests(), "the window draws its own bar");
        (
            g.bar_rect(),
            g.window_search().anchor(),
            g.bar.window_controls().pill,
        )
    };
    assert!(
        search.area() > 0.0 && bar.contains(search.center()),
        "the window's search stands in the bar: {search:?} in {bar:?}"
    );
    assert!(
        controls.area() > 0.0 && bar.contains(controls.center()),
        "the window's controls end the bar: {controls:?} in {bar:?}"
    );
    // The bar's own ground — anywhere on it that is not one of its controls — moves the window.
    let g = frame.root_mut::<WindowGroup>();
    assert!(
        (0..u32_of(bar.width())).any(|x| {
            let at = Point::new(bar.x0 + f64::from(x), bar.center().y);
            g.bar_grab_hit(at)
        }),
        "the bar moves the window"
    );
}

/// An app whose window's bar holds a menu, declaring its slots itself: the desktop hands what it
/// declares to the bar of its tab every frame, as it does for an app that runs on its own.
struct MenuApp {
    content: crate::widget::Column,
    /// What it declares in the window's bar now.
    toolbar: std::rc::Rc<std::cell::RefCell<crate::window_toolbar::WindowToolbar>>,
    /// What the bar handed it back, as it took it.
    heard: std::rc::Rc<std::cell::RefCell<Vec<crate::window_toolbar::ToolbarEvent>>>,
}

/// The slots an app with a menu declares: its button, listing two orders.
fn menu_slots() -> crate::window_toolbar::WindowToolbar {
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

/// The app's own sync: it declares its slots, and takes what the bar handed it back.
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

/// A Notes-like window whose app declares `slots` in its bar, with the app's own window handle
/// and what it hears back.
fn menu_window() -> (
    crate::headless::FrameHarness,
    crate::hosted_window::HostedWindow,
    std::rc::Rc<std::cell::RefCell<crate::window_toolbar::WindowToolbar>>,
    std::rc::Rc<std::cell::RefCell<Vec<crate::window_toolbar::ToolbarEvent>>>,
) {
    menu_window_with(menu_slots(), W)
}

/// The same, in a window `width` wide whose app declares `slots` at first.
fn menu_window_with(
    slots: crate::window_toolbar::WindowToolbar,
    width: f64,
) -> (
    crate::headless::FrameHarness,
    crate::hosted_window::HostedWindow,
    std::rc::Rc<std::cell::RefCell<crate::window_toolbar::WindowToolbar>>,
    std::rc::Rc<std::cell::RefCell<Vec<crate::window_toolbar::ToolbarEvent>>>,
) {
    crate::window_toolbar::set_hosted(true);
    let toolbar = std::rc::Rc::new(std::cell::RefCell::new(slots));
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
    let mut frame = crate::headless::FrameHarness::for_root(
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

/// The sort menu's button in the bar, where it stands.
fn sort_button(frame: &mut crate::headless::FrameHarness) -> Point {
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
        .and_then(|index| g.bar.toolbar_row().slots().actions.get(index).copied())
        .expect("the bar lists the sort menu where its button stands")
        .center()
}

/// Whether the bar has a menu open, and the rows it lists.
fn menu(frame: &mut crate::headless::FrameHarness) -> (bool, Vec<String>) {
    let g = frame.root_mut::<WindowGroup>();
    let row = g.bar.toolbar_row();
    (row.popup_open(), row.menu_labels())
}

/// The row `label` of the open menu, where it stands.
fn menu_row(frame: &mut crate::headless::FrameHarness, label: &str) -> Rect {
    let g = frame.root_mut::<WindowGroup>();
    g.bar
        .toolbar_row()
        .menu_row(label)
        .unwrap_or_else(|| panic!("the menu lists {label}"))
}

/// A menu in the bar opens on one click, stays open while the person reads it (and while the app
/// declares its slots again), closes on a second click on its button, and the row chosen in it
/// reaches the app as that button's row.
#[test]
fn a_menu_in_the_bar_opens_on_one_click_and_stays_open() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, window, toolbar, heard) = menu_window();
    let mut slots = crate::window_slots::WindowSlots::default();
    frame_of(&mut frame, &mut slots, &window);
    let sort = sort_button(&mut frame);

    frame.click_at(sort.x, sort.y);
    frame_of(&mut frame, &mut slots, &window);
    let (open, labels) = menu(&mut frame);
    assert!(open, "one click opens the sort menu");
    assert_eq!(labels, ["Everything", "Title"], "the menu's rows are up");
    assert!(
        heard.borrow().is_empty(),
        "opening the menu runs none of its rows"
    );

    // It stays up: the frames after the click leave it alone, and so does the app declaring its
    // slots again with a command before the menu's button.
    for _ in 0..3 {
        frame_of(&mut frame, &mut slots, &window);
    }
    toolbar.borrow_mut().actions.insert(
        0,
        crate::window_toolbar::ToolbarAction::new("archive", "archive", "Archive"),
    );
    frame_of(&mut frame, &mut slots, &window);
    let (open, _) = menu(&mut frame);
    assert!(
        open,
        "the menu stays open while it is read and the app declares its slots again"
    );

    // The row chosen runs for the menu it stands in, not for whatever now leads the row.
    let row = menu_row(&mut frame, "Title");
    frame.click_at(row.center().x, row.center().y);
    frame_of(&mut frame, &mut slots, &window);
    assert_eq!(
        heard.borrow().as_slice(),
        [crate::window_toolbar::ToolbarEvent::Action {
            id: "sort~row-1".into()
        }],
        "the row chosen runs for the button the menu belongs to"
    );
    let (open, _) = menu(&mut frame);
    assert!(!open, "choosing a row closes the menu");

    // A click opens it again, and a second click on the button closes it.
    let sort = sort_button(&mut frame);
    frame.click_at(sort.x, sort.y);
    frame_of(&mut frame, &mut slots, &window);
    assert!(menu(&mut frame).0, "the button opens it again");
    frame.click_at(sort.x, sort.y);
    frame_of(&mut frame, &mut slots, &window);
    assert!(
        !menu(&mut frame).0,
        "a second click on the button closes it"
    );
}

/// A press on a menu in the bar ends on the command it began on: the app declaring its slots
/// again while the button is down moves that command to another place of the row without moving
/// the button, and the click still opens the menu the person pressed.
#[test]
fn a_press_on_a_menu_survives_the_app_declaring_its_slots_again() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, window, toolbar, _heard) = menu_window();
    let mut slots = crate::window_slots::WindowSlots::default();
    frame_of(&mut frame, &mut slots, &window);
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
    frame_of(&mut frame, &mut slots, &window);
    frame.send(&crate::synthetic_input::mouse_release());
    frame_of(&mut frame, &mut slots, &window);
    let (open, labels) = menu(&mut frame);
    assert!(open, "the press ends on the command it began on");
    assert_eq!(labels, ["Everything", "Title"], "the sort menu opened");
}

/// The slots an app with a menu before the location declares: its visitor button, listing the way
/// into a window of the person's own.
fn leading_menu_slots() -> crate::window_toolbar::WindowToolbar {
    crate::window_toolbar::WindowToolbar {
        leading: vec![
            crate::window_toolbar::ToolbarAction::new("custody", "bot", "Visitor window")
                .with_items(vec![crate::window_toolbar::ToolbarAction::new(
                    "custody~open",
                    "",
                    "Open in your window",
                )]),
        ],
        ..crate::window_toolbar::WindowToolbar::default()
    }
}

/// The leading button `id` in the bar, where it stands.
fn leading_button(frame: &mut crate::headless::FrameHarness, id: &str) -> Point {
    let g = frame.root_mut::<WindowGroup>();
    let row = g.bar.toolbar_row();
    let index = row
        .toolbar()
        .and_then(|toolbar| toolbar.leading.iter().position(|action| action.id == id))
        .expect("the app declares the leading button");
    let rect = row
        .slots()
        .leading
        .get(index)
        .copied()
        .expect("the leading button's place");
    assert!(rect.area() > 0.0, "the row shows {id} where it stands");
    rect.center()
}

/// The bar's overflow button, where it stands.
fn overflow_button(frame: &mut crate::headless::FrameHarness) -> Point {
    let g = frame.root_mut::<WindowGroup>();
    let rect = g.bar.toolbar_row().slots().overflow;
    assert!(rect.area() > 0.0, "the bar has an overflow button");
    rect.center()
}

/// A menu before the location opens on one click, stays open while the person reads it (and while
/// the app declares its slots again), closes on a second click on its button, and the row chosen
/// in it runs for that button, as one after the location does.
#[test]
fn a_menu_before_the_location_opens_like_one_after_it() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, window, toolbar, heard) = menu_window_with(leading_menu_slots(), W);
    let mut slots = crate::window_slots::WindowSlots::default();
    frame_of(&mut frame, &mut slots, &window);
    let button = leading_button(&mut frame, "custody");

    frame.click_at(button.x, button.y);
    frame_of(&mut frame, &mut slots, &window);
    let (open, labels) = menu(&mut frame);
    assert!(open, "one click opens the menu of a leading button");
    assert_eq!(labels, ["Open in your window"], "the menu's rows are up");
    assert!(
        heard.borrow().is_empty(),
        "opening the menu runs none of its rows"
    );

    // It stays up while it is read, and while the app declares its slots again with another
    // command before the menu's button.
    for _ in 0..3 {
        frame_of(&mut frame, &mut slots, &window);
    }
    assert!(menu(&mut frame).0, "the menu stays open while it is read");
    toolbar.borrow_mut().leading.insert(
        0,
        crate::window_toolbar::ToolbarAction::new("site", "arrow-left", "Back to site"),
    );
    frame_of(&mut frame, &mut slots, &window);
    assert!(
        menu(&mut frame).0,
        "the menu stays open while the app declares its slots again"
    );

    // The row chosen runs for the menu it stands in, not for whatever now leads the row.
    let row = menu_row(&mut frame, "Open in your window");
    frame.click_at(row.center().x, row.center().y);
    frame_of(&mut frame, &mut slots, &window);
    assert_eq!(
        heard.borrow().as_slice(),
        [crate::window_toolbar::ToolbarEvent::Action {
            id: "custody~open".into()
        }],
        "the row chosen runs for the button the menu belongs to"
    );
    assert!(!menu(&mut frame).0, "choosing a row closes the menu");

    // A click opens it again, and a second click on the button closes it.
    let button = leading_button(&mut frame, "custody");
    frame.click_at(button.x, button.y);
    frame_of(&mut frame, &mut slots, &window);
    assert!(menu(&mut frame).0, "the button opens it again");
    frame.click_at(button.x, button.y);
    frame_of(&mut frame, &mut slots, &window);
    assert!(
        !menu(&mut frame).0,
        "a second click on the button closes it"
    );
}

/// A leading menu the row has no room for lists its items in the overflow menu, under its name, and
/// a row chosen there runs for it: never one dead row standing for the button.
#[test]
fn a_folded_leading_menu_lists_its_items_in_the_overflow_menu() {
    let _settled = crate::motion::testing::settled();
    let mut declared = leading_menu_slots();
    declared.location = Some(crate::window_toolbar::ToolbarLocation {
        label: "tilo.coffee".into(),
        value: "https://tilo.coffee/".into(),
        editable: true,
        ..crate::window_toolbar::ToolbarLocation::default()
    });
    let (mut frame, window, _toolbar, heard) = menu_window_with(declared, 220.0);
    let mut slots = crate::window_slots::WindowSlots::default();
    frame_of(&mut frame, &mut slots, &window);
    {
        let g = frame.root_mut::<WindowGroup>();
        let leading = g.bar.toolbar_row().slots().leading.clone();
        assert_eq!(
            leading.first().map(|rect| rect.area()),
            Some(0.0),
            "no room: the leading menu is in the overflow menu"
        );
    }

    let more = overflow_button(&mut frame);
    frame.click_at(more.x, more.y);
    frame_of(&mut frame, &mut slots, &window);
    let (open, labels) = menu(&mut frame);
    assert!(open, "the overflow menu opens");
    assert_eq!(
        labels,
        ["Visitor window", "Open in your window"],
        "the folded menu's name, then its rows"
    );

    let row = menu_row(&mut frame, "Open in your window");
    frame.click_at(row.center().x, row.center().y);
    frame_of(&mut frame, &mut slots, &window);
    assert_eq!(
        heard.borrow().as_slice(),
        [crate::window_toolbar::ToolbarEvent::Action {
            id: "custody~open".into()
        }],
        "the row chosen runs for the folded menu"
    );
}

/// The stop the keyboard is on, as a screen reader names it.
fn focused_stop(frame: &crate::headless::FrameHarness) -> Option<String> {
    frame.focused_access_node().and_then(|node| node.name)
}

/// The pixels of the frame that wear the toolkit's focus ring inside `rect`, as a renderer draws
/// the frame the layout encoded.
#[cfg(not(target_arch = "wasm32"))]
fn ring_pixels(frame: &crate::headless::FrameHarness, rect: Rect) -> usize {
    let ring = Theme::dark().border_focus;
    let wanted = [0, 1, 2]
        .map(|channel| crate::num::i32_of_f64(f64::from(ring.components[channel]) * 255.0));
    let width = u32_of(W);
    let rgba = frame.render_rgba();
    let area = rect.inflate(3.0, 3.0);
    let mut found = 0;
    for y in u32_of(area.y0)..u32_of(area.y1) {
        for x in u32_of(area.x0)..u32_of(area.x1) {
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

/// The window's own furniture in a row holding the bar is part of the window's Tab order, after the
/// app's own header controls: the keyboard reaches the new-tab door, then restore/maximize, then
/// close, Shift+Tab goes back the same way, the control it is on wears the toolkit's focus ring, and
/// Enter there does what a pointer press does.
#[test]
fn a_row_that_holds_the_bar_puts_the_furniture_in_the_windows_tab_order() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, _window) = shell_window(holding_row_shell());
    let names = [
        crate::widget::tabs::new_tab_name(),
        window_control_name("host-window-maximize"),
        window_control_name("host-window-close"),
    ];
    // The order the keyboard walks: the app's own header controls, then the window's furniture.
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
        "the new-tab door, then restore/maximize, then close: {stops:?}"
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
        let slot = g
            .held_bar_for_tests()
            .expect("the app's row is the window's bar");
        // Tab travels the window, not the app alone: the row it holds is the window's bar, and
        // the window's furniture stands in it. The app's own tile leaves travel to the window.
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
        crate::widget::window_controls::WindowControls::lay_out(slot.controls).close
    };

    // Tab from the app's own last header control reaches them one by one.
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
    // Shift+Tab walks back the same way.
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

    // Enter does on the close what a pointer press there does.
    frame.key("Enter");
    assert!(
        frame.root_mut::<WindowGroup>().has_closing_host(1),
        "Enter on the focused close ends the window"
    );
}

/// A window that draws its own bar keeps one stop for it: the bar's controls there are its own, not
/// stops of the window's for each of them.
#[test]
fn a_window_with_its_own_band_keeps_one_bar_stop() {
    let _settled = crate::motion::testing::settled();
    let (mut frame, _window) = shell_window(
        crate::widget::NavShell::new()
            .content(Box::new(crate::widget::Text::new("Body")))
            .content_header(
                "Design",
                vec![Box::new(
                    crate::widget::Button::new("plain", "Plain").on_effect(|| {}),
                )],
            ),
    );
    {
        let mut g = frame.root_mut::<WindowGroup>();
        assert!(g.draws_its_bar_for_tests());
        g.set_focused(true);
        // Tab travels the window there too, its bar and its app alike, never the desktop's chrome.
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
    }
    let stops: Vec<Option<String>> = frame
        .focus_access_nodes()
        .into_iter()
        .map(|node| node.name)
        .collect();
    assert!(
        !stops
            .iter()
            .any(|stop| stop.as_deref() == Some(crate::widget::tabs::new_tab_name().as_str())),
        "the band bar's door is no stop of its own: {stops:?}"
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

/// A window an agent opened names the agent in its bar, and that mark is the keyboard's next stop
/// after the window's tab list: Tab lands on it, it wears the toolkit's focus ring, and Enter there
/// hands the host the run that opened the window, as a press does.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_agents_mark_is_the_keyboards_stop_after_the_tab_list() {
    const AGENT: &str = "Jane Doe's agent";
    let _settled = crate::motion::testing::settled();
    let mut g = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Application.pdf"),
            TileContent::Notice {
                title: "Form".into(),
                detail: String::new(),
            },
        )],
        1,
        WindowWidth::Normal,
    );
    assert!(g.set_tab_opened_by(
        1,
        Some(tilo_desktop_model::ownership::OpenedBy {
            run: "agent-run:7".into(),
            label: AGENT.into(),
            title: "Fill Jane Doe's visa application".into(),
        })
    ));
    let mut frame = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        g,
        (u32_of(W), u32_of(H)),
        Theme::dark().with_reduce_motion(true),
    );
    for _ in 0..3 {
        frame.relayout();
    }
    let stops: Vec<(crate::a11y::Role, Option<String>)> = frame
        .focus_access_nodes()
        .into_iter()
        .map(|node| (node.role, node.name))
        .collect();
    let at = stops
        .iter()
        .position(|(_, name)| name.as_deref() == Some(AGENT))
        .unwrap_or_else(|| panic!("the mark is a stop of the keyboard's: {stops:?}"));
    assert!(
        at > 0 && stops[at - 1].0 == crate::a11y::Role::TabBar,
        "right after the window's tab list: {stops:?}"
    );
    let mark = frame
        .root_mut::<WindowGroup>()
        .bar
        .opened_by_stop()
        .map(|(rect, _)| rect)
        .expect("the mark stands in the bar");
    assert_eq!(
        ring_pixels(&frame, mark),
        0,
        "no ring before the keyboard comes"
    );

    let mut walked = Vec::new();
    for _ in 0..=stops.len() {
        frame.key("Tab");
        walked.push(focused_stop(&frame));
        if walked.last().cloned().flatten().as_deref() == Some(AGENT) {
            break;
        }
    }
    assert_eq!(
        walked.last().cloned().flatten().as_deref(),
        Some(AGENT),
        "Tab reaches the mark: {walked:?} of {stops:?}"
    );
    frame.relayout();
    assert!(
        ring_pixels(&frame, mark) > 0,
        "the mark the keyboard is on wears the toolkit's focus ring"
    );
    frame.key("Enter");
    frame.relayout();
    assert_eq!(
        frame.root_mut::<WindowGroup>().take_opened_by_press(),
        Some((1, "agent-run:7".to_owned())),
        "Enter on the mark opens its run, as a press does"
    );
}
