//! A window that asks the desktop's session: opening another app, opening a resource on its record,
//! asking another app for an action, and opening content in one.

use super::*;

/// An app whose capture offers to open Writer, as Camera offers Photos.
#[derive(Default)]
struct OpensWriter {
    rect: ui::kurbo::Rect,
}

impl ui::widget::Widget for OpensWriter {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _: &ui::Theme,
        _: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> ui::taffy::NodeId {
        taffy
            .new_leaf(ui::taffy::Style {
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap()
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn name(&self) -> &'static str {
        "OpensWriter"
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.rect
    }
}

impl ui::resources::PublishesResources for OpensWriter {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<ui::tilo_actions::ResourceSpec> {
        vec![
            ui::tilo_actions::ResourceSpec::new("camera.capture", "camera.capture", "Capture")
                .with_action(
                    ui::tilo_actions::remote::ApplicationLaunch::app("so.tilo.writer").spec(
                        "open-writer",
                        "Open Writer",
                        "Open Writer",
                    ),
                ),
        ]
    }
}

/// A desk whose Camera window offers to open Writer; any `so.tilo.` app launches as a window of its
/// own.
struct OpeningApps;

impl TileSource for OpeningApps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        if tab.title == "Camera" {
            return TileHost::new(ui::widget::TileContent::App(Box::new(Hosted(Box::new(
                OpensWriter::default(),
            )))));
        }
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, workspace: &Workspace, id: u64, app: &str) -> Option<Tab> {
        Notices(Rc::new(Cell::new(0))).launch(workspace, id, app)
    }
}

/// The page desktop: one frame hosting its windows in process, serving them its own session for the
/// workspace `workspace` .
fn serving_desk(
    workspace: &str,
) -> (
    ui::headless::FrameHarness,
    DesktopHandle,
    tilo_desktop_surface::session::Openings,
) {
    let mut minter = IdMinter::new(7);
    let camera = tab(&mut minter, "Camera");
    let tree = seed_tree(&mut minter, "Desk", vec![camera]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(OpeningApps));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    let (session, openings) =
        tilo_desktop_surface::session::serve(workspace, std::sync::Arc::default());
    driver.set_session_door(
        ui::RemoteActions::with_host(
            session,
            ui::tilo_actions::remote::Caller::local_person("tester").in_workspace(workspace),
        ),
        Some(workspace.into()),
    );
    driver.relayout();
    (driver, handle, openings)
}

/// Camera's "Open Photos" in the browser desktop: a window asks the page's session to open another
/// app, and the desktop opens it as a window of its own beside the one that asked. What the desktop
/// refuses to open is said over the window that asked, never dropped.
#[test]
fn a_window_opens_another_app_through_the_desktop_s_session() {
    let (mut driver, handle, openings) = serving_desk("workspace");
    driver
        .invoke_published("camera.capture", "open-writer", &serde_json::Map::new())
        .expect("the session takes the open");
    assert_eq!(
        handle.snapshot("g", vec![]).entries.len(),
        1,
        "not yet opened"
    );
    assert!(openings.open(&handle, |_, _| Ok(())));
    driver.idle();
    driver.relayout();
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 2, "{:?}", snapshot.entries);
    assert_eq!(snapshot.entries[1].label, "writer");
    assert_eq!(
        snapshot.active.as_ref().unwrap(),
        &snapshot.entries[1].route,
        "the opened app is in front"
    );
    assert_eq!(driver.refusal_said(), None);

    let (mut driver, handle, openings) = serving_desk("workspace");
    driver
        .invoke_published("camera.capture", "open-writer", &serde_json::Map::new())
        .expect("the session takes the open");
    openings.open(&handle, |_, _| {
        Err(ui::tilo_actions::remote::ActionHostError::NotPermitted)
    });
    driver.idle();
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), 1);
    assert!(
        driver.refusal_said().is_some(),
        "the refusal is said over the window that asked"
    );
}

/// An app whose page fills its window and scrolls a document that can be long or empty, as Speed
/// Test's and every writing surface's does.
struct FillingPage(ui::widget::Scrollable);

impl ui::widget::Decorator for FillingPage {
    type Inner = ui::widget::Scrollable;

    fn inner(&self) -> &ui::widget::Scrollable {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut ui::widget::Scrollable {
        &mut self.0
    }
}

impl ui::widget::AppWidget for FillingPage {
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Filling"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("F")
    }
}

/// Every window but the one named `Filling` , which shows `text` .
struct FillingTiles(ui::LocalState<String>);

impl TileSource for FillingTiles {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        if tab.title != "Filling" {
            return TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            });
        }
        let page = ui::widget::Scrollable::new_vertical()
            .fill_parent_height()
            .with_content_min_viewport_height()
            .with_child(Box::new(
                ui::widget::TextInput::from_state(&self.0)
                    .multiline(true)
                    .with_id("filling-editor"),
            ));
        TileHost::new(ui::widget::TileContent::App(Box::new(FillingPage(page))))
    }

    fn launch(&mut self, _: &Workspace, _: u64, _: &str) -> Option<Tab> {
        None
    }
}

/// Where each window's app is drawn: its tile.
fn windows(driver: &ui::headless::FrameHarness) -> Vec<ui::vello::kurbo::Rect> {
    fn walk(node: &ui::headless::InspectionNode, out: &mut Vec<ui::vello::kurbo::Rect>) {
        if node.widget == "TileHost" {
            let [x, y, w, h] = node.rect;
            out.push(ui::vello::kurbo::Rect::new(x, y, x + w, y + h));
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut tiles = Vec::new();
    walk(&driver.inspection().root, &mut tiles);
    tiles
}

/// A window whose page fills it never makes the strip taller than its band,
/// however long its document, and fills the window again once the document
/// is empty. The row of windows and each window's tile took their content's
/// height as their minimum, so a long document raised every window past the
/// screen. A page that fills at least its viewport then kept that height as
/// its floor, and with the page and its content adjusting a pass apart they
/// traded the old and new heights without settling, so the desktop stopped
/// drawing.
#[test]
fn a_filling_page_keeps_its_strip_to_the_band_and_settles() {
    let text = ui::LocalState::new("A line of the document.\n".repeat(120));
    let mut minter = IdMinter::new(7);
    let tabs = ["Notes", "Filling", "Scratch"]
        .iter()
        .map(|title| tab(&mut minter, title))
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(FillingTiles(text.clone())));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    driver.set_caller_palette_enabled(false);
    driver
        .relayout_at((1200, 800))
        .expect("the desktop with a long document settles");
    let long = *driver.targets().get("filling-editor").unwrap();
    let tiles = windows(&driver);
    assert_eq!(tiles.len(), 3, "{tiles:?}");
    for tile in &tiles {
        assert!(
            tile.y1 <= 800.0,
            "no window reaches past the band for a long document: {tile:?} ({long:?})"
        );
    }

    text.set(String::new());
    for _ in 0..3 {
        driver
            .relayout_at((1200, 800))
            .expect("the desktop settles once the document is empty");
    }
    let short = *driver.targets().get("filling-editor").unwrap();
    let tiles = windows(&driver);
    for tile in &tiles {
        assert!(
            tile.y1 <= 800.0,
            "the height the long document filled is no floor once it is gone: {tile:?}"
        );
    }
    let filling = tiles
        .iter()
        .find(|tile| tile.contains(short.origin()))
        .expect("the editor is in a window");
    assert!(
        (short.height() - filling.height()).abs() < 1.0,
        "the empty page fills its window: {short:?} in {filling:?}"
    );
}

/// Windows on records: each record launch opens its app on that record,
/// and the record's window, once open, is the one shown again.
struct RecordWindows {
    opened: Rc<std::cell::RefCell<Vec<(u64, String)>>>,
}

impl TileSource for RecordWindows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str) -> Option<Tab> {
        let (app, record) = tilo_desktop_search::launched_record(app)?;
        self.opened.borrow_mut().push((id, record.to_owned()));
        Some(Tab {
            id,
            title: record.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: record.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
    }

    fn showing(&self, app: &str) -> Option<u64> {
        let (_, record) = tilo_desktop_search::launched_record(app)?;
        self.opened
            .borrow()
            .iter()
            .find(|(_, opened)| opened == record)
            .map(|(tab, _)| *tab)
    }
}

/// A shared window's message opens the resource it shares: the page's session opens the resource's
/// app on its record, and asked again shows that window instead of a second one. A resource in
/// another workspace is not this desktop's to open.
#[test]
fn a_window_opens_a_resource_on_its_record_through_the_desktop_s_session() {
    use ui::tilo_actions::remote::{
        APPLICATION_OPEN_ACTION, ActionHost as _, Caller, InvokeRequest, application_record_target,
    };
    let mut minter = IdMinter::new(7);
    let chat = tab(&mut minter, "Chat");
    let tree = seed_tree(&mut minter, "Desk", vec![chat]);
    let opened = Rc::default();
    let (surface, handle) = DesktopSurface::new(
        tree,
        7,
        Box::new(RecordWindows {
            opened: Rc::clone(&opened),
        }),
    );
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1200, 800),
        ui::Theme::dark(),
    );
    let (session, openings) =
        tilo_desktop_surface::session::serve("workspace", std::sync::Arc::default());
    let caller = Caller::local_person("tester").in_workspace("workspace");
    let open = |workspace: &str| -> InvokeRequest {
        serde_json::from_value(serde_json::json!({
            "invocation": ui::tilo_actions::InvocationId::new(),
            "target": application_record_target(workspace, "so.tilo.notes", "n:plan", "plan", APPLICATION_OPEN_ACTION),
        }))
        .unwrap()
    };
    let refused = session.invoke(&caller, open("elsewhere"));
    drop(refused);
    assert!(
        !openings.open(&handle, |_, _| Ok(())),
        "nothing asked of here"
    );

    for _ in 0..2 {
        drop(session.invoke(&caller, open("workspace")));
        assert!(openings.open(&handle, |app, _| {
            assert_eq!(app, "so.tilo.notes", "admitted as its app");
            Ok(())
        }));
        driver.relayout();
    }
    assert_eq!(opened.borrow().len(), 1, "one window on the record");
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 2, "{:?}", snapshot.entries);
    assert_eq!(snapshot.entries[1].label, "n:plan");
    assert_eq!(snapshot.active.as_ref(), Some(&snapshot.entries[1].route));
}

/// Another action than opening, asked of an app's resource (a launchpad card's "show tasks", an app
/// launching another with an action): the page's session opens the app on the record and hands the
/// action to the host, which invokes it in that window once it is up, as the workspace broker
/// delivers an action to an app it launched.
#[test]
fn a_window_asks_another_app_for_an_action_through_the_desktop_s_session() {
    use ui::tilo_actions::remote::{
        ActionHost as _, Caller, InvokeRequest, application_record_target,
    };
    let mut minter = IdMinter::new(7);
    let chat = tab(&mut minter, "Chat");
    let tree = seed_tree(&mut minter, "Desk", vec![chat]);
    let opened = Rc::default();
    let (surface, handle) = DesktopSurface::new(
        tree,
        7,
        Box::new(RecordWindows {
            opened: Rc::clone(&opened),
        }),
    );
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1200, 800),
        ui::Theme::dark(),
    );
    let (session, openings) =
        tilo_desktop_surface::session::serve("workspace", std::sync::Arc::default());
    let caller = Caller::local_person("tester").in_workspace("workspace");
    let ask = |argument: serde_json::Value| -> InvokeRequest {
        serde_json::from_value(serde_json::json!({
            "invocation": ui::tilo_actions::InvocationId::new(),
            "target": application_record_target("workspace", "so.tilo.notes", "n:plan", "plan", "pin"),
            "argument": argument,
        }))
        .unwrap()
    };
    drop(session.invoke(&caller, ask(serde_json::json!({"Json": {"place": "top"}}))));
    assert!(openings.open(&handle, |_, _| Ok(())));
    driver.relayout();
    assert_eq!(opened.borrow().len(), 1, "the app opens on the record");
    let reveals = openings.take_reveals();
    assert_eq!(reveals.len(), 1, "{reveals:?}");
    assert_eq!(
        (
            reveals[0].app.as_str(),
            reveals[0].record.as_str(),
            reveals[0].resource.as_str(),
            reveals[0].action.as_str(),
        ),
        ("so.tilo.notes", "n:plan", "plan", "pin")
    );
    assert_eq!(
        reveals[0].arguments.get("place"),
        Some(&serde_json::json!("top"))
    );
}

/// Windows opened with content: each content launch opens its app on what it was handed.
struct ContentWindows {
    opened: Rc<std::cell::RefCell<Vec<(String, String)>>>,
}

impl TileSource for ContentWindows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str) -> Option<Tab> {
        let (app, hash) = tilo_desktop_surface::session::launched_content(app)?;
        self.opened
            .borrow_mut()
            .push((app.to_owned(), hash.to_owned()));
        Some(Tab {
            id,
            title: app.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: app.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
    }
}

/// A window hands content to another app (the camera's capture to Photos,
/// a report's recording to the app that replays it): the page's session
/// opens that app with the content, by its hash, as the workspace broker
/// stages it for an app it launches. Anything but a content hash is no
/// content to open.
#[test]
fn a_window_opens_content_in_another_app_through_the_desktop_s_session() {
    use ui::tilo_actions::remote::{ActionHost as _, ApplicationLaunch, Caller, InvokeRequest};
    let mut minter = IdMinter::new(7);
    let camera = tab(&mut minter, "Camera");
    let tree = seed_tree(&mut minter, "Desk", vec![camera]);
    let opened = Rc::default();
    let (surface, handle) = DesktopSurface::new(
        tree,
        7,
        Box::new(ContentWindows {
            opened: Rc::clone(&opened),
        }),
    );
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1200, 800),
        ui::Theme::dark(),
    );
    let (session, openings) =
        tilo_desktop_surface::session::serve("workspace", std::sync::Arc::default());
    let caller = Caller::local_person("tester").in_workspace("workspace");
    let hash = "ab".repeat(32);
    let launch = ApplicationLaunch::with_content("so.tilo.photos", hash.clone());
    let ask = |argument: ui::tilo_actions::remote::InvokeArgument| -> InvokeRequest {
        serde_json::from_value(serde_json::json!({
            "invocation": ui::tilo_actions::InvocationId::new(),
            "target": launch.target("workspace"),
            "argument": argument,
        }))
        .unwrap()
    };
    let refused = session.invoke(
        &caller,
        ask(ui::tilo_actions::remote::InvokeArgument::Text(
            "../photo.jpg".into(),
        )),
    );
    drop(refused);
    assert!(
        !openings.open(&handle, |_, _| Ok(())),
        "a path is no content to open"
    );
    drop(session.invoke(&caller, ask(launch.argument())));
    assert!(openings.open(&handle, |app, existing| {
        assert_eq!(app, "so.tilo.photos", "admitted as its app");
        assert!(!existing, "opening an app with content is opening the app");
        Ok(())
    }));
    driver.relayout();
    assert_eq!(
        *opened.borrow(),
        vec![("so.tilo.photos".to_owned(), hash)],
        "the app opens with the content"
    );
}
