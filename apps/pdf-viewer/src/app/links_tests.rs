//! Links on real pages: hovering shows a pointer, clicking an internal link
//! goes to its destination (through the current page plan), clicking an
//! external one hands the URL to the host's opener, and other schemes and
//! files are refused.

use ui::widget::Edits;
use winit::dpi::PhysicalPosition;

use super::*;
use crate::actions::{delete_page_plan_action, move_page_plan_action};
use crate::source::LinkTarget;

fn links_fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("OUT_DIR")).join("links.pdf")
}

/// The reader on the four-page links fixture, sized like a desktop window,
/// with its markup record checked out. `None` (with a message) without an
/// engine on this host.
fn open_links() -> Option<(PdfApp, ui::State)> {
    let mut app = tests::open_settled(links_fixture())?;
    let mut state = ui::State::new();
    tests::settle_record(&mut app, &mut state);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.recompute_regions();
    app.sync_viewport_frame();
    app.tool = Tool::Select;
    app.go_to_page(1);
    Some((app, state))
}

/// The screen point at the centre of the first link on `page` whose target
/// matches `want`, after loading that page's links.
fn link_centre(app: &mut PdfApp, page: usize, want: impl Fn(&LinkTarget) -> bool) -> Point {
    app.ensure_page_links(page);
    tests::drain_until(app, |app| app.raster_links.contains_key(&page));
    let map = app.page_map(page);
    let link = app.raster_links[&page]
        .iter()
        .find(|l| want(&l.target))
        .expect("the link is on the page")
        .clone();
    let (cx, cy) = map.layer_rect_to_card(&link.rect).center();
    let origin = app.page_origins()[page - 1];
    Point::new(origin.x + cx * app.zoom(), origin.y + cy * app.zoom())
}

/// Press and release at `p` through the widget's real event entry point.
fn click(app: &mut PdfApp, state: &mut ui::State, p: Point) {
    let mut at_root = ui::testing::Dispatch::new()
        .doc(&mut state.doc)
        .path(&state.root);
    let device_id = winit::event::DeviceId::dummy();
    for ev in [
        WindowEvent::CursorMoved {
            device_id,
            position: PhysicalPosition::new(p.x, p.y),
        },
        WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id,
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ] {
        at_root.send(app, &ev);
    }
}

fn is_internal(t: &LinkTarget) -> bool {
    matches!(t, LinkTarget::Page { .. })
}

/// Without an engine on this host the first open skips (with a message)
/// and no later case runs either, exactly as each case skipped on its own.
#[test]
#[serial_test::serial(pdfium)]
fn internal_links_show_a_pointer_and_navigate_through_the_page_plan() {
    use ui::Widget as _;
    // Hovering shows a pointer only in the select tool.
    let Some((mut app, _state)) = open_links() else {
        return;
    };
    let over = link_centre(&mut app, 1, is_internal);
    assert_eq!(
        app.cursor_shape(over),
        Some(winit::window::CursorIcon::Pointer)
    );
    let below = Point::new(over.x, over.y + 80.0 * app.zoom());
    assert_eq!(
        app.cursor_shape(below),
        None,
        "no link below the first line"
    );
    app.tool = Tool::Highlight;
    assert_eq!(
        app.cursor_shape(over),
        None,
        "markup tools draw over links rather than follow them"
    );

    // Clicking an internal link goes to its destination.
    {
        let (mut app, mut state) = open_links().expect("the engine opened the fixture above");
        let at = link_centre(&mut app, 1, is_internal);
        click(&mut app, &mut state, at);
        assert_eq!(app.current_page, 3, "the appendix is on page three");
        // The heading sits part way down its page, and the view scrolls to it
        // rather than to the top of the page.
        let page_top = 2.0 * app.stack_unit() * app.zoom();
        assert!(
            app.scroll_y() > page_top + 20.0 * app.zoom(),
            "scrolled to the destination point ({} vs page top {page_top})",
            app.scroll_y()
        );
    }

    // An internal link follows its page through reorder and deletion.
    {
        let (mut app, mut state) = open_links().expect("the engine opened the fixture above");
        // Move the appendix (source page 3) to the front: it is now page 1 and
        // the link's own page is page 2.
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            move_page_plan_action("source-3", None),
        );
        tests::settle_until(&mut app, &mut state, |app| {
            app.pending_worker_plan.is_none()
        });
        assert_eq!(app.resource_page_sources, vec![3, 1, 2, 4]);
        app.go_to_page(2);
        let at = link_centre(&mut app, 2, is_internal);
        click(&mut app, &mut state, at);
        assert_eq!(app.current_page, 1, "the destination moved to the front");

        // Delete the appendix: the link says so instead of jumping anywhere.
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            delete_page_plan_action("source-3"),
        );
        tests::settle_until(&mut app, &mut state, |app| {
            app.pending_worker_plan.is_none()
        });
        assert_eq!(app.resource_page_sources, vec![1, 2, 4]);
        app.go_to_page(1);
        let at = link_centre(&mut app, 1, is_internal);
        // A separate click much later, not the second half of a double-click
        // on the same screen spot.
        app.drag_select = DragSelect::new();
        click(&mut app, &mut state, at);
        assert_eq!(app.current_page, 1);
        assert!(
            app.status_note
                .as_deref()
                .is_some_and(|note| note.contains("no longer in the document")),
            "{:?}",
            app.status_note
        );
    }

    // Markup over a link keeps its own click.
    {
        let (mut app, mut state) = open_links().expect("the engine opened the fixture above");
        let at = link_centre(&mut app, 1, is_internal);
        let (_, pt) = app.hit_page(at).expect("on page one");
        app.annotations.push(Annotation::new(
            1,
            AnnoKind::highlight(PtRect::new(pt.0 - 5.0, pt.1 - 5.0, 10.0, 10.0)),
            Swatch::Yellow,
            "Ada",
        ));
        let id = app.annotations.last().unwrap().id.clone();
        click(&mut app, &mut state, at);
        assert_eq!(app.selected_anno.as_deref(), Some(id.as_str()));
        assert_eq!(app.current_page, 1, "the link underneath was not followed");
    }
}

#[test]
#[serial_test::serial(pdfium)]
fn external_links_hand_only_web_and_mail_urls_to_the_host() {
    // Clicking an external link hands the URL to the host.
    let Some((mut app, mut state)) = open_links() else {
        return;
    };
    let at = link_centre(&mut app, 1, |t| matches!(t, LinkTarget::Uri(_)));
    let (services, recorder) = ui::host::HostServices::recording();
    ui::host::scoped(&services, || click(&mut app, &mut state, at));
    assert_eq!(recorder.opened_uris(), ["https://example.org/docs"]);
    assert_eq!(app.current_page, 1, "an external link does not navigate");

    // Only web and mail links are opened.
    {
        let (mut app, _state) = open_links().expect("the engine opened the fixture above");
        let (services, recorder) = ui::host::HostServices::recording();
        let rect = PtRect::new(0.0, 0.0, 10.0, 10.0);
        ui::host::scoped(&services, || {
            for target in [
                LinkTarget::Uri("mailto:ada@example.org".into()),
                LinkTarget::Uri("javascript:alert(1)".into()),
                LinkTarget::Uri("file:///etc/passwd".into()),
                LinkTarget::File("other.pdf".into()),
            ] {
                app.activate_link(1, &crate::source::PageLink { rect, target });
            }
        });
        assert_eq!(recorder.opened_uris(), ["mailto:ada@example.org"]);
        assert!(
            app.status_note
                .as_deref()
                .is_some_and(|note| note.contains("other files")),
            "{:?}",
            app.status_note
        );
    }
}
