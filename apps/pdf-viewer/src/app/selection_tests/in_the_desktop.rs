//! Selecting and copying a PDF's text with the reader in a window of the
//! desktop surface, beside another window, as the browser desktop hosts every
//! app: a press-drag reaches the reader through the strip and its window, and
//! Ctrl+C copies what it selected.

use std::{cell::RefCell, rc::Rc, time::Duration};

use serial_test::serial;
use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::application::{HostContext, HostContextInputs, LaunchTarget, RestoreInput};
use ui::headless::FrameHarness;
use ui::owner_wake::OwnerWakeQueue;
use ui::widget::{TileHost, Widget};
use vello::kurbo::Point;

use super::{PdfApp, glyph_at, past, text_pdf};

/// Builds each window's reader on the PDF, as the browser desktop builds a
/// window's component.
struct Readers {
    owners: ui::record::RecordOwners,
    state: std::path::PathBuf,
    queue: OwnerWakeQueue,
    pdf: std::path::PathBuf,
}

impl TileSource for Readers {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        let room = workspace.room().to_string();
        let definition = crate::definition();
        let records = self
            .owners
            .open(&definition.app_id, &room)
            .expect("records");
        let views = Rc::new(
            ui::application_view::InstanceViewStore::new(self.state.clone(), "device", &room)
                .expect("views"),
        );
        let permissions = Rc::new(RefCell::new(ui::permissions::PermissionContext::member(
            "reader",
            ui::permissions::Role::Owner,
        )));
        let mut context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views,
                device_id: "device".into(),
                instance_id: tab.id,
                permissions,
                author: ui::permissions::UserId("reader".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            &LaunchTarget::New,
        )
        .expect("the window's context");
        context.provide_launch_arguments(vec![self.pdf.to_string_lossy().into_owned()]);
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .expect("the reader");
        component
            .mount_with_owner_queue(&self.queue, |_| {})
            .expect("mounted");
        TileHost::from_component(component)
    }
}

/// A visit to one reader, taken by the first that matches.
type Visit<'a, T> = Option<Box<dyn FnOnce(&mut PdfApp) -> T + 'a>>;

/// Visit the reader of window `nth` (from the left) of the desktop.
fn reader<T>(h: &mut FrameHarness, nth: usize, visit: impl FnOnce(&mut PdfApp) -> T) -> Option<T> {
    fn walk<T>(widget: &mut dyn Widget, nth: &mut usize, visit: &mut Visit<'_, T>) -> Option<T> {
        if let Some(any) = widget.as_any_mut()
            && ui::framework::with_surface::<crate::PdfMarkup, PdfApp, ()>(any, |_| ()).is_some()
        {
            if *nth > 0 {
                *nth -= 1;
                return None;
            }
            let visit = visit.take()?;
            return ui::framework::with_surface::<crate::PdfMarkup, PdfApp, T>(
                widget.as_any_mut()?,
                visit,
            );
        }
        widget
            .children_mut()
            .into_iter()
            .find_map(|child| walk(child, nth, visit))
    }
    let mut nth = nth;
    let mut visit: Visit<'_, T> = Some(Box::new(visit));
    walk(h.root_mut::<DesktopSurface>(), &mut nth, &mut visit)
}

/// Run the desktop's frames until `done` holds of the right-hand reader.
fn settle(h: &mut FrameHarness, mut done: impl FnMut(&PdfApp) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let _ = h.poll_frame();
        h.relayout();
        h.service_pending_owner_models().expect("owner models");
        if reader(h, 1, |pdf| done(pdf)).unwrap_or(false) {
            return;
        }
        assert!(ui::time::now() < deadline, "the reader never settled");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[serial(clipboard, pdfium)]
#[test]
fn a_drag_in_a_desktop_window_selects_the_text_and_ctrl_c_copies_it() {
    ui::i18n::I18nManager::init("en-US");
    let dir = tempfile::tempdir().expect("temp dir");
    let pdf = text_pdf(dir.path(), 1);
    let mut minter = IdMinter::new(29);
    let tabs = (0..2)
        .map(|_| Tab {
            id: minter.mint(),
            title: "PDF Viewer".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.pdf-viewer".into(),
                label: "PDF Viewer".into(),
                note: String::new(),
            },
            app_title: None,
        })
        .collect();
    let queue = OwnerWakeQueue::default();
    let state = dir.path().join("state");
    let (surface, handle) = DesktopSurface::new(
        seed_tree(&mut minter, "Desk", tabs),
        29,
        Box::new(Readers {
            owners: ui::record::RecordOwners::new(state.clone()),
            state,
            queue: queue.clone(),
            pdf,
        }),
    );
    handle.set_viewport_width(1440.0);
    let mut h = FrameHarness::for_root_with(
        ui::state::State::new(),
        surface,
        (1440, 900),
        ui::headless::FrameOptions {
            theme: Some(ui::Theme::dark()),
            ..ui::headless::FrameOptions::default()
        },
    );
    // As the browser desktop runs its frame.
    h.set_caller_palette_enabled(false);
    h.attach_owner_wake_queue(queue).expect("owner wakes");
    h.set_coalesce_input_frames(true);
    settle(&mut h, |pdf| {
        pdf.has_document && pdf.pending_open.is_none() && !pdf.pending_visuals
    });
    if !reader(&mut h, 1, |pdf| pdf.raster_active()).unwrap_or(false) {
        eprintln!("SKIPPING: no PDF engine reads text on this host");
        return;
    }

    // The pointer rests on the page, which reads its text ahead.
    let rest = reader(&mut h, 1, |pdf| {
        let origin = pdf.page_origins()[0];
        Point::new(origin.x + 40.0, origin.y + 40.0)
    })
    .expect("the reader");
    h.send_host_event(&ui::synthetic_input::cursor_moved(rest.x, rest.y));
    settle(&mut h, |pdf| pdf.raster_text.contains_key(&1));

    let (from, to) = reader(&mut h, 1, |pdf| {
        (
            glyph_at(pdf, 1, "Page 1 line 2:", 7),
            past(pdf, 1, "Page 1 line 4:", 12),
        )
    })
    .expect("the reader");
    // One frame between each event, as the page's animation frames run.
    h.send_host_event(&ui::synthetic_input::cursor_moved(from.x, from.y));
    h.send_host_event(&ui::synthetic_input::mouse_press());
    let _ = h.poll_frame();
    for step in 1..=6 {
        let t = f64::from(step) / 6.0;
        h.send_host_event(&ui::synthetic_input::cursor_moved(
            from.x + (to.x - from.x) * t,
            from.y + (to.y - from.y) * t,
        ));
        let _ = h.poll_frame();
    }
    h.send_host_event(&ui::synthetic_input::mouse_release());
    settle(&mut h, |pdf| pdf.selection.is_some_and(|sel| !sel.dragging));
    let selected = reader(&mut h, 1, |pdf| pdf.selection_text())
        .flatten()
        .unwrap_or_default();
    assert!(
        selected.starts_with("line 2:") && selected.ends_with("Page 1 line 4"),
        "the drag selects what it covered, in reading order: {selected:?}"
    );
    assert!(
        reader(&mut h, 0, |pdf| pdf.selection.is_none()).expect("the other reader"),
        "the window beside it selects nothing"
    );

    ui::clipboard_copy("before");
    h.set_modifiers(&["ctrl".to_owned()]);
    h.key("c");
    h.set_modifiers(&[]);
    settle(&mut h, |pdf| {
        pdf.status_note
            .as_deref()
            .is_some_and(|note| note.starts_with("Copied"))
    });
    let copied = ui::clipboard_paste().unwrap_or_default();
    if copied == "before" {
        // No clipboard in this session: the copy said it copied the selection.
        eprintln!("no clipboard in this session; the copy reported itself");
    } else {
        assert_eq!(copied, selected, "Ctrl+C copies the selection");
    }
}
