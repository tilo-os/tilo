//! The compose surface has to stay up long enough to be typed into.
//!
//! Somebody right-clicks a photograph, a panel opens beside the context menu
//! carrying that photograph's address, they write a sentence and press Enter.
//! Every part of that was built and the gesture still produced nothing,
//! because the panel went away between opening and the first click aimed at
//! it — so the field was never focused, nothing was typed, and no remark was
//! ever put on the wire.
//!
//! These drive the real host: `open_remark`, then ordinary `handle_event`
//! pointer and keyboard events at real rects, then the outbox the compositor
//! drains. Nothing is stubbed, so a panel that dismisses itself fails here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::state::{
    AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use taffy::prelude::TaffyMaxContent;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

/// The address the gesture is made on, in the shape the file manager resolves
/// one: the kind and the path together, so it still means the same object in a
/// session opened tomorrow.
const SUBJECT: &str = "files.entry:/var/tmp/qos/workspace/harbour.png";

/// One-strip, one-group, one-tab tree — the smallest topology the chrome can
/// render. The panel is compositor chrome and sits over whatever is there.
fn minimal_tree() -> WorkspaceTree {
    let ws_id = WorkspaceId::new();
    let strip_id = StripId::new();
    let group_id: GroupId = 1;
    let tab_id: u64 = 0xc0;
    let group = WindowGroup {
        id: group_id,
        tabs: vec![Tab {
            id: tab_id,
            title: "Files".into(),
            tile: TileContent::Stub {
                kind: AppKind::Other("Files".into()),
                label: "stub".into(),
            },
        }],
        active_tab: tab_id,
        width: WindowWidth::Normal,
        maximized: false,
    };
    let strip = Strip {
        id: strip_id,
        name: "S".into(),
        window_groups: vec![group],
    };
    let ws = Workspace {
        strips: vec![strip],
        ..Workspace::test_default(ws_id, "W")
    };
    let mut active_strip = HashMap::new();
    active_strip.insert(ws_id, strip_id);
    let mut active_group = HashMap::new();
    active_group.insert(strip_id, group_id);
    WorkspaceTree {
        workspaces: vec![ws],
        active: ws_id,
        active_strip,
        active_group,
        collaborators: vec![],
        ..Default::default()
    }
}

/// Lay the host out into a 1600x900 viewport. `self.rect` is set as a side
/// effect, which is what `open_remark`'s placement reads.
fn layout_host(host: &mut shell::shell::overlay_host::ShellOverlayHost) -> (TaffyTree<()>, NodeId) {
    use taffy::{Dimension, Style};
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let host_node = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[host_node],
        )
        .unwrap();
    let map_for_measure = node_map.clone();
    let mut tr = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1600.0),
                height: AvailableSpace::Definite(900.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .or_else(|_| taffy.compute_layout(root, taffy::Size::MAX_CONTENT))
        .expect("layout must compute");
    host.apply_layout(&taffy, host_node, Point::ZERO);
    (taffy, host_node)
}

/// One event through the host, the way the compositor sends it.
fn dispatch(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    taffy: &TaffyTree<()>,
    host_node: NodeId,
    doc: &mut automerge::AutoCommit,
    event: &WindowEvent,
) {
    let root_id = automerge::ROOT;
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    host.handle_event(
        event,
        1.0,
        &winit::event::Modifiers::default(),
        doc,
        &root_id,
        &mut handled,
        &mut focus_target,
        host_node,
        taffy,
    );
}

fn move_cursor(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    taffy: &TaffyTree<()>,
    host_node: NodeId,
    doc: &mut automerge::AutoCommit,
    p: Point,
) {
    dispatch(
        host,
        taffy,
        host_node,
        doc,
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(p.x, p.y),
        },
    );
}

/// A whole left click at `p`: the motion that puts the pointer there, then
/// press and release. Three events, in the order real hardware produces them
/// — the press is hit-tested against the position the motion left behind.
fn click_at(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    taffy: &TaffyTree<()>,
    host_node: NodeId,
    doc: &mut automerge::AutoCommit,
    p: Point,
) {
    move_cursor(host, taffy, host_node, doc, p);
    for state in [ElementState::Pressed, ElementState::Released] {
        dispatch(
            host,
            taffy,
            host_node,
            doc,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state,
                button: MouseButton::Left,
            },
        );
    }
}

fn key_event(key: Key) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = key.clone();
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = if let Key::Character(s) = &key {
        Some(s.clone())
    } else {
        None
    };
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

fn type_str(
    host: &mut shell::shell::overlay_host::ShellOverlayHost,
    taffy: &TaffyTree<()>,
    host_node: NodeId,
    doc: &mut automerge::AutoCommit,
    s: &str,
) {
    for c in s.chars() {
        if c.is_control() {
            continue;
        }
        let ev = key_event(Key::Character(winit::keyboard::SmolStr::new(c.to_string())));
        dispatch(host, taffy, host_node, doc, &ev);
    }
}

/// The rect of the widget carrying `name`, found by walking the published
/// tree — the same walk a test harness makes over the control socket.
fn named_rect(root: &dyn Widget, name: &str) -> Option<Rect> {
    if root.test_id() == Some(name) {
        return Some(root.get_rect());
    }
    for child in root.children() {
        if let Some(r) = named_rect(child, name) {
            return Some(r);
        }
    }
    None
}

fn subject() -> qos_reference::ResourceRef {
    qos_reference::ResourceRef::whole("files.entry", SUBJECT, "harbour.png")
}

/// The context menu the gesture opened, in the asking window's coordinates,
/// and the offset onto the desktop — the numbers the film's own run logged.
fn menu_and_origin() -> (Rect, Point) {
    (Rect::new(413.6, 103.0, 633.6, 505.0), Point::new(0.0, 124.0))
}

/// A press the chrome cannot place must not put the panel away.
///
/// This is the shape the failure actually had. The panel opens above the
/// windows on a right-click made *inside* one, so the pointer has been over an
/// application — whose motion belongs to that application and is withheld from
/// the chrome — for the whole gesture. The press that follows carries no
/// position of its own, and the chrome had none to measure it against.
///
/// Substituting one is where it went wrong: the origin is outside every panel
/// there has ever been, so the first press after the panel opened always read
/// as aimed away from it, and the surface closed under the hand reaching for
/// it. There is a difference between a press somewhere else and a press
/// nowhere known, and only the first of them is a dismissal.
#[test]
fn a_press_the_chrome_cannot_place_leaves_the_panel_up() {
    let tree = minimal_tree();
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = layout_host(&mut host);
    let mut doc = automerge::AutoCommit::new();

    let (menu, origin) = menu_and_origin();
    assert!(host.open_remark(subject(), menu, origin), "the panel opens");
    assert_eq!(
        host.pointer_position(),
        None,
        "no motion has been dispatched into this tree, which is the state the \
         chrome is in when the pointer has spent the gesture inside a window",
    );

    for state in [ElementState::Pressed, ElementState::Released] {
        dispatch(
            &mut host,
            &taffy,
            host_node,
            &mut doc,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state,
                button: MouseButton::Left,
            },
        );
    }

    assert!(
        host.remark_is_open(),
        "a press the chrome could not place dismissed the panel; a position \
         nobody knows is not evidence that somebody aimed away from it",
    );
}

/// A press aimed at the menu the panel opened beside must not put the panel
/// away. The two are one gesture — the panel is *for* the object the menu is
/// about — and the click that first reaches for the panel travels across the
/// menu to get there.
#[test]
fn a_press_on_the_menu_beside_it_leaves_the_panel_up() {
    let tree = minimal_tree();
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = layout_host(&mut host);
    let mut doc = automerge::AutoCommit::new();

    let (menu, origin) = menu_and_origin();
    assert!(
        host.open_remark(subject(), menu, origin),
        "the room is always a destination, so the panel must open"
    );
    assert!(host.remark_is_open(), "the panel opened");

    // Dead centre of the menu, on the desktop. This is where a harness
    // aiming at the panel by the only rect it had — the app's account of
    // its own menu — actually lands.
    let inside_menu = Point::new(
        menu.x0 + origin.x + menu.width() * 0.5,
        menu.y0 + origin.y + menu.height() * 0.28,
    );
    click_at(&mut host, &taffy, host_node, &mut doc, inside_menu);

    assert!(
        host.remark_is_open(),
        "a press on the menu the panel opened beside dismissed it; \
         the panel and the menu are one gesture",
    );
}

/// The whole gesture, from the request an application makes to the remark the
/// compositor drains: open, click into the field, type, Enter.
///
/// Asserted on the outbox rather than on the panel closing, because a panel
/// that closes looks the same whether it sent or gave up — which is exactly
/// the state this beat shipped in.
#[test]
fn typing_into_the_panel_and_pressing_enter_produces_a_remark() {
    let tree = minimal_tree();
    let (mut host, _chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = layout_host(&mut host);
    let mut doc = automerge::AutoCommit::new();

    let (menu, origin) = menu_and_origin();
    assert!(host.open_remark(subject(), menu, origin), "the panel opens");

    let field = named_rect(&host, "remark_input")
        .expect("the panel publishes its text field, so a harness can aim at it");
    assert!(
        field.area() > 0.0,
        "the field has to be laid out before it can be clicked; got {field:?}",
    );

    click_at(
        &mut host,
        &taffy,
        host_node,
        &mut doc,
        Point::new(
            (field.x0 + field.x1) * 0.5,
            (field.y0 + field.y1) * 0.5,
        ),
    );
    assert!(
        host.remark_is_open(),
        "clicking into the panel's own field closed it",
    );

    type_str(&mut host, &taffy, host_node, &mut doc, "claude: crop this");
    dispatch(
        &mut host,
        &taffy,
        host_node,
        &mut doc,
        &key_event(Key::Named(NamedKey::Enter)),
    );

    let sent = host.take_remarks();
    assert_eq!(
        sent.len(),
        1,
        "Enter in the field is a send, so exactly one remark should be waiting",
    );
    let remark = &sent[0];
    assert_eq!(
        remark.reference.resource_id().as_str(),
        SUBJECT,
        "what left carries the file's address, not a description of it",
    );
    assert_eq!(
        remark.text, "claude: crop this",
        "the sentence typed is the sentence sent",
    );
    match remark.destination.target() {
        qos_reference::Target::Channel(id) => assert_eq!(
            id,
            qos_reference::ROOM_CONVERSATION,
            "the room is the first destination, so an untouched selector sends there",
        ),
        other => panic!("a remark nobody re-addressed went to {other:?}"),
    }
}

// -----------------------------------------------------------------------------
// What the session says it did
// -----------------------------------------------------------------------------
//
// The gesture's only account of itself outside this process is two lines in the
// session log, and they are read by whatever is watching a run — field by
// field, finding the end of a field by finding a space. So they are asserted as
// text, at the shape a reader parses, rather than as "something was logged".
//
// It matters that they come from `deliver`, which is the end that puts the
// message on the wire. The application never reaches the code that used to
// write them: a window whose chrome this session draws never composes and never
// sends. The send worked and the only evidence of it went with the panel, which
// looks exactly like a send that gave up.

/// A tracing sink that keeps what was written, so a test can read the session's
/// own account of the gesture rather than a paraphrase of it.
#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<u8>>>);

impl Recorded {
    fn text(&self) -> String {
        let held = self.0.lock().expect("log buffer");
        String::from_utf8_lossy(&held).into_owned()
    }
}

impl std::io::Write for Recorded {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Recorded {
    type Writer = Recorded;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn broker_socket_paths(label: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    let n = std::process::id() as u64 * 1_000_003
        + std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .subsec_nanos() as u64;
    (
        base.join(format!("qos-{label}-{n}.json.sock")),
        base.join(format!("qos-{label}-{n}.capnp.sock")),
    )
}

/// The whole gesture, ending where it has to end: in the room, with a session
/// log that says so.
///
/// Driven through the three stages the compositor runs every frame — the panel
/// takes the sentence, the host drains its outbox, `deliver` puts it in the
/// room — so nothing here stands in for the step it is asserting.
#[test]
fn a_sent_remark_is_reported_by_the_end_that_sent_it() {
    let tree = minimal_tree();
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(&tree);
    let (taffy, host_node) = layout_host(&mut host);
    let mut doc = automerge::AutoCommit::new();

    let (menu, origin) = menu_and_origin();
    assert!(host.open_remark(subject(), menu, origin), "the panel opens");
    type_str(&mut host, &taffy, host_node, &mut doc, "claude: crop this");
    dispatch(
        &mut host,
        &taffy,
        host_node,
        &mut doc,
        &key_event(Key::Named(NamedKey::Enter)),
    );
    let written = host.take_remarks();
    assert_eq!(written.len(), 1, "Enter sent exactly one remark");

    let (json_path, capnp_path) = broker_socket_paths("remark-delivery");
    let broker = shell::shell::collab_broker::spawn_full(json_path, Some(capnp_path), None)
        .expect("the broker spawns");

    let recorded = Recorded::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(recorded.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        shell::shell::remark_surface::deliver(&chrome, &broker, written);
    });
    let log = recorded.text();

    // The line a reader takes the destination, the subject and the words from.
    // `to=` carries the destination's *id*: the selector's label for the room
    // is localised prose with a space in it, and a reader that finds the end of
    // a field by finding a space would take `the` for the whole destination and
    // then fail to find either field after it.
    let sent = log
        .lines()
        .find(|l| l.contains("remark sent"))
        .unwrap_or_else(|| panic!("nothing in the session log says a remark was sent:\n{log}"))
        .to_string();
    assert!(
        sent.contains(&format!(
            "remark sent to=channel:{} about={} text=claude: crop this",
            qos_reference::ROOM_CONVERSATION,
            SUBJECT,
        )),
        "the send line is not in the shape a reader parses: {sent}",
    );

    // And the second claim, from the same end: not merely composed — put in the
    // room. The id follows the sentence directly, with no key in front of it,
    // because it is logged through a field tracing renders as the message.
    let wire = log
        .lines()
        .find(|l| l.contains("sending a remark into the room"))
        .unwrap_or_else(|| panic!("nothing in the session log says it left:\n{log}"))
        .to_string();
    let after = wire
        .split("sending a remark into the room")
        .nth(1)
        .expect("the line names what followed")
        .trim_start()
        .to_string();
    assert!(
        after.starts_with("m_"),
        "the id of the message that went into the room should follow the \
         sentence directly; got {wire}",
    );

    // Printed so a passing run still shows what the session claimed.
    println!("{sent}");
    println!("{wire}");
}
