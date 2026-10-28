use super::*;
use crate::widget::{AppKind, NavShell, TaffyTreeExt};

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

/// A Tilo app drawn in-process: a `NavShell` with a sidebar, a content
/// column and their header rows.
struct HeaderedApp {
    rect: Rect,
    shell: NavShell,
}

impl HeaderedApp {
    fn new(header_in_bar: bool) -> Self {
        let shell = NavShell::new()
            .sidebar(Box::new(Page { rect: Rect::ZERO }))
            .content(Box::new(Page { rect: Rect::ZERO }))
            .sidebar_header("Notes", Vec::new())
            .content_header(
                "Groceries",
                vec![Box::new(crate::widget::Button::icon_only(
                    "more",
                    crate::icons::Icon::MoreHorizontal,
                    "More",
                ))],
            );
        let shell = if header_in_bar {
            shell.with_header_in_window_bar()
        } else {
            shell
        };
        Self {
            rect: Rect::ZERO,
            shell,
        }
    }
}

impl Widget for HeaderedApp {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.shell.build_layout(taffy, theme, node_map);
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(&child) = taffy.children(node).unwrap_or_default().first() {
            self.shell
                .apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell.draw(scene, transform, tr, theme, presence);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.shell]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.shell]
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.shell.take_structure_dirty()
    }

    fn name(&self) -> &'static str {
        "HeaderedApp"
    }
}

impl crate::widget::app_widget::AppWidget for HeaderedApp {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Notes"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("N")
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
    fn uses_host_record(&self) -> bool {
        false
    }
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

fn paint(g: &mut WindowGroup, theme: &Theme, scale: f64) -> Scene {
    let mut scene = Scene::new();
    let t = Affine::scale(scale);
    // The desktop's top bar above the window, so a gap would show.
    scene.fill(
        vello::peniko::Fill::NonZero,
        t,
        theme.tokens.colors.surfaces.base,
        None,
        &Rect::new(0.0, 0.0, W, TOP),
    );
    g.draw(
        &mut scene,
        t,
        &mut TextRenderer::new(),
        theme,
        &PresenceCtx::empty(),
    );
    scene
}

/// Review frames: a maximized foreign-like window at rest (1× and 2×),
/// the bar revealed and its Restore hovered, and a Tilo app with its
/// header in the bar at rest and revealed, plus one with a permanent
/// header and the controls merged into its row.
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

#[test]
#[ignore = "a review tool: needs a GPU adapter; writes PNGs to TILO_UI_SCREENSHOT_DIR"]
fn render_maximized_bar_frames() {
    let dir = std::path::PathBuf::from(
        std::env::var("TILO_UI_SCREENSHOT_DIR").expect("TILO_UI_SCREENSHOT_DIR"),
    );
    std::fs::create_dir_all(&dir).expect("shot dir");
    let theme = Theme::dark().with_reduce_motion(true);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "small fixed canvas"
    )]
    let shoot = |g: &mut WindowGroup, scale: f64, name: &str, pointer: Option<Point>| {
        let (pw, ph) = ((W * scale) as u32, (H * scale) as u32);
        let mut renderer =
            crate::offscreen::OffscreenRenderer::try_new(pw, ph).expect("a GPU adapter");
        let mut scene = paint(g, &theme, scale);
        if let Some(p) = pointer {
            let dot = vello::kurbo::Circle::new(p, 4.0);
            let t = Affine::scale(scale);
            scene.fill(
                vello::peniko::Fill::NonZero,
                t,
                vello::peniko::Color::from_rgb8(255, 210, 0),
                None,
                &dot,
            );
        }
        let rgba = renderer.render(&scene, vello::peniko::Color::BLACK);
        crate::png::write_rgba8(dir.join(format!("{name}.png")), pw, ph, &rgba)
            .expect("write frame");
    };

    for scale in [1.0, 2.0] {
        let mut g = page_group();
        lay_out(&mut g, &theme);
        shoot(&mut g, scale, &format!("maximized-rest-{scale}x"), None);
    }
    // A sequence: pointer on the app's top row, resting in the band,
    // the bar shown, Restore hovered, then down into the app.
    let mut g = page_group();
    lay_out(&mut g, &theme);
    let _ = paint(&mut g, &theme, 1.0);
    let top_row = Point::new(700.0, TOP + 22.0);
    g.note_maximized_pointer(top_row);
    let _ = g.elapse_maximized_chrome_intent();
    shoot(&mut g, 2.0, "seq-1-aiming-at-app-top-row", Some(top_row));
    let band = Point::new(1100.0, TOP + 2.0);
    g.note_maximized_pointer(band);
    g.note_maximized_pointer(Point::new(1101.0, TOP + 2.0));
    shoot(&mut g, 2.0, "seq-2-resting-in-band", Some(band));
    let _ = g.elapse_maximized_chrome_intent();
    shoot(&mut g, 2.0, "seq-3-bar-shown", Some(band));
    let restore = g.presentation_bounds().restore.expect("shown");
    g.note_maximized_pointer(restore.center());
    g.restore_hover = true;
    shoot(&mut g, 2.0, "seq-4-restore-hovered", Some(restore.center()));
    g.restore_hover = false;
    let below = Point::new(restore.center().x, 400.0);
    g.note_maximized_pointer(Point::new(restore.center().x, 300.0));
    g.note_maximized_pointer(below);
    shoot(&mut g, 2.0, "seq-5-back-in-app", Some(below));

    for (header_in_bar, name) in [
        (true, "notes-header-in-bar"),
        (false, "files-permanent-header"),
    ] {
        let mut g = group(TileContent::App(Box::new(HeaderedApp::new(header_in_bar))));
        lay_out(&mut g, &theme);
        shoot(&mut g, 2.0, &format!("{name}-rest"), None);
        g.set_maximized_chrome_shown_for_tests(true);
        lay_out(&mut g, &theme);
        let _ = paint(&mut g, &theme, 2.0);
        shoot(
            &mut g,
            2.0,
            &format!("{name}-revealed"),
            Some(Point::new(900.0, TOP + 2.0)),
        );
    }
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
