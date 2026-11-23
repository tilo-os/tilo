//! Click-driven navigation proof.

use settings::categories::Category;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::Affine;

use crate::support::{Window, click_category, selected, window, window_row, window_rows};
use ui::widget::DrawChild;

// Tall enough for the complete category corpus. Sidebar scrolling itself is
// covered by Scrollable's integration tests; this suite exercises category
// activation and must click visible rows rather than coordinates below clip.
const VIEWPORT: (u32, u32) = (1200, 1800);

fn row_is_active(app: &mut Window, label: &str) -> bool {
    window_row(app, label).is_some_and(|row| row.selected)
}

fn renders_something(app: &mut Window) -> bool {
    app.with_widget::<ui::widget::NavShell, _>(|shell| {
        let theme = ui::Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        let mut scene = Scene::new();
        shell.draw_child(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        !scene.encoding().is_empty()
    })
    .expect("settings shows its shell")
}

/// Count the selected navigation rows. After a navigation exactly one row, the
/// target, must be selected.
fn active_row_count(app: &mut Window) -> usize {
    window_rows(app).iter().filter(|row| row.selected).count()
}

fn click(app: &mut Window, target: Category) {
    let rect = window_row(app, &target.label())
        .unwrap_or_else(|| panic!("no sidebar row for {target:?}"))
        .rect;
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "row {target:?} has a real rect"
    );
    assert!(
        rect.y0 >= 0.0 && rect.y1 <= f64::from(VIEWPORT.1),
        "navigation fixture must show {target:?}: {rect:?}"
    );
    click_category(app, target);
}

/// Every category, each in a window of its own: the window opens on the
/// default category, a click on the category's row selects it and only it,
/// its pane draws, and the frames an idle window runs afterwards change
/// nothing in the resource. The default category's row is clicked too: the
/// row already selected stays the one selected.
#[test]
fn clicking_a_category_navigates_to_its_pane_and_leaves_the_resource_alone() {
    for &target in Category::ALL {
        let mut app = window(VIEWPORT);
        assert_eq!(
            selected(&mut app),
            Category::opening(),
            "{target:?}: starts on the default category"
        );
        click(&mut app, target);
        let heads = app.read_document(ui::tilo_resource::heads_of);
        assert_eq!(
            selected(&mut app),
            target,
            "after the click the selection must be {target:?} (read back from the app)"
        );

        // The sidebar highlights the clicked category and ONLY it.
        assert!(
            row_is_active(&mut app, &target.label()),
            "the {target:?} row must be active after navigating to it"
        );
        if target != Category::opening() {
            assert!(
                !row_is_active(&mut app, &Category::opening().label()),
                "the previously-active default row must no longer be highlighted after navigating to {target:?}"
            );
        }
        assert_eq!(
            active_row_count(&mut app),
            1,
            "exactly one sidebar row may be active after navigating to {target:?}"
        );

        assert!(
            renders_something(&mut app),
            "the {target:?} content pane must render after navigating to it"
        );

        app.settle();
        assert_eq!(
            app.read_document(ui::tilo_resource::heads_of),
            heads,
            "{target:?} idle frames must leave the resource unchanged"
        );
    }
}

#[test]
fn two_consecutive_clicks_navigate() {
    let mut app = window(VIEWPORT);

    click(&mut app, Category::Displays);
    assert_eq!(selected(&mut app), Category::Displays);

    click(&mut app, Category::Permissions);
    assert_eq!(selected(&mut app), Category::Permissions);
    assert!(row_is_active(&mut app, &Category::Permissions.label()));
    assert!(!row_is_active(&mut app, &Category::Displays.label()));
}

/// The page's element saying `words`: its number and the target it carries, found the way
/// `page.mjs` finds the element a press lands on (the nearest one carrying `data-a` over the
/// words).
fn page_row_saying(html: &str, words: &str) -> Option<(u32, Option<String>)> {
    let at = html.find(&format!(">{words}<"))?;
    let before = html.get(..at)?;
    let start = before.rfind("<li")?;
    let open = before.get(start..)?.split('>').next()?;
    let number = open.split("data-p=\"").nth(1)?.split('"').next()?;
    let target = open
        .split("data-t=\"")
        .nth(1)
        .map(|rest| rest.split('"').next().unwrap_or_default().to_owned());
    Some((number.parse().ok()?, target))
}

/// A press on a category in the sidebar of the page a person's browser shows selects that
/// category, as a press in the window does: the page hands the element's number and target to
/// the view, which acts on the widget the element stands for. The window is as tall as the one an
/// app worker lays out, so the category pressed is one its own height cuts off: a page shows every
/// row its list holds, and a press on one is the list's row action, never a point in the frame.
#[test]
fn a_press_on_a_category_in_the_page_selects_it() {
    let mut app = window((1280, 720));
    let composition = app.harness().composition();
    let mut page = ui::composition::page::Page::new();
    let html = page.render(&composition);
    let label = Category::ExternalAccounts.label();
    let (row, target) = page_row_saying(&html, &label)
        .unwrap_or_else(|| panic!("the page's sidebar offers {label}: {html}"));
    let path = page
        .path(row)
        .expect("the element stands for a widget")
        .to_owned();
    // An element a page numbers for part of a widget is acted on through the widget.
    let widget = path
        .split_once('#')
        .map_or(path.as_str(), |(widget, _)| widget)
        .to_owned();
    app.harness()
        .perform_semantic_named(&widget, target.as_deref(), &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("the press on the page's row is acted on: {error}"));
    app.settle();
    assert_eq!(
        selected(&mut app),
        Category::ExternalAccounts,
        "the pressed category is the chosen one"
    );
}

/// A window shorter than its page leaves the controls below the fold out of each frame. Once
/// the page is drawn the window asks for no more frames, however long it stays open: an idle
/// Settings window costs the device nothing.
#[test]
fn the_opening_page_comes_to_rest_in_a_window_shorter_than_it() {
    let mut app = window((1238, 940));
    let mut frames = 0;
    while app.frame_if_owed() {
        frames += 1;
        assert!(
            frames < 60,
            "the idle opening page still asks for frames after {frames}: {:?}",
            app.harness().frame_schedule()
        );
        app.idle();
    }
    app.idle();
    let schedule = app.harness().frame_schedule();
    assert!(
        !schedule.needs_redraw && !schedule.needs_animation_frame,
        "the idle opening page asks for a frame: {schedule:?}"
    );
}
