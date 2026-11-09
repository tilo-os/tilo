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
/// window's one tab is told what the bar hosts and runs against it.
fn shell_window(shell: crate::widget::NavShell) -> crate::headless::FrameHarness {
    let mut g = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Design"),
            TileContent::App(Box::new(ShellApp { shell })),
        )],
        1,
        WindowWidth::Normal,
    );
    if let Some(host) = g.host_for_tab_mut(1) {
        host.set_hosted_window(crate::hosted_window::HostedWindow::new());
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
    frame
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
    let mut frame = shell_window(
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
    let mut plain = shell_window(
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
