use super::clock::clock_overlay_controls;
use super::*;
use automerge::AutoCommit;
use tilo_desktop_model::{
    DesktopTree, Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};

#[test]
fn compact_workspace_identity_outranks_redundant_position() {
    let mut bar = sample_chrome();
    bar.workspace_name = "Home".into();
    bar.workspace_total = 1;
    bar.workspace_index = 0;
    let mut tr = TextRenderer::new();
    let label = bar.workspace_label(&mut tr, &Theme::default().text(TextRole::Label), 112.0);
    assert!(label.contains("Home"), "{label}");
}

#[test]
fn desktop_topbar_uses_scaled_tokens_once_and_contains_workspace_text() {
    let theme = Theme::dark().with_text_scale(2.0);
    let mut bar = sample_chrome();
    bar.workspace_name = "Home".into();
    bar.rect = Rect::new(0.0, 0.0, 960.0, 48.0);
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(2.0);
    let mut scene = Scene::new();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        &theme,
        &PresenceCtx::default(),
    );
    let runs = &scene.encoding().resources.glyph_runs;
    assert!(!runs.is_empty());
    assert!(
        runs.iter()
            .all(|run| run.font_size <= theme.tokens.typography.size(TextRole::Heading))
    );
    assert_eq!(renderer.text_scale(), 2.0);
    renderer.set_text_scale(1.0);
    // Measured in the style the pill draws its label in.
    let (_, height) = renderer.measure_style(
        "Home \u{203a} Main",
        None,
        &theme
            .tokens
            .typography
            .style(TextRole::Caption)
            .with_weight(bar.workspace_name_text_weight()),
    );
    assert!(f64::from(height) <= bar.ws_pill_rect.height());
    assert!(bar.ws_pill_rect.x1 <= bar.rect.center().x - bar.center_band_width(&theme) / 2.0);
}

fn sample_chrome() -> TopBarChrome {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    TopBarChrome::from_tree(&tree)
}

#[test]
fn time_bucket_wait_runs_to_the_next_whole_bucket() {
    let minute_in = Duration::from_secs(60 * 1_000 + 12) + Duration::from_millis(500);
    assert_eq!(
        until_next_time_bucket(minute_in, 60),
        Duration::from_millis(47_500)
    );
    assert_eq!(
        until_next_time_bucket(Duration::from_secs(60 * 1_000), 60),
        Duration::from_secs(60),
        "a reading taken on the boundary holds for the whole next minute"
    );
    assert_eq!(
        until_next_time_bucket(Duration::from_millis(4_250), 1),
        Duration::from_millis(750)
    );
}

/// The clock is read only when drawing, so an idle desktop repaints it
/// solely through the bar's wake deadline at the end of the minute.
#[test]
fn idle_clock_wakes_and_repaints_when_the_minute_turns() {
    let mut bar = sample_chrome();
    assert_eq!(
        bar.next_wake_deadline(),
        None,
        "nothing drawn, nothing stale"
    );
    assert!(!bar.update(&(), &()));

    // The bar's clock stands still, so the minute turns only when the
    // test says it did.
    let now = ui::time::now();
    ui::time::testing::with_monotonic_time(now, || {
        lay_out_and_draw(&mut bar, 1200.0);
        let wake = bar
            .next_wake_deadline()
            .expect("a drawn clock wakes when its minute ends");
        assert!(wake > now && wake <= now + Duration::from_secs(60));
        assert!(!bar.update(&(), &()), "the minute has not turned yet");

        let overdue = now
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock is past its origin");
        bar.time_wake = Some(overdue);
        assert!(
            bar.next_wake_deadline().is_some_and(|due| due <= now),
            "a passed wake stays due, so the host runs the frame that turns the clock"
        );
        assert!(
            bar.update(&(), &()),
            "the turned minute marks the bar dirty so the frame repaints it"
        );
        assert!(
            bar.next_wake_deadline().is_some_and(|next| next > now),
            "the next minute is scheduled"
        );
        assert!(!bar.update(&(), &()), "one repaint per minute");
    });
}

fn floating_clock() -> TopBarChrome {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    {
        let mut state = chrome.borrow_mut();
        state.overlays.clock = true;
        state.clock_activity = Some(ClockActivity {
            label: "Tea".into(),
            remaining_secs: 125,
            overlay: true,
            ..Default::default()
        });
    }
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
    bar.clock_overlay_rect = Rect::new(100.0, 80.0, 352.0, 238.0);
    [
        bar.clock_pause_rect,
        bar.clock_add_rect,
        bar.clock_close_rect,
    ] = clock_overlay_controls(bar.clock_overlay_rect);
    bar
}

#[test]
fn floating_clock_cursor_exposes_buttons_and_drag_state() {
    let mut bar = floating_clock();
    assert_eq!(
        bar.cursor_shape(bar.clock_pause_rect.center()),
        Some(winit::window::CursorIcon::Pointer)
    );
    let header = Point::new(
        bar.clock_overlay_rect.x0 + 20.0,
        bar.clock_overlay_rect.y0 + 20.0,
    );
    assert_eq!(
        bar.cursor_shape(header),
        Some(winit::window::CursorIcon::Grab)
    );
    assert!(bar.begin_clock_drag_at(header));
    assert_eq!(
        bar.cursor_shape(Point::new(900.0, 700.0)),
        Some(winit::window::CursorIcon::Grabbing)
    );
}

#[test]
fn floating_clock_accessibility_announces_time_and_focus() {
    let mut bar = floating_clock();
    bar.clock_control_focus = Some(1);
    let node = bar.clock_accessibility_node().expect("visible timer node");
    assert_eq!(node.name.as_deref(), Some("Floating timer"));
    assert!(
        node.description
            .as_deref()
            .is_some_and(|text| text.contains("Alt"))
    );
    assert_eq!(node.children[0].name.as_deref(), Some("Tea"));
    assert_eq!(node.children[0].value.as_deref(), Some("2:05"));
    assert!(node.children[2].state.focused);
}

fn lay_out_and_draw_with_theme(bar: &mut TopBarChrome, width: f32, theme: &Theme) {
    lay_out_and_draw_with_text(bar, width, theme, &mut TextRenderer::new());
}

/// [`lay_out_and_draw_with_theme`] with `text_renderer`, which a test
/// comparing two draws keeps: its fonts are what the glyphs refer to.
pub(super) fn lay_out_and_draw_with_text(
    bar: &mut TopBarChrome,
    width: f32,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
) {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, theme, &mut node_map);
    let viewport = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(width, TOPBAR_HEIGHT_PX),
                ..Default::default()
            },
            &[node],
        )
        .expect("viewport");
    taffy
        .compute_layout_with_measure(
            viewport,
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("layout");
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    bar.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        text_renderer,
        theme,
        &PresenceCtx::default(),
    );
}

pub(super) fn lay_out_and_draw(bar: &mut TopBarChrome, width: f32) {
    lay_out_and_draw_with_theme(bar, width, &Theme::default());
}

/// Lay `bar` out at the top of a 1280x800 column whose other child
/// stands for the windows, as the desktop surface stacks them; returns
/// where the windows start.
fn lay_out_over_windows(bar: &mut TopBarChrome) -> f32 {
    let theme = Theme::default();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    let windows = taffy
        .new_leaf(Style {
            flex_grow: 1.0,
            ..Default::default()
        })
        .expect("windows");
    let column = taffy
        .new_with_children(
            Style {
                flex_direction: taffy::FlexDirection::Column,
                size: Size::from_lengths(1280.0, 800.0),
                ..Default::default()
            },
            &[node, windows],
        )
        .expect("column");
    taffy
        .compute_layout_with_measure(
            column,
            Size {
                width: taffy::AvailableSpace::Definite(1280.0),
                height: taffy::AvailableSpace::Definite(800.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .expect("layout");
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    taffy.layout(windows).expect("windows laid out").location.y
}

fn painted(bar: &mut TopBarChrome) -> bool {
    let mut scene = Scene::new();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::default(),
        &PresenceCtx::default(),
    );
    !scene.encoding().is_empty()
}

fn click(bar: &mut TopBarChrome, at: Point) -> bool {
    dispatch_topbar_event(bar, &ui::synthetic_input::cursor_moved(at.x, at.y));
    let pressed = dispatch_topbar_event(bar, &ui::synthetic_input::mouse_press());
    let released = dispatch_topbar_event(bar, &ui::synthetic_input::mouse_release());
    pressed || released
}

/// The person's "hide the top bar": the windows take its strip at once;
/// hidden, it paints nothing and every press under it goes to what is
/// below; shown, it stands over the windows in its docked place,
/// painted and pressable, and nothing under it moves either way.
#[test]
fn a_hidden_bar_gives_its_strip_to_the_windows_and_shows_over_them_in_place() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());

    assert_eq!(lay_out_over_windows(&mut bar), TOPBAR_HEIGHT_PX, "docked");
    assert!(painted(&mut bar));
    let docked = bar.rect;
    let menu = bar.ws_pill_rect.center();

    chrome.borrow_mut().top_bar = TopBarPresence::Hidden;
    assert!(bar.is_layout_dirty(), "hiding re-lays the column out");
    assert!(
        bar.take_structure_dirty(),
        "and rebuilds the tree the bar stands in"
    );
    assert_eq!(
        lay_out_over_windows(&mut bar),
        0.0,
        "the windows take the strip"
    );
    assert!(!bar.is_layout_dirty());
    assert_eq!(bar.rect, docked, "it keeps its place for when it shows");
    assert!(!painted(&mut bar), "hidden paints nothing");
    assert!(!click(&mut bar, menu), "hidden takes no press");
    assert!(chrome.borrow().intents.is_empty());

    chrome.borrow_mut().top_bar = TopBarPresence::Shown;
    assert!(bar.needs_redraw(), "showing repaints the bar");
    assert!(
        !bar.is_layout_dirty() && !bar.take_structure_dirty(),
        "showing over the windows changes no layout: nothing moves"
    );
    assert_eq!(lay_out_over_windows(&mut bar), 0.0);
    assert!(painted(&mut bar));
    assert_eq!(bar.ws_pill_rect.center(), menu, "same place as docked");
    assert!(click(&mut bar, menu), "shown, its controls work");
    assert_eq!(
        chrome.borrow().intents.as_slice(),
        &[ChromeIntent::ToggleWorkspaceMenu]
    );

    chrome.borrow_mut().top_bar = TopBarPresence::Hidden;
    assert!(bar.needs_redraw(), "hiding again clears it from the screen");
    assert!(!painted(&mut bar));
}

/// Do Not Disturb is shown in the status cluster, as the bell silenced
/// in its own slot, not as a card over the person's work.
#[test]
fn quiet_mode_silences_the_bell_in_the_status_cluster() {
    let mut bar = sample_chrome();
    bar.status.link = Some(crate::chrome::LinkStatus {
        kind: crate::chrome::LinkKind::Wired,
        name: None,
        signal: 0,
    });
    lay_out_and_draw(&mut bar, 1280.0);
    let loud = bar.tray_pill_rect;
    bar.bell.quiet = true;
    lay_out_and_draw(&mut bar, 1280.0);
    let quiet = bar.tray_pill_rect;
    assert_eq!(quiet, loud, "the bell's slot was there all along");
    bar.bell.unread = 2;
    let node = bar.accessibility().expect("topbar semantics");
    let tray = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some(TopBarChrome::CONTROL_QUICK_SETTINGS))
        .expect("the status cluster");
    assert_eq!(
        tray.description.as_deref(),
        Some("Notifications silenced, 2 unread"),
        "the silenced bell is named"
    );
    assert!(
        tray.children
            .iter()
            .any(|bell| bell.name.as_deref() == Some("Notifications silenced, 2 unread"))
    );
}

/// The unread count on the bell's shoulder stays whole inside the bar,
/// never cut off at its top edge, whatever the count and text size.
#[test]
fn the_unread_count_stays_inside_the_bar() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        for unread in [1, 42, 5000] {
            let tree = DesktopTree {
                active: WorkspaceId(1),
                workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
                ..DesktopTree::default()
            };
            let chrome = fixture::FixtureChrome::new(tree.clone());
            chrome.borrow_mut().notifications_only = true;
            chrome.borrow_mut().notification_badge = unread;
            let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
            lay_out_and_draw_with_theme(&mut bar, 1440.0, &theme);
            let badge = bar.bell_badge_rect;
            assert!(
                !badge.is_zero_area(),
                "{unread} unread at {scale}x: a badge"
            );
            assert!(
                badge.y0 >= bar.rect.y0
                    && badge.y1 <= bar.rect.y1
                    && badge.x0 >= bar.rect.x0
                    && badge.x1 <= bar.rect.x1,
                "{unread} unread at {scale}x: the badge {badge:?} is inside the bar {:?}",
                bar.rect
            );
        }
    }
}

/// Every host draws the bell in the status cluster, quiet with nothing
/// unread, so both trays are one design. A host without quick settings
/// (the web) names the cluster "Notifications", since that is all it
/// opens.
#[test]
fn without_quick_settings_the_status_cluster_is_the_notifications_bell() {
    let mut bar = sample_chrome();
    lay_out_and_draw(&mut bar, 1280.0);
    let desktop = bar.tray_pill_rect;
    assert_eq!(bar.tray_slot_count(), 1, "the bell, with nothing unread");
    bar.bell.only = true;
    lay_out_and_draw(&mut bar, 1280.0);
    assert_eq!(bar.tray_slot_count(), 1, "the same bell");
    assert_eq!(bar.tray_pill_rect, desktop, "one tray on both hosts");
    let node = bar.accessibility().expect("topbar semantics");
    let tray = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some(TopBarChrome::CONTROL_QUICK_SETTINGS))
        .expect("the status cluster");
    assert_eq!(tray.name.as_deref(), Some("Notifications"));
}

#[test]
fn persistent_topbar_controls_have_coherent_targets() {
    let mut bar = sample_chrome();
    lay_out_and_draw(&mut bar, 1280.0);
    assert!(bar.ws_pill_rect.height() >= BAR_CONTROL_PX);
    assert!(bar.tray_pill_rect.height() >= BAR_CONTROL_PX);
    assert!(bar.center_rect.height() >= BAR_CONTROL_PX);
}

/// HIG §9.6: every pressable thing in the bar shares one height, so
/// the pills and buttons sit on one top edge and one bottom edge,
/// centred on the bar.
#[test]
fn bar_controls_share_one_height_and_centre_line() {
    let mut bar = sample_chrome();
    lay_out_and_draw(&mut bar, 1280.0);
    let reference = bar.ws_pill_rect;
    let controls = [
        ("workspace pill", bar.ws_pill_rect),
        ("centre band", bar.center_rect),
        ("tray pill", bar.tray_pill_rect),
    ];
    for (name, r) in controls {
        assert!(
            (r.height() - BAR_CONTROL_PX).abs() < 0.01,
            "{name} is {} px tall, not the shared {BAR_CONTROL_PX}",
            r.height()
        );
        assert!(
            (r.y0 - reference.y0).abs() < 0.01 && (r.y1 - reference.y1).abs() < 0.01,
            "{name} {r:?} does not share the workspace pill's edges {reference:?}"
        );
    }
    let bar_centre = f64::midpoint(bar.rect.y0, bar.rect.y1);
    assert!(
        (reference.center().y - bar_centre).abs() < 0.01,
        "controls centre at {} in a bar centred on {bar_centre}",
        reference.center().y
    );
    // The clock is the time on the bar: no pill, and nothing to press.
    assert!(bar.center_is_plain_text());
}

/// The workspace capsule pads its label on the left exactly as much
/// as it pads the chevron on the right, and the chevron sits on the
/// pill's centre line — so the pill reads centred on its contents.
#[test]
fn workspace_pill_pads_its_label_and_chevron_symmetrically() {
    let mut bar = sample_chrome();
    let theme = Theme::default();
    lay_out_and_draw_with_theme(&mut bar, 1280.0, &theme);
    let pad = f64::from(theme.tokens.spacing.s2);
    // Without the mark the pill leads the bar, `s3` in from its edge;
    // its label starts one `s2` inside it.
    let edge = f64::from(theme.tokens.spacing.s3);
    assert!(
        (bar.ws_pill_rect.x0 - bar.rect.x0 - edge).abs() < 0.01,
        "the pill stands {} in from the bar's edge, not {edge}",
        bar.ws_pill_rect.x0 - bar.rect.x0
    );
    let text_x = bar.rect.x0 + edge + pad;
    assert!(
        (text_x - bar.ws_pill_rect.x0 - pad).abs() < 0.01,
        "leading pad is {} not {pad}",
        text_x - bar.ws_pill_rect.x0
    );
    // A bar control's width rounds up to a whole pixel, so the trailing
    // pad may be up to a pixel wider, never narrower.
    let trailing = bar.ws_pill_rect.x1 - bar.ws_chevron_rect.x1;
    assert!(
        trailing >= pad - 0.01 && trailing < pad + 1.0,
        "trailing pad is {trailing} not {pad}",
    );
    assert!(
        (bar.ws_chevron_rect.center().y - bar.ws_pill_rect.center().y).abs() < 0.01,
        "chevron {:?} is off the pill's centre line {:?}",
        bar.ws_chevron_rect,
        bar.ws_pill_rect
    );
}

/// The bar's density constants are the spacing scale under other
/// names; this pins them so they cannot drift off it unnoticed.
#[test]
fn bar_density_sits_on_the_spacing_scale() {
    let tokens = Theme::default().tokens;
    let spacing = tokens.spacing;
    assert_eq!(
        BAR_CONTROL_PX,
        f64::from(tokens.metrics.bar_control_height - spacing.s1)
    );
    assert_eq!(CENTER_INSET_PX, f64::from(spacing.s3));
    assert_eq!(TOPBAR_HEIGHT_PX, 32.0);
    assert_eq!(TOPBAR_HEIGHT_PX, BAR_CONTROL_LOGICAL + 2.0 * spacing.s1);
    assert!(TOPBAR_HEIGHT_PX < tokens.metrics.bar_height);
}

/// At the default text size the bar is 32 px; a large text size grows
/// it so the time's line never spills out of it.
#[test]
fn the_bar_grows_with_large_text_and_not_before() {
    assert_eq!(topbar_height(&Theme::default()), TOPBAR_HEIGHT_PX);
    for scale in [1.25_f32, 1.5, 2.0] {
        let theme = Theme::default().with_text_scale(scale);
        let line = theme
            .tokens
            .typography
            .style(TextRole::Body)
            .line_height_px();
        let height = topbar_height(&theme);
        assert!(
            line < height,
            "at {scale}x the time's {line} px line fits the {height} px bar"
        );
    }
}

#[test]
fn large_text_activity_stays_single_line_inside_center_content_lane() {
    let mut bar = sample_chrome();
    bar.center_rect = Rect::new(460.0, 7.0, 820.0, 41.0);
    bar.activity_headline = Some("Recorded 3 workspaces: Home, Research and Design".into());
    bar.activity_unread = 3;
    let mut renderer = TextRenderer::new();
    let theme = Theme::default().with_text_scale(1.5);
    let style = theme.text(TextRole::Caption);
    let max_width = bar.activity_content_width(&theme);
    let label = bar.activity_display_label(&mut renderer, &style, max_width);
    let (_, height) =
        renderer.measure_style(&label, None, &style.clone().with_weight(TextWeight::Medium));
    let (width, _) =
        renderer.measure_style(&label, None, &style.clone().with_weight(TextWeight::Medium));
    assert!(
        !label.contains('\n'),
        "activity headline must never wrap in the fixed topbar pill: {label:?}"
    );
    assert!(
        width <= max_width + 0.5,
        "activity headline width {width} exceeds reserved center lane {max_width}"
    );
    assert!(
        height <= topbar_height(&theme) - 4.0,
        "activity headline height {height} must fit in the compact topbar"
    );
}

#[test]
fn compact_large_text_preserves_the_full_activity_sentence_and_detail_route() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    // The 960px chrome reserves a 360px centre selector; after its dots
    // and breathing room, this is the usable activity lane.
    bar.center_rect = Rect::new(300.0, 7.0, 660.0, 41.0);
    bar.activity_headline = Some("Quinten · 2 permission changes".into());
    bar.activity_unread = 4;
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        ui::font_catalog::checkout_root(),
        "en-US",
    );
    let mut renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
    let theme = Theme::default().with_text_scale(1.3);
    let style = theme.text(TextRole::Caption);
    let max_width = bar.activity_content_width(&theme);
    let label = bar.activity_display_label(&mut renderer, &style, max_width);
    let (width, _) =
        renderer.measure_style(&label, None, &style.clone().with_weight(TextWeight::Medium));
    // The headline alone, whole: the unread count is the badge's.
    assert!(
        label.starts_with("Quinten · 2 permission changes"),
        "the full sentence survives: {label:?}"
    );
    assert!(width <= max_width + 0.5);

    bar.activity_started = Some(ui::time::now());
    bar.last_cursor = Some(bar.center_rect.center());
    let mut handled = false;
    bar.handle_left_click(&mut handled);
    assert!(handled);
    assert!(
        matches!(
            chrome.borrow().intents.last(),
            Some(crate::chrome::ChromeIntent::ShowActivity(_))
        ),
        "the explicit chevron label points to the activity detail route"
    );
    assert!(!chrome.borrow().overlays.room_activity);
}

fn press_named_key(bar: &mut TopBarChrome, key: winit::keyboard::NamedKey) -> bool {
    let key_event = ui::synthetic_input::key_event(
        winit::keyboard::Key::Named(key),
        None,
        winit::event::ElementState::Pressed,
    );
    let event = ui::synthetic_input::keyboard_input(key_event);
    let mut handled = false;
    bar.handle_keyboard(&event, &mut handled);
    handled
}

fn dispatch_topbar_event(bar: &mut TopBarChrome, event: &WindowEvent) -> bool {
    dispatch_topbar_event_with_modifiers(bar, event, winit::event::Modifiers::default())
}

fn dispatch_topbar_event_with_modifiers(
    bar: &mut TopBarChrome,
    event: &WindowEvent,
    modifiers: winit::event::Modifiers,
) -> bool {
    let mut taffy = TaffyTree::new();
    let node = taffy.new_leaf(Style::default()).expect("test leaf");
    let mut state = AutoCommit::new();
    let mut handled = false;
    let mut focus = None;
    bar.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: ui::widget::Edits::lend(&mut state),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
}

fn two_workspace_tree() -> DesktopTree {
    let first = Workspace::new(WorkspaceId(1), "A");
    let second = Workspace::new(WorkspaceId(2), "B");
    let active = first.id;
    DesktopTree {
        workspaces: vec![first, second],
        active,
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    }
}

/// Your colour rings a top-bar control only for keyboard focus. Clicking
/// the bar used to light its first stop in your colour, though no
/// keyboard was involved. Focus the shell merely placed on the bar (its
/// first focusable control, while a window has the keyboard) is not
/// keyboard use either: it shows no ring and holds no auto-hiding bar
/// shown, or a maximized window's bar stayed up for as long as nobody
/// pressed a key.
#[test]
fn the_focus_ring_follows_the_keyboard_not_a_click() {
    let tab = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
        None,
        winit::event::ElementState::Pressed,
    ));
    let mut bar = sample_chrome();
    bar.set_focus(true);
    assert!(
        !bar.focus_ring_visible() && !bar.holds_itself_shown(),
        "focus placed with no input is nobody reading the bar"
    );
    bar.set_focus(false);

    let _ = dispatch_topbar_event(&mut bar, &tab);
    bar.set_focus(true);
    assert!(
        bar.focus_ring_visible() && bar.holds_itself_shown(),
        "keyboard traversal into the bar shows it"
    );

    let press = ui::synthetic_input::mouse_input(
        winit::event::ElementState::Pressed,
        winit::event::MouseButton::Left,
    );
    let _ = dispatch_topbar_event(&mut bar, &press);
    assert!(bar.is_focused());
    assert!(
        !bar.focus_ring_visible(),
        "a click on the bar shows no ring"
    );

    let key = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowRight),
        None,
        winit::event::ElementState::Pressed,
    ));
    let _ = dispatch_topbar_event(&mut bar, &key);
    assert!(bar.focus_ring_visible(), "a key press brings it back");

    let _ = dispatch_topbar_event(&mut bar, &press);
    bar.set_focus(false);
    let _ = dispatch_topbar_event(&mut bar, &tab);
    bar.set_focus(true);
    assert!(
        bar.focus_ring_visible(),
        "focus tabbed into afresh starts unpointed"
    );

    // A click, then Escape (closing the menu the click opened): Escape
    // moves no focus, so no ring lights where the click left focus.
    let _ = dispatch_topbar_event(&mut bar, &press);
    assert!(!bar.focus_ring_visible());
    let escape = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        winit::event::ElementState::Pressed,
    ));
    let _ = dispatch_topbar_event(&mut bar, &escape);
    assert!(
        !bar.focus_ring_visible(),
        "Escape after a click leaves no focus ring behind"
    );
}

/// A running application's icon stands on the bar as its own control:
/// it is in the focus order, a reader names it with the name the
/// application chose, and Enter does what a click does.
#[test]
fn an_applications_tray_icon_is_a_named_control_the_keyboard_reaches() {
    let workspace = Workspace::new(WorkspaceId(1), "A");
    let tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    lay_out_and_draw(&mut bar, 1280.0);
    assert!(
        !bar.control_targets()
            .iter()
            .any(|target| target.starts_with(TopBarChrome::APP_TRAY_TARGET)),
        "no running application, no tray control"
    );

    let key = "org.keepassxc.KeePassXC/StatusNotifierItem";
    let rect = Rect::new(1100.0, 4.0, 1132.0, 36.0);
    chrome
        .borrow_mut()
        .app_badges
        .insert("org.keepassxc.KeePassXC".into(), 3);
    bar.app_tray.push(AppTraySlot {
        key: key.to_string(),
        id: "keepassxc".into(),
        owner_app: Some("org.keepassxc.KeePassXC".into()),
        title: "KeePassXC".into(),
        item_is_menu: false,
        rect,
    });
    let target = TopBarChrome::tray_target(key);
    assert!(
        bar.control_targets().contains(&target),
        "the icon is a bar control"
    );
    assert_eq!(bar.control_name(&target), "KeePassXC");
    assert_eq!(bar.control_rect(&target), rect);

    let node = bar.accessibility().expect("topbar semantics");
    let tray = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some(target.as_str()))
        .expect("a reader finds the icon among the bar's buttons");
    assert_eq!(tray.name.as_deref(), Some("KeePassXC"));
    assert_eq!(
        tray.description.as_deref(),
        Some("3 unread"),
        "the icon says whose the bell's total is, not just how many there are"
    );
    assert!(tray.state.focusable);

    // Keyboard: the bar's last stop is the icon, and Enter activates it.
    bar.set_focus(true);
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::End));
    assert_eq!(bar.focused_control.as_deref(), Some(target.as_str()));
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
    assert_eq!(
        chrome.borrow().intents.last(),
        Some(&ChromeIntent::AppTray {
            key: key.to_string(),
            request: AppTrayRequest::Activate { x: 1116, y: 20 },
        }),
        "Enter does what a click on the icon does"
    );
}

#[test]
fn keyboard_navigation_is_one_focus_stop_with_optional_controls() {
    let mut bar = sample_chrome();
    bar.collaborator_initials.push('A');
    bar.media_indicator_rect = Rect::new(1120.0, 8.0, 1152.0, 40.0);
    bar.set_focus(true);

    assert!(bar.is_focusable());
    assert!(bar.is_focused());
    assert_eq!(
        bar.focused_control.as_deref(),
        Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
    );

    let second = bar.control_targets()[1].clone();
    assert!(press_named_key(
        &mut bar,
        winit::keyboard::NamedKey::ArrowRight
    ));
    assert_eq!(bar.focused_control.as_deref(), Some(second.as_str()));
    assert!(press_named_key(
        &mut bar,
        winit::keyboard::NamedKey::ArrowLeft
    ));
    assert_eq!(
        bar.focused_control.as_deref(),
        Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
    );
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::End));
    assert_eq!(
        bar.focused_control.as_deref(),
        Some(TopBarChrome::CONTROL_MEDIA)
    );
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Home));
    assert_eq!(
        bar.focused_control.as_deref(),
        Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
    );

    // Removing an optional target leaves the focus stop intact and End
    // resolves against the newly published control set.
    bar.media_indicator_rect = Rect::ZERO;
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::End));
    assert_eq!(
        bar.focused_control.as_deref(),
        Some(TopBarChrome::CONTROL_COLLABORATORS)
    );
}

#[test]
fn keyboard_activation_matches_pointer_actions() {
    let tree = two_workspace_tree();
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    bar.set_focus(true);

    bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS.to_string());
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Space));
    assert_eq!(
        chrome.borrow().intents.last(),
        Some(&ChromeIntent::ToggleWorkspaceMenu)
    );
}

/// A workspace with two named strips, the second shown.
fn strips_tree() -> DesktopTree {
    let mut tree = two_workspace_tree();
    tree.workspaces[0].strips = vec![
        tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(10), "Lounge"),
        tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(11), "Research"),
    ];
    let active = tree.active;
    tree.active_strip
        .insert(active, tilo_desktop_model::StripId(11));
    tree
}

/// The left end names where you are: the workspace, then the strip
/// shown. The strip's name is its own target and opens the strip
/// switcher; the workspace's pill opens the workspace menu.
#[test]
fn the_bar_names_the_strip_after_the_workspace_and_it_opens_the_strip_switcher() {
    let tree = strips_tree();
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    lay_out_and_draw(&mut bar, 1280.0);
    let targets = bar.control_targets();
    assert_eq!(
        &targets[..2],
        &[
            TopBarChrome::CONTROL_WORKSPACE_SETTINGS,
            TopBarChrome::CONTROL_STRIP_NAME,
        ],
        "workspace, strip: nothing else at the left end"
    );
    assert_eq!(bar.strip_name(), Some("Research"));

    let strip_rect = bar.control_rect(TopBarChrome::CONTROL_STRIP_NAME);
    assert!(strip_rect.width() > 0.0);
    assert!(
        strip_rect.x0 >= bar.ws_pill_rect.x1,
        "the strip follows the workspace's pill"
    );
    assert!(click(&mut bar, strip_rect.center()));
    assert_eq!(
        chrome.borrow().intents.last(),
        Some(&ChromeIntent::ShowStrips)
    );
    let pill = bar.ws_pill_rect.center();
    assert!(click(&mut bar, pill));
    assert_eq!(
        chrome.borrow().intents.last(),
        Some(&ChromeIntent::ToggleWorkspaceMenu)
    );
}

/// A strip without a name is not named: the pill stands alone.
#[test]
fn an_unnamed_strip_leaves_the_workspace_pill_alone() {
    let mut tree = strips_tree();
    tree.workspaces[0].strips[1].name = String::new();
    let mut bar = TopBarChrome::from_tree(&tree);
    lay_out_and_draw(&mut bar, 1280.0);
    assert_eq!(bar.strip_name(), None);
    assert!(
        !bar.control_targets()
            .contains(&TopBarChrome::CONTROL_STRIP_NAME.to_string())
    );
    assert!(bar.ws_strip_rect.is_zero_area());
}

/// Scrolling over the top of the screen changes nothing: the bar keeps
/// the time, and a wheel there is not the bar's.
#[test]
fn a_wheel_over_the_time_changes_nothing() {
    let tree = two_workspace_tree();
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    lay_out_and_draw(&mut bar, 1280.0);
    let clock = bar.center_rect.center();
    dispatch_topbar_event(
        &mut bar,
        &ui::synthetic_input::cursor_moved(clock.x, clock.y),
    );
    let before = scene_fingerprint(&mut bar);
    for delta in [3.0, -7.0, 12.0] {
        assert!(
            !dispatch_topbar_event(&mut bar, &ui::synthetic_input::mouse_wheel(0.0, delta)),
            "the bar does not take the wheel"
        );
    }
    assert!(!bar.needs_redraw());
    assert_eq!(scene_fingerprint(&mut bar), before);
    assert!(bar.center_is_plain_text(), "still the time");
    assert!(chrome.borrow().intents.is_empty());
}

/// What the bar paints, to compare: its path data and how many draws
/// and glyph runs.
fn scene_fingerprint(bar: &mut TopBarChrome) -> (Vec<u32>, usize, usize) {
    let mut scene = Scene::new();
    bar.cache_fp = CacheFingerprint::default();
    bar.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::default(),
        &PresenceCtx::default(),
    );
    let encoding = scene.encoding();
    (
        encoding.path_data.clone(),
        encoding.draw_tags.len(),
        encoding.resources.glyph_runs.len(),
    )
}

#[test]
fn accessibility_children_have_stable_semantics_and_synthetic_parity() {
    let tree = two_workspace_tree();
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
    lay_out_and_draw(&mut bar, 1280.0);
    bar.set_focus(true);
    bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS.to_string());

    let node = bar.accessibility().expect("topbar semantics");
    assert_eq!(node.role, ui::a11y::Role::Toolbar);
    assert_eq!(node.semantic_key.as_deref(), Some("topbar"));
    assert_eq!(node.name.as_deref(), Some("Desktop navigation"));
    assert!(node.state.focusable);
    assert!(node.state.focused);

    for target in bar.control_targets() {
        let child = node
            .children
            .iter()
            .find(|child| child.action_target.as_deref() == Some(target.as_str()))
            .unwrap_or_else(|| panic!("missing semantic child {target}"));
        let expected_name = bar.control_name(&target);
        assert_eq!(child.name.as_deref(), Some(expected_name.as_str()));
        assert!(child.state.focusable);
        assert!(child.bounds.is_some(), "{target} needs a hit target");
        let bounds = child.bounds.expect("checked above").to_rect();
        assert!(
            bounds.width() > 0.0 && bounds.height() > 0.0,
            "{target}: {bounds:?}"
        );
        assert_eq!(child.semantic_key.as_deref(), Some(target.as_str()));
    }

    let workspace = node
        .children
        .iter()
        .find(|child| {
            child.action_target.as_deref() == Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
        })
        .expect("workspace semantics");
    assert_eq!(workspace.role, ui::a11y::Role::Button);
    assert!(workspace.state.focused);
    assert_eq!(
        workspace.description.as_deref(),
        Some("Workspace 1 of 2"),
        "the position is said, not painted"
    );
    let clock = node
        .children
        .iter()
        .find(|child| child.role == ui::a11y::Role::StaticText)
        .expect("the time is read out");
    assert_eq!(clock.action_target, None, "the time is not a control");

    let index = node
        .children
        .iter()
        .position(|child| {
            child.action_target.as_deref() == Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
        })
        .expect("workspace child index");
    assert_eq!(
        bar.synthetic_accessibility_rect(&[index]),
        workspace.bounds.map(ui::a11y::AccessBounds::to_rect)
    );
    bar.focused_control = None;
    assert!(bar.focus_synthetic_accessibility(&[index]));
    assert_eq!(
        bar.focused_control.as_deref(),
        Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS)
    );
}

#[test]
fn the_tilo_mark_is_opt_in_leads_the_bar_and_asks_for_its_menu() {
    let tree = two_workspace_tree();
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut plain = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    lay_out_and_draw(&mut plain, 1280.0);
    assert!(
        !plain
            .control_targets()
            .contains(&TopBarChrome::CONTROL_TILO_MARK.to_string())
    );
    assert_eq!(plain.tilo_mark_rect(), Rect::ZERO);

    let mut bar = TopBarChrome::from_tree(&tree)
        .with_chrome(chrome.clone())
        .with_tilo_mark();
    lay_out_and_draw(&mut bar, 1280.0);
    assert_eq!(
        bar.control_targets().first().map(String::as_str),
        Some(TopBarChrome::CONTROL_TILO_MARK)
    );
    let mark = bar.tilo_mark_rect();
    assert!(
        mark.width() >= BAR_CONTROL_PX && mark.height() >= BAR_CONTROL_PX,
        "{mark:?}"
    );
    assert!(mark.x1 <= bar.ws_pill_rect.x0, "the mark leads");
    assert!(
        bar.test_rects()
            .iter()
            .any(|(id, rect)| id == "topbar_tilo_mark" && *rect == mark)
    );
    let node = bar.accessibility().expect("topbar semantics");
    let child = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some(TopBarChrome::CONTROL_TILO_MARK))
        .expect("the mark is a semantic child");
    assert_eq!(child.name.as_deref(), Some("Tilo menu"));
    assert_eq!(child.role, ui::a11y::Role::Button);

    bar.last_cursor = Some(mark.center());
    let mut handled = false;
    bar.handle_left_click(&mut handled);
    assert!(handled);
    bar.set_focus(true);
    bar.focused_control = Some(TopBarChrome::CONTROL_TILO_MARK.to_string());
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
    let asked = chrome
        .borrow()
        .intents
        .iter()
        .filter(|intent| matches!(intent, ChromeIntent::ToggleTiloMenu))
        .count();
    assert_eq!(asked, 2, "a click and Enter each ask once");
}

/// The bar leads with the mark and the workspace's name as one menu,
/// shows desks only when there are several, and ends with the person's
/// own avatar opening their profile card.
#[test]
fn the_bar_leads_with_one_menu_and_ends_with_you() {
    let tree = two_workspace_tree();
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    chrome.borrow_mut().own = Some(crate::chrome::OwnChip {
        name: "Kim Lee".into(),
        rgb: (200, 120, 90),
        identity: String::new(),
    });
    let mut bar = TopBarChrome::from_tree(&tree)
        .with_chrome(chrome.clone())
        .with_tilo_mark();
    lay_out_and_draw(&mut bar, 1280.0);

    let targets = bar.control_targets();
    assert_eq!(
        targets[..2].iter().map(String::as_str).collect::<Vec<_>>(),
        [
            TopBarChrome::CONTROL_TILO_MARK,
            TopBarChrome::CONTROL_WORKSPACE_SETTINGS
        ]
    );
    assert!(targets.contains(&TopBarChrome::CONTROL_PROFILE.to_string()));
    let mark = bar.tilo_mark_rect();
    let menu = bar.workspace_menu_rect();
    let own = bar.own_avatar_rect();
    assert!(mark.x1 < menu.x0, "the workspace follows the mark");
    assert!(
        menu.x0 - mark.x1 <= f64::from(ui::tokens::Spacing::BASE.s3),
        "with nothing between them"
    );
    assert!(own.x1 < bar.tray_hit_rect.x0, "you, then quick settings");
    assert!(own.x0 > bar.center_rect.x1, "on the right");
    assert_eq!(bar.ws_chevron_rect.width(), SMALL_GLYPH_PX);
    let style = Theme::default().tokens.typography.style(TextRole::Caption);
    assert_eq!(
        bar.workspace_label(&mut TextRenderer::new(), &style, 400.0),
        "A",
        "no position count: the menu lists the others"
    );

    let node = bar.accessibility().expect("topbar semantics");
    let named = |target: &str| {
        node.children
            .iter()
            .find(|child| child.action_target.as_deref() == Some(target))
            .and_then(|child| child.name.clone())
    };
    assert_eq!(
        named(TopBarChrome::CONTROL_PROFILE).as_deref(),
        Some("Kim Lee (you): your profile")
    );
    assert!(
        named(TopBarChrome::CONTROL_WORKSPACE_SETTINGS).is_some_and(|name| name.starts_with("A:"))
    );

    for at in [menu.center(), own.center()] {
        bar.last_cursor = Some(at);
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        assert!(handled);
    }
    bar.set_focus(true);
    bar.focused_control = Some(TopBarChrome::CONTROL_WORKSPACE_SETTINGS.to_string());
    assert!(press_named_key(&mut bar, winit::keyboard::NamedKey::Enter));
    let intents = chrome.borrow().intents.clone();
    assert_eq!(
        intents,
        [
            ChromeIntent::ToggleWorkspaceMenu,
            ChromeIntent::ToggleProfile,
            ChromeIntent::ToggleWorkspaceMenu
        ]
    );
}

#[test]
fn long_workspace_name_yields_to_the_center_lane() {
    for width in [800.0, 1280.0, 1600.0] {
        let mut bar = sample_chrome();
        bar.workspace_name =
            "Überaus ausführlicher Arbeitsbereich für lokalisierte Forschungsunterlagen"
                .to_string();
        lay_out_and_draw(&mut bar, width);
        assert!(
            bar.ws_pill_rect.x1 + 12.0 <= bar.center_rect.x0,
            "workspace pill {:?} must not collide with centre {:?} at {width}px",
            bar.ws_pill_rect,
            bar.center_rect,
        );
    }
}

#[test]
fn narrow_topbar_releases_center_width_for_workspace_identity() {
    let mut short = sample_chrome();
    lay_out_and_draw(&mut short, 800.0);
    assert_eq!(short.center_lane.width(), 220.0);

    let mut long = sample_chrome();
    long.workspace_name =
        "Überaus ausführlicher Arbeitsbereich für lokalisierte Forschungsunterlagen".to_string();
    lay_out_and_draw(&mut long, 800.0);
    assert!(
        long.ws_pill_rect.width() > short.ws_pill_rect.width(),
        "narrow workspace identity should retain more than the position suffix: short={:?}, long={:?}",
        short.ws_pill_rect,
        long.ws_pill_rect,
    );
    assert!(
        long.ws_pill_rect.x1 + 12.0 <= long.center_rect.x0,
        "workspace pill {:?} must keep its breathing room from centre {:?}",
        long.ws_pill_rect,
        long.center_rect,
    );
}

#[test]
fn narrow_large_text_keeps_workspace_and_right_clusters_separate() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    tree.workspaces[0]
        .collaborators
        .push(tilo_desktop_model::Collaborator::new("alice", "Alice"));
    let mut bar = TopBarChrome::from_tree(&tree);
    let theme = Theme::default().with_text_scale(1.5);
    lay_out_and_draw_with_theme(&mut bar, 800.0, &theme);

    assert!(bar.center_rect.width() < 360.0);
    assert!(bar.ws_pill_rect.x1 + 12.0 <= bar.center_rect.x0);
    assert!(bar.center_rect.x1 + 12.0 <= bar.avatar_stack_rect.x0);
    assert!(bar.avatar_stack_rect.x1 + 8.0 <= bar.tray_pill_rect.x0);
}

/// The widget's content is centred in its band: equal insets either
/// side, so the time does not sit off the band's middle.
#[test]
fn center_content_is_centred_in_its_band() {
    let band = Rect::new(620.0, 2.0, 980.0, 22.0);
    let (x0, x1) = TopBarChrome::center_content_lane(band);
    assert!((f64::midpoint(x0, x1) - band.center().x).abs() < 1e-9);
    assert!(x0 > band.x0 && x1 < band.x1);
}

/// The clock's pill hugs the time instead of spanning the 360 px lane,
/// keeps the time on its centre and holds its width as the minutes
/// change; the bar's neighbours lay out against the lane.
#[test]
fn the_clock_pill_hugs_the_time_and_holds_its_width() {
    let pill_at = |clock: &str| {
        let mut bar = sample_chrome();
        bar.clock = ClockSnapshot {
            clock: clock.into(),
            ..ClockSnapshot::default()
        };
        lay_out_and_draw(&mut bar, 1600.0);
        (bar.center_rect, bar.center_lane, bar.ws_pill_rect)
    };
    let (clock, lane, ws) = pill_at("11:11");
    assert_eq!(lane.width(), 360.0);
    assert!(
        clock.width() < lane.width() / 2.0,
        "a time is not a 360 px pill: {clock:?}"
    );
    assert!((clock.center().x - lane.center().x).abs() < 1e-9);
    let (later, later_lane, later_ws) = pill_at("20:08");
    assert_eq!(
        later.width(),
        clock.width(),
        "the pill holds still minute to minute"
    );
    assert_eq!((later_lane, later_ws), (lane, ws), "neighbours do not move");
    let (x0, x1) = TopBarChrome::center_content_lane(clock);
    let mut text = TextRenderer::new();
    let theme = Theme::default();
    let (time, _) = text.measure_style(
        "11:11",
        None,
        &theme
            .tokens
            .typography
            .style(TextRole::Body)
            .with_weight(sample_chrome().clock_text_weight()),
    );
    assert!(x1 - x0 >= f64::from(time), "the time fits its content lane");
}

#[test]
fn new_activity_temporarily_shows_over_the_time() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());

    chrome.borrow_mut().publish_activity_glance(
        Some("Ada · Prepared launch brief".to_string()),
        3,
        42,
        Some(crate::chrome::ActivityFocus {
            author: Some("Ada".into()),
            group_id: Some("issue:42".into()),
            entry_id: None,
            resource_id: None,
            resource_version: None,
        }),
    );
    bar.refresh_status_and_media_state();
    assert!(bar.activity_is_visible());
    assert!(!bar.center_is_plain_text());

    chrome.borrow_mut().publish_activity_glance(
        Some("Ada · Prepared launch brief".to_string()),
        0,
        42,
        None,
    );
    bar.refresh_status_and_media_state();
    assert!(!bar.activity_is_visible());
    assert!(bar.center_is_plain_text(), "the time again");
}

/// The time and a live activity swap with the standard cross-fade
/// (`ui::motion::CrossFade`), which settles and then asks for nothing.
#[test]
fn the_time_and_a_live_activity_cross_fade_and_settle() {
    let motion = Theme::dark().motion();
    let now = ui::time::now();
    let mut fade = ui::motion::CrossFade::new(false);
    let mut bar = TopBarChrome::from_tree(&DesktopTree::default());
    bar.center_fade
        .show(true, motion, ui::tokens::MotionSource::Pointer, now);
    assert!(bar.needs_animation_frame(), "the swap animates");
    assert!(
        bar.center_fade_key().is_some(),
        "and redraws on each of its frames"
    );
    fade.show(true, motion, ui::tokens::MotionSource::Pointer, now);
    let settled = fade.settles_at().expect("cross-fading");
    assert!(fade.outgoing(settled).is_none());
    assert_eq!(fade.incoming(settled), 1.0);
    assert_eq!(
        bar.center_activity_opacity(settled),
        1.0,
        "the pill shows with it"
    );
}

// The centre showed the time and an activity on top of each other, both
// at part opacity: the frame painted mid cross-fade stayed on screen
// because nothing asked to paint once the fade had settled.
#[test]
fn a_centre_that_swaps_again_and_again_settles_on_one_label() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    let theme = Theme::default();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(1600.0, TOPBAR_HEIGHT_PX),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: taffy::AvailableSpace::Definite(1600.0),
                height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .unwrap();
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut paint = |bar: &mut TopBarChrome| {
        let mut scene = Scene::new();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    };
    paint(&mut bar);
    let start = ui::time::now();
    let mut at = start;
    for (round, unread) in [3_usize, 0, 2, 0, 5].into_iter().enumerate() {
        chrome.borrow_mut().publish_activity_glance(
            Some(format!("Ada prepared launch brief {round}")),
            unread,
            100 + round as u64,
            None,
        );
        // A swap is drawn on its first frame and a few mid-fade ones.
        for step in 0..4_u32 {
            ui::time::testing::with_monotonic_time(at, || {
                bar.refresh_status_and_media_state();
                if step == 0 || bar.needs_redraw() {
                    paint(&mut bar);
                }
            });
            at += Duration::from_millis(40);
        }
        // Then the fade settles with no other reason to paint.
        let settled =
            bar.center_settles_at().map_or(at, |end| end.max(at)) + Duration::from_millis(1);
        ui::time::testing::with_monotonic_time(settled, || {
            assert!(
                bar.needs_redraw(),
                "round {round}: the settled centre still has to be painted"
            );
            assert!(
                bar.needs_animation_frame(),
                "round {round}: and asks for it"
            );
            paint(&mut bar);
            assert!(!bar.needs_redraw(), "round {round}: then it is quiet");
            assert!(!bar.needs_animation_frame(), "round {round}");
            assert_eq!(
                bar.cache_fp.center_fade, None,
                "round {round}: no fade on screen"
            );
            assert!(
                bar.center_fade.outgoing(settled).is_none(),
                "round {round}: exactly one label is drawn"
            );
            assert_eq!(bar.center_fade.incoming(settled), 1.0, "round {round}");
            assert_eq!(*bar.center_fade.shown(), unread > 0, "round {round}");
        });
        at = settled + Duration::from_millis(1);
    }
}

// The centre selector jumped from the time's width to the whole lane
// when an activity arrived; it grows into the lane on the standard
// expand/collapse motion and shrinks back to the time when it goes.
#[test]
fn the_centre_selector_grows_into_the_lane_and_back() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    let theme = Theme::default();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(1600.0, TOPBAR_HEIGHT_PX),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: taffy::AvailableSpace::Definite(1600.0),
                height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .unwrap();
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut paint_at = |bar: &mut TopBarChrome, at: Instant| {
        ui::time::testing::with_monotonic_time(at, || {
            bar.refresh_status_and_media_state();
            let mut scene = Scene::new();
            bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        });
    };
    let start = ui::time::now();
    paint_at(&mut bar, start);
    let time_w = bar.center_rect.width();
    let lane_w = bar.center_lane.width();
    assert!(time_w + 1.0 < lane_w, "the time is narrower than its lane");

    for (shown, from, to) in [(3_usize, time_w, lane_w), (0, lane_w, time_w)] {
        let at = bar.center_settles_at().unwrap_or(start).max(start) + Duration::from_secs(1);
        chrome.borrow_mut().publish_activity_glance(
            Some("Ada prepared the launch brief".to_owned()),
            shown,
            100 + shown as u64,
            None,
        );
        paint_at(&mut bar, at);
        assert!(
            (bar.center_rect.width() - from).abs() < 0.5,
            "the selector starts from where it was: {} vs {from}",
            bar.center_rect.width()
        );
        let mid = at + Duration::from_millis(60);
        ui::time::testing::with_monotonic_time(mid, || {
            assert!(bar.needs_animation_frame(), "the morph asks for frames");
            assert!(bar.needs_redraw(), "and draws each of them");
        });
        paint_at(&mut bar, mid);
        let drawn = bar.center_rect.width();
        assert!(
            drawn > from.min(to) + 0.5 && drawn < from.max(to) - 0.5,
            "part of the way from {from} to {to}: {drawn}"
        );
        let settled = bar.center_settles_at().expect("morphing") + Duration::from_millis(1);
        paint_at(&mut bar, settled);
        assert!(
            (bar.center_rect.width() - to).abs() < 0.5,
            "it lands on {to}: {}",
            bar.center_rect.width()
        );
        ui::time::testing::with_monotonic_time(settled, || {
            assert!(!bar.needs_animation_frame(), "then it asks for nothing");
            assert!(!bar.needs_redraw(), "and is quiet");
        });
    }
}

#[test]
fn clicking_a_person_opens_people_not_history() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    let chip = Rect::new(10.0, 10.0, 30.0, 30.0);
    bar.avatar_stack_rect = chip;
    bar.last_cursor = Some(chip.center());

    let mut handled = false;
    bar.handle_left_click(&mut handled);
    assert!(handled);
    assert!(chrome.borrow().overlays.collaborators);
    assert!(
        !chrome.borrow().overlays.room_activity,
        "a presence face represents People; activity has its own explicit route"
    );
}

/// A strip whose windows show the given foreign surfaces under the
/// given titles, so sharing disclosure can name them.
fn shared_windows(surfaces: &[(u64, &str)]) -> Strip {
    let mut strip = Strip::new(tilo_desktop_model::StripId(2), "Main");
    for (surface, title) in surfaces {
        strip.window_groups.push(WindowGroup {
            id: surface.saturating_add(1000),
            tabs: vec![Tab {
                id: *surface,
                title: (*title).into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.editor".into(),
                    surface_id: *surface,
                },
                app_title: None,
            }],
            active_tab: *surface,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    }
    strip
}

#[test]
fn live_share_is_persistent_visible_named_and_accessible() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    tree.workspaces[0]
        .collaborators
        .push(tilo_desktop_model::Collaborator::new("ada", "Ada").with_session(7));
    tree.workspaces[0]
        .strips
        .push(shared_windows(&[(42, "Budget")]));
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    chrome
        .borrow_mut()
        .set_active_exposures(vec![crate::chrome::ActiveExposure {
            peer: 7,
            surface: 42,
            can_control: true,
        }]);
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
    lay_out_and_draw(&mut bar, 1600.0);

    assert!(
        !bar.sharing_indicator_rect.is_zero_area(),
        "an active exposure must paint trusted chrome"
    );
    let node = bar.accessibility().expect("topbar semantics");
    assert!(node.children.iter().any(|child| {
        child.action_target.as_deref() == Some(TopBarChrome::CONTROL_COLLABORATORS)
            && child
                .name
                .as_deref()
                .is_some_and(|name| name.contains("visible to Ada (person): can control Budget"))
    }));
}

#[test]
fn disclosure_attributes_each_surface_to_its_actual_viewer() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    tree.workspaces[0].collaborators.extend([
        tilo_desktop_model::Collaborator::new("ada", "Ada").with_session(7),
        tilo_desktop_model::Collaborator::new("lin", "Lin").with_session(9),
    ]);
    tree.workspaces[0].strips.push(shared_windows(&[
        (42, "Notes"),
        (43, "Budget"),
        (44, "Slides"),
    ]));
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    chrome.borrow_mut().set_active_exposures(vec![
        crate::chrome::ActiveExposure {
            peer: 7,
            surface: 42,
            can_control: false,
        },
        crate::chrome::ActiveExposure {
            peer: 9,
            surface: 43,
            can_control: true,
        },
        crate::chrome::ActiveExposure {
            peer: 7,
            surface: 44,
            can_control: true,
        },
    ]);
    let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
    let disclosure = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
    assert!(
        disclosure.contains("Ada (person): can control Slides; view only Notes"),
        "{disclosure}"
    );
    assert!(
        disclosure.contains("Lin (person): can control Budget"),
        "{disclosure}"
    );
    assert!(!disclosure.contains("Ada (person): can control Budget"));
    assert!(!disclosure.contains("Lin (person): can control Notes"));
}

#[test]
fn agent_viewer_is_never_described_as_a_person() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let workspace = &mut tree.workspaces[0];
    workspace.set_owner("owner");
    assert!(
        !crate::workspace_settings::admit_agent(
            workspace,
            &"owner".to_string(),
            "scribe",
            crate::workspace_settings::AgentPreset::Operator,
        )
        .is_empty(),
        "owner admits agent"
    );
    workspace.collaborators.push(
        tilo_desktop_model::Collaborator::new("scribe", "Release Notes Agent").with_session(9),
    );
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    chrome
        .borrow_mut()
        .set_active_exposures(vec![crate::chrome::ActiveExposure {
            peer: 9,
            surface: 42,
            can_control: false,
        }]);
    let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);

    let name = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
    assert!(
        name.contains("Release Notes Agent (agent): view only a window"),
        "{name}"
    );
    assert!(!name.contains("Release Notes Agent (person"), "{name}");
    assert_eq!(
        bar.sharing_label().as_deref(),
        Some("Agent watching: Release Notes Agent"),
        "critical privacy state must precede the elidable display name"
    );
}

#[test]
fn disclosure_keeps_identity_and_target_when_user_switches_workspaces() {
    let mut tree = two_workspace_tree();
    let shared = &mut tree.workspaces[1];
    shared.name = "Private".into();
    shared.set_owner("owner");
    assert!(
        !crate::workspace_settings::admit_agent(
            shared,
            &"owner".to_string(),
            "scribe",
            crate::workspace_settings::AgentPreset::Operator,
        )
        .is_empty()
    );
    shared
        .collaborators
        .push(tilo_desktop_model::Collaborator::new("scribe", "scribe").with_session(9));
    let mut strip = Strip::new(tilo_desktop_model::StripId(2), "Main");
    let tab_id = 42;
    strip.window_groups.push(WindowGroup {
        id: 43,
        tabs: vec![Tab {
            id: tab_id,
            title: "Draft".into(),
            tile: TileContent::Foreign {
                app_id: "so.tilo.editor".into(),
                surface_id: 42,
            },
            app_title: None,
        }],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    shared.strips.push(strip);

    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    chrome
        .borrow_mut()
        .set_active_exposures(vec![crate::chrome::ActiveExposure {
            peer: 9,
            surface: 42,
            can_control: true,
        }]);
    let bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
    let name = bar.control_name(TopBarChrome::CONTROL_COLLABORATORS);
    assert!(
        name.contains("scribe (agent): can control Draft in Private"),
        "{name}"
    );
    assert!(!name.contains("unidentified session"), "{name}");
}

/// The people control says who is here and nothing else: unread room
/// activity is told by the activity pill in the centre (and History),
/// never by a badge on the people. The badge it had sat half above the
/// bar, white on white, on the empty room's Invite seat.
#[test]
fn unread_activity_paints_nothing_on_the_people_control() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let theme = Theme::default();
    let paint = |unread: usize| {
        let chrome = super::fixture::FixtureChrome::new(tree.clone());
        chrome.borrow_mut().can_invite = true;
        chrome
            .borrow_mut()
            .publish_activity_glance(None, unread, 0, None);
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(1600.0, TOPBAR_HEIGHT_PX),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        bar.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::default(),
        );
        assert_eq!(bar.activity_unread, unread, "nothing else reset it");
        let rgba = ui::offscreen::OffscreenRenderer::new(1600, 48).render(&scene, Color::BLACK);
        (rgba, bar.invite_rect())
    };
    let (quiet, people) = paint(0);
    let (unread, _) = paint(3);
    assert!(people.area() > 0.0, "the empty room shows its Invite seat");
    let around = people.inflate(12.0, 12.0);
    let px = ui::num::u32_of_f64;
    for y in px(around.y0)..px(around.y1.min(48.0)) {
        for x in px(around.x0)..px(around.x1.min(1600.0)) {
            assert_eq!(
                ui::offscreen::OffscreenRenderer::sample(&quiet, 1600, x, y),
                ui::offscreen::OffscreenRenderer::sample(&unread, 1600, x, y),
                "unread activity painted on the people control at ({x}, {y})"
            );
        }
    }
}

#[test]
fn avatar_stack_has_breathing_room_to_tray_pill() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    tree.workspaces[0]
        .collaborators
        .push(tilo_desktop_model::Collaborator::new("alice", "Alice"));

    let mut bar = TopBarChrome::from_tree(&tree);
    let theme = Theme::default();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(1600.0, TOPBAR_HEIGHT_PX),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: taffy::AvailableSpace::Definite(1600.0),
                height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .unwrap();
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let avatar = bar.avatar_stack_rect;
    let tray = bar.tray_pill_rect();
    assert!(
        avatar.width() > 0.0,
        "test seeded one collaborator but avatar_stack_rect is empty",
    );
    let gap = tray.x0 - avatar.x1;
    assert!(
        gap >= 8.0,
        "avatar→tray gap must be ≥ 8 px (got {gap:.2}); avatar.x1={}, tray.x0={}",
        avatar.x1,
        tray.x0,
    );
}

#[test]
fn the_invite_button_invites_by_pointer_and_by_assistive_technology() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;

    let mut bar = TopBarChrome::from_tree(&tree);
    let chrome = super::fixture::FixtureChrome::new(tree);
    chrome.borrow_mut().can_invite = true;
    bar = bar.with_chrome(chrome.clone());

    let theme = Theme::default();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(1600.0, 40.0),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: taffy::AvailableSpace::Definite(1600.0),
                height: taffy::AvailableSpace::Definite(40.0),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .unwrap();
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    assert!(
        bar.collaborator_initials().is_empty() && bar.collaborator_overflow() == 0,
        "this test is about the empty case and the roster is not empty",
    );
    assert!(
        bar.invite_rect().area() > 0.0,
        "an empty room offers the first invite",
    );
    assert!(bar.avatar_stack_rect.is_zero_area(), "and no faces");

    bar.last_cursor = Some(bar.invite_rect().center());
    let mut handled = false;
    bar.handle_left_click(&mut handled);
    assert!(handled);
    assert_eq!(chrome.borrow().intents.last(), Some(&ChromeIntent::Invite));
    assert!(
        !chrome.borrow().overlays.collaborators,
        "inviting is one step: the list stays closed"
    );

    let node = bar.accessibility().expect("topbar semantics");
    let (index, invite) = node
        .children
        .iter()
        .enumerate()
        .find(|(_, child)| child.name.as_deref() == Some("Invite"))
        .expect("assistive technology reaches the Invite control");
    assert_eq!(invite.role, ui::a11y::Role::Button);
    assert_eq!(
        bar.synthetic_accessibility_rect(&[index]),
        Some(bar.invite_rect())
    );
    let target = invite.action_target.clone().expect("an action target");
    let mut doc = AutoCommit::new();
    chrome.borrow_mut().intents.clear();
    assert!(bar.perform_semantic_action(
        &target,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(
        chrome.borrow().intents.as_slice(),
        [ChromeIntent::Invite],
        "activating Invite invites, as a click does",
    );
}

#[test]
fn spectators_and_marked_agents_read_in_the_avatar_stack() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;
    let paint = |bar: &mut TopBarChrome| {
        let theme = Theme::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
        let vp = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(1600.0, 40.0),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout_with_measure(
                vp,
                Size {
                    width: taffy::AvailableSpace::Definite(1600.0),
                    height: taffy::AvailableSpace::Definite(40.0),
                },
                |_, _, _, _, _| taffy::Size::ZERO,
            )
            .unwrap();
        bar.apply_layout(&taffy, node, Point::ORIGIN);
        bar.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::default(),
        );
    };

    // Spectators alone: the chip is the People control, not Invite.
    let chrome = super::fixture::FixtureChrome::new(tree.clone());
    chrome.borrow_mut().watching = 2;
    let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome.clone());
    paint(&mut bar);
    assert!(bar.collaborator_initials().is_empty());
    assert!(bar.people_button_rect.height() >= BAR_CONTROL_PX);
    let node = bar.accessibility().expect("topbar semantics");
    assert!(
        node.children
            .iter()
            .all(|child| child.name.as_deref() != Some("Invite")),
        "an audience is not an empty room"
    );
    assert!(
        node.children.iter().any(|child| child
            .name
            .as_deref()
            .is_some_and(|name| name.contains("2 people watching"))),
        "the bar names its spectators"
    );
    bar.last_cursor = Some(bar.people_button_rect.center());
    let mut handled = false;
    bar.handle_left_click(&mut handled);
    assert!(handled && chrome.borrow().overlays.collaborators);

    // A hosted roster marks software by role id; it draws as an agent.
    let mut agent = tilo_desktop_model::Collaborator::new("scribe", "Scribe");
    agent.role_id = Some(crate::collaborators::AGENT_ROLE_MARKER.into());
    tree.workspaces[0].collaborators =
        vec![agent, tilo_desktop_model::Collaborator::new("ada", "Ada")];
    let mut bar = TopBarChrome::from_tree(&tree);
    bar.sync_from_tree(&tree);
    assert_eq!(bar.collaborator_agents, vec![true, false]);
    // Each face is named for a reader, an agent as one.
    paint(&mut bar);
    let node = bar.accessibility().expect("topbar semantics");
    let people = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some(TopBarChrome::CONTROL_COLLABORATORS))
        .expect("the people control");
    let faces: Vec<_> = people
        .children
        .iter()
        .filter_map(|face| face.name.as_deref())
        .collect();
    assert_eq!(faces, ["Scribe, agent", "Ada"]);
    assert!(people.children.iter().all(|face| face.bounds.is_some()));
}

#[test]
fn compact_topbar_aligns_floating_controls_and_keeps_the_corner_target() {
    let mut tree = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    tree.active = tree.workspaces[0].id;

    let mut bar = TopBarChrome::from_tree(&tree);
    let theme = Theme::default();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = bar.build_layout(&mut taffy, &theme, &mut node_map);
    let vp = taffy
        .new_with_children(
            Style {
                size: Size::from_lengths(800.0, TOPBAR_HEIGHT_PX),
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            vp,
            Size {
                width: taffy::AvailableSpace::Definite(800.0),
                height: taffy::AvailableSpace::Definite(TOPBAR_HEIGHT_PX),
            },
            |_, _, _, _, _| taffy::Size::ZERO,
        )
        .unwrap();
    bar.apply_layout(&taffy, node, Point::ORIGIN);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

    let bar_bottom = bar.rect.y1;
    assert!(
        (bar_bottom - f64::from(TOPBAR_HEIGHT_PX)).abs() < 0.5,
        "topbar y1 ({bar_bottom}) should equal TOPBAR_HEIGHT_PX ({TOPBAR_HEIGHT_PX})",
    );
    assert_eq!(TOPBAR_HEIGHT_PX, 32.0);
    // Every control is a bar control, centred: the same air above and
    // below.
    let inset = (f64::from(TOPBAR_HEIGHT_PX) - BAR_CONTROL_PX) / 2.0;
    for (label, rect) in [
        ("workspace", bar.ws_pill_rect),
        ("center", bar.center_rect),
        ("tray", bar.tray_pill_rect),
    ] {
        assert_eq!(rect.y0 - bar.rect.y0, inset, "{label} top inset");
        assert_eq!(bar.rect.y1 - rect.y1, inset, "{label} bottom inset");
    }
    assert_eq!(bar.tray_hit_rect.y0, bar.rect.y0);
    assert_eq!(bar.tray_hit_rect.y1, bar.rect.y1);
    assert_eq!(bar.tray_hit_rect.x1, bar.rect.x1);
    assert!(bar.tray_hit_rect.contains(Point::new(799.0, 1.0)));
    assert!(!bar.tray_pill_rect.contains(Point::new(799.0, 1.0)));
}

/// Both states asserted together, since a stack that's empty (or
/// full) in both cases would pass only half.
#[test]
fn a_present_collaborator_in_this_workspace_gets_an_avatar() {
    let empty = DesktopTree {
        workspaces: vec![Workspace::new(WorkspaceId(1), "A")],
        active: WorkspaceId::from(0),
        active_strip: HashMap::default(),
        active_group: HashMap::default(),
        collaborators: Vec::new(),
        ..Default::default()
    };
    let mut empty = empty;
    empty.active = empty.workspaces[0].id;
    let bar = TopBarChrome::from_tree(&empty);
    assert!(
        bar.collaborator_initials().is_empty(),
        "a workspace nobody else is in has nobody to show",
    );

    let mut elsewhere = empty.clone();
    elsewhere.collaborators = vec![tilo_desktop_model::Collaborator::new("alice", "Alice")];
    let bar = TopBarChrome::from_tree(&elsewhere);
    assert!(
        bar.collaborator_initials().is_empty(),
        "session-wide presence must not leak somebody from another workspace into this bar",
    );

    let mut joined = empty.clone();
    joined.workspaces[0].collaborators = vec![tilo_desktop_model::Collaborator::new(
        "peer@11895642303397647149",
        "Xaver",
    )];
    let bar = TopBarChrome::from_tree(&joined);
    assert_eq!(
        bar.collaborator_initials(),
        &['X'],
        "the bar must show somebody present in this workspace",
    );
    assert_eq!(
        bar.collaborator_overflow(),
        0,
        "one peer is not an overflow",
    );

    // Invited but not redeemed: an avatar here would falsely claim
    // he's in the workspace.
    let mut asked = empty.clone();
    asked.workspaces[0].collaborators = vec![
        tilo_desktop_model::Collaborator::new("peer@11895642303397647149", "Xaver").as_invited(),
    ];
    let bar = TopBarChrome::from_tree(&asked);
    assert!(
        bar.collaborator_initials().is_empty(),
        "somebody invited and not yet arrived is not an avatar in the \
         cluster: {:?}",
        bar.collaborator_initials(),
    );
    assert_eq!(
        bar.collaborator_overflow(),
        0,
        "and does not show up in the +N chip either",
    );

    // Another device of this person is theirs, not a face in the stack.
    let mut own = empty;
    own.workspaces[0].collaborators =
        vec![tilo_desktop_model::Collaborator::new("peer@7", "Orange Pi").as_own_device()];
    let bar = TopBarChrome::from_tree(&own);
    assert!(bar.collaborator_initials().is_empty());
    assert_eq!(bar.collaborator_overflow(), 0);
}
