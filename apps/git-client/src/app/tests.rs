use super::*;
use ui::widget::Widget;

/// The first-run screen's three controls fit the lane they are drawn in:
/// side by side while the path keeps room to type in, and in two rows
/// below that, so neither a field nor a button is squeezed to nothing.
#[test]
fn the_open_screen_folds_its_row_before_it_squeezes_the_path() {
    let wide_lane = Rect::new(0.0, 0.0, 400.0, 400.0);
    let wide = OpenControls::at(wide_lane, 60.0, 32.0, 8.0, 102.0, 58.0, 180.0);
    assert!(
        (wide.field.y0 - wide.choose.y0).abs() < f64::EPSILON,
        "one row when it fits"
    );
    assert!(
        wide.field.width() >= 180.0,
        "the path keeps its room: {:?}",
        wide.field
    );
    assert!(wide.choose.x1 <= wide.open.x0, "the buttons do not overlap");
    assert!(!wide.field.overlaps(wide.choose) && !wide.choose.overlaps(wide.open));

    // The same lane at a 1.5 text scale in an expanded language: the two
    // buttons take almost all of it.
    let lane = Rect::new(0.0, 0.0, 342.0, 400.0);
    let narrow = OpenControls::at(lane, 60.0, 48.0, 12.0, 215.0, 85.0, 180.0);
    assert!(
        narrow.field.y1 <= narrow.choose.y0,
        "the buttons moved under the path: {:?} {:?}",
        narrow.field,
        narrow.choose
    );
    assert!(
        (narrow.field.width() - lane.width()).abs() < f64::EPSILON,
        "the path keeps the whole row"
    );
    assert!(
        narrow.choose.width() >= 215.0,
        "the chooser keeps its label"
    );
    assert!(
        narrow.choose.x1 <= narrow.open.x0,
        "the buttons do not overlap"
    );
}

#[test]
fn live_initial_view_stays_empty_until_the_repository_is_explicitly_bound() {
    let mut app = GitClientApp::new();
    let mut state = crate::resource::GitClient::default();
    app.present_state(&state);
    assert!(app.awaiting_repo);
    assert!(app.repo_path.is_none());
    assert!(app.model.branches.is_empty());
    assert!(app.model.commits.is_empty());
    let repository = tempfile::tempdir().unwrap();
    git2::Repository::init(repository.path()).unwrap();
    state.repository = ui::tilo_resource::Atom(repository.path().to_string_lossy().into_owned());
    app.present_state(&state);
    assert!(!app.awaiting_repo);
    assert_eq!(app.repo_path.as_deref(), Some(repository.path()));
}

/// Build an app over the bundled sample, laid out at `w` x 800 with the
/// flap settled to its mode default so geometry is deterministic.
fn app_at(w: f64, screen: Screen) -> GitClientApp {
    let mut app = GitClientApp::from_model(crate::sample::model().0, crate::sample::model().1);
    app.screen = screen;
    app.rect = Rect::new(0.0, 0.0, w, 800.0);
    app.drawer.resolve(app.rect);
    app.drawer.snap(app.drawer.is_open());
    draw_frame(&mut app);
    app
}

fn press(app: &mut GitClientApp, pos: Point, doc: &mut AutoCommit) {
    app.pointer = Some(pos);
    let hit = app
        .targets
        .iter()
        .rev()
        .find(|t| t.rect.contains(pos))
        .map(|t| t.hit.clone());
    if let Some(hit) = hit {
        app.activate(hit, &mut ui::widget::Edits::lend(doc));
    }
    draw_frame(app);
}

/// Adaptive screen mode: wide Changes docks, narrow master-detail overlays closed,
/// the breakpoint boundary is inclusive, Panels never collapses, and crossing the
/// breakpoint restores the mode default. Each phase is a former test with its own fresh fixture.
#[test]
fn screen_mode_follows_the_breakpoint_and_preset() {
    wide_changes_screen_is_docked();
    narrow_master_detail_screen_is_overlay_closed();
    breakpoint_boundary_is_inclusive_docked();
    panels_screen_never_collapses();
    crossing_breakpoint_restores_mode_default();
}

fn wide_changes_screen_is_docked() {
    let app = app_at(1280.0, Screen::Changes);
    assert!(app.drawer.is_docked());
    assert!(app.drawer.is_open());
}

fn narrow_master_detail_screen_is_overlay_closed() {
    let app = app_at(680.0, Screen::Changes);
    assert!(app.drawer.is_overlay());
    assert!(!app.drawer.is_open());
}

fn breakpoint_boundary_is_inclusive_docked() {
    let boundary = app_at(1280.0, Screen::Changes).drawer.resolved_breakpoint();
    assert!(app_at(boundary, Screen::Changes).drawer.is_docked());
    assert!(app_at(boundary - 1.0, Screen::Changes).drawer.is_overlay());
}

fn panels_screen_never_collapses() {
    let app = app_at(400.0, Screen::Panels);
    let toolbar = app.window_toolbar();
    assert!(
        toolbar.sidebar_unavailable,
        "no list, so the toggle cannot act"
    );
    assert!(!app.drawer_is_modal());
}

/// The overlay flap: the bar's sidebar toggle opens and closes it, a scrim
/// click dismisses it, a click inside keeps it open, and while open it
/// hides and blocks background controls. Each phase is a former test with
/// its own fresh fixture.
#[test]
fn overlay_flap_toggles_dismisses_and_blocks_the_background() {
    bar_toggle_opens_and_closes_overlay_flap();
    scrim_click_dismisses_open_overlay_flap();
    click_inside_open_flap_does_not_dismiss_it();
    open_overlay_hides_and_blocks_background_controls();
}

fn bar_toggle_opens_and_closes_overlay_flap() {
    let mut app = app_at(680.0, Screen::Changes);
    assert!(!app.drawer.is_open());
    assert_eq!(app.window_toolbar().sidebar, Some(false));
    app.run_window_bar_event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar);
    draw_frame(&mut app);
    assert!(app.drawer.is_open());
    assert_eq!(app.window_toolbar().sidebar, Some(true));
    app.run_window_bar_event(&ui::window_toolbar::ToolbarEvent::ToggleSidebar);
    draw_frame(&mut app);
    assert!(!app.drawer.is_open());
}

fn scrim_click_dismisses_open_overlay_flap() {
    let mut app = app_at(680.0, Screen::Changes);
    app.drawer.snap(true);
    draw_frame(&mut app);
    assert!(app.drawer.is_open());
    press(&mut app, Point::new(670.0, 400.0), &mut AutoCommit::new());
    assert!(!app.drawer.is_open());
}

fn click_inside_open_flap_does_not_dismiss_it() {
    let mut app = app_at(680.0, Screen::Changes);
    app.drawer.snap(true);
    draw_frame(&mut app);
    let panel = app.drawer.sidebar_rect();
    press(
        &mut app,
        Point::new(panel.x0 + 4.0, panel.y1 - 4.0),
        &mut AutoCommit::new(),
    );
    assert!(app.drawer.is_open());
}

fn open_overlay_hides_and_blocks_background_controls() {
    let mut app = app_at(680.0, Screen::Changes);
    app.drawer.snap(true);
    draw_frame(&mut app);
    assert!(app.drawer_is_modal());
    assert!(
        app.semantic
            .iter()
            .all(|t| matches!(t.hit, Hit::Change(_) | Hit::Commit(_))),
        "covered content controls leave the accessible tree"
    );
    let covered = app.drawer.sidebar_rect();
    press(
        &mut app,
        Point::new(covered.x0 + 4.0, covered.y1 - 4.0),
        &mut AutoCommit::new(),
    );
    assert_eq!(app.screen, Screen::Changes);
    assert!(app.drawer.is_open());
}

fn crossing_breakpoint_restores_mode_default() {
    let mut app = app_at(1280.0, Screen::Changes);
    app.drawer.close();
    assert!(!app.drawer.is_open());
    app.rect = Rect::new(0.0, 0.0, 680.0, 800.0);
    app.drawer.resolve(app.rect);
    assert!(app.drawer.is_overlay());
    assert!(!app.drawer.is_open());
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.drawer.resolve(app.rect);
    assert!(app.drawer.is_docked());
    assert!(
        !app.drawer.is_open(),
        "resizing preserves the person's docked choice"
    );
}

/// Semantic parity: semantic open-repository and text edits reach their
/// inputs, and synthetic accessibility controls sit at their drawn rects.
/// Each phase is a former test with its own fresh fixture.
#[test]
fn affordances_and_semantic_controls_reach_the_production_actions() {
    semantic_open_repository_binds_the_entered_repository();
    semantic_text_edits_reach_embedded_repository_commit_and_branch_inputs();
    synthetic_accessibility_controls_are_addressable_at_their_drawn_rects();
}

fn semantic_open_repository_binds_the_entered_repository() {
    let directory = tempfile::tempdir().unwrap();
    let _repository = git2::Repository::init(directory.path()).unwrap();
    let mut app = GitClientApp::open_empty();
    app.open_input.set_text(directory.path().to_str().unwrap());
    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_semantic_action(
        &mut app,
        "git.open-repository",
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert!(!app.test_awaiting_repo());
    assert_eq!(app.test_last_error(), None);
}

fn semantic_text_edits_reach_embedded_repository_commit_and_branch_inputs() {
    let mut app = GitClientApp::open_empty();
    let mut doc = automerge::AutoCommit::new();
    for (target, text) in [
        ("git.open-path", r"C:\Work\Grün 🌍"),
        ("git.commit-message", "Keep Unicode: Grün 🌍"),
        ("git.new-branch-name", "feature/grün"),
    ] {
        assert!(Widget::perform_semantic_action(
            &mut app,
            target,
            &ui::a11y::Action::SetValue(text.into()),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
    }
    assert_eq!(app.open_input.value(), r"C:\Work\Grün 🌍");
    assert_eq!(app.commit_input.value(), "Keep Unicode: Grün 🌍");
    assert_eq!(app.branch_input.value(), "feature/grün");
    assert!(app.test_awaiting_repo(), "editing a path must not open it");
}

fn synthetic_accessibility_controls_are_addressable_at_their_drawn_rects() {
    let mut app = app_at(1_280.0, Screen::Changes);
    let node = Widget::accessibility(&app).expect("git client is semantic");
    assert!(!node.children.is_empty());
    let targets: std::collections::HashSet<_> = node
        .children
        .iter()
        .filter_map(|child| child.action_target.as_deref())
        .collect();
    assert_eq!(targets.len(), node.children.len());
    assert!(targets.contains("git.commit-message"));
    assert!(
        targets
            .iter()
            .any(|target| target.starts_with("git.change.open:")),
        "the changed files are reachable: {targets:?}"
    );
    for (index, child) in node.children.iter().enumerate() {
        assert_eq!(
            Widget::synthetic_accessibility_rect(&app, &[index]),
            child.bounds.map(ui::a11y::AccessBounds::to_rect),
            "semantic control {index} must activate inside the visible control"
        );
    }

    let mut doc = automerge::AutoCommit::new();
    assert!(Widget::perform_semantic_action(
        &mut app,
        "git.commit-message",
        &ui::a11y::Action::Focus,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(app.focus, Some(Field::Commit));
}

/// Paint one frame with a real text renderer so every measured rect is
/// the production geometry, not a hand-poked stand-in.
fn draw_frame(app: &mut GitClientApp) {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    Widget::draw(
        app,
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::default(),
    );
}
