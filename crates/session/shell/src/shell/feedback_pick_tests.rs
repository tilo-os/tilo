use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, Role};
use winit::dpi::PhysicalPosition;
use winit::event::DeviceId;

use super::*;

/// The shell's own tree: a bar along the top with a named button, and an
/// unnamed area where the window another process draws is.
struct Node {
    name: &'static str,
    rect: Rect,
    access: Option<AccessNode>,
    kids: Vec<Node>,
}

impl Widget for Node {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &ui::theme::Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.access.clone()
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.kids.iter().map(|kid| kid as &dyn Widget).collect()
    }
}

fn shell() -> Node {
    Node {
        name: "Shell",
        rect: Rect::new(0.0, 0.0, 1000.0, 800.0),
        access: None,
        kids: vec![Node {
            name: "Bar",
            rect: Rect::new(0.0, 0.0, 1000.0, 40.0),
            access: None,
            kids: vec![Node {
                name: "Button",
                rect: Rect::new(10.0, 5.0, 90.0, 35.0),
                access: Some(
                    AccessNode::new(Role::Button)
                        .with_name("Workspaces")
                        .with_interface_name(true),
                ),
                kids: Vec::new(),
            }],
        }],
    }
}

/// Chat's window, at (100, 100) on the desktop.
fn chat() -> PickWindow {
    PickWindow {
        surface: 9,
        rect: Rect::new(100.0, 100.0, 600.0, 500.0),
        origin: Point::new(100.0, 100.0),
        app: "so.tilo.chat".into(),
        name: "Chat".into(),
        record: Some("chat.workspace".into()),
        connections: vec![4],
    }
}

fn moved(to: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(to.x, to.y),
    }
}

fn button(pressed: bool) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        },
        button: MouseButton::Left,
    }
}

struct Picking {
    picker: NativePicker,
    root: Node,
    now: Instant,
}

impl Picking {
    fn open() -> Self {
        let mut picker = NativePicker::default();
        let now = ui::time::now();
        picker.open(vec![chat()], now);
        Self {
            picker,
            root: shell(),
            now,
        }
    }

    fn event(&mut self, event: &WindowEvent, at: Point, shift: bool) {
        let modifiers = if shift {
            ModifiersState::SHIFT
        } else {
            ModifiersState::empty()
        };
        assert!(
            self.picker
                .handle_event(event, Some(at), modifiers, &self.root, self.now),
            "the picker takes every event"
        );
    }

    /// The one question out, to Chat's connection.
    fn asked(&mut self) -> (u64, PickQuery) {
        let mut outbox = self.picker.take_outbox();
        assert_eq!(outbox.len(), 1, "one question: {outbox:?}");
        let (connection, FloatingSurfaceHostMessage::Pick { request, query }) = outbox.remove(0)
        else {
            panic!("a pick question");
        };
        assert_eq!(connection, 4, "asked over Chat's own connection");
        (request, query)
    }

    fn answer(&mut self, app: &str, request: u64, answer: PickAnswer) -> bool {
        self.picker
            .answered(app, request, answer, &self.root, self.now)
    }
}

fn part(path: &str, role: &str, rect: [f64; 4]) -> PickedPart {
    PickedPart {
        path: path.into(),
        role: role.into(),
        rect,
    }
}

fn cx() -> ReportContext {
    ReportContext {
        names: vec!["Ada Lovelace".into()],
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pick's whole round trip, from hover to report, reads in order"
)]
fn a_window_names_what_is_under_the_pointer_and_a_click_reports_it_with_its_texts() {
    let mut picking = Picking::open();
    assert!(picking.picker.is_open());
    let at = Point::new(150.0, 130.0);
    picking.event(&moved(at), at, false);
    let (request, query) = picking.asked();
    assert_eq!(
        query,
        PickQuery::At {
            point: [50.0, 30.0]
        },
        "in the window's own coordinates"
    );
    assert_eq!(
        picking.picker.deadline(picking.now),
        Some(picking.now + ANSWER_WAIT)
    );
    // Moving on while the question is out asks once the answer is in.
    let later = Point::new(160.0, 130.0);
    picking.event(&moved(later), later, false);
    assert!(
        picking.picker.take_outbox().is_empty(),
        "one question at a time"
    );
    assert!(
        !picking.answer(
            "so.tilo.files",
            request,
            PickAnswer::Targets { parts: Vec::new() }
        ),
        "another app's answer is not taken"
    );
    let named = PickAnswer::Targets {
        parts: vec![
            part("Root[0]/List[0]", "List", [0.0, 0.0, 500.0, 300.0]),
            part(
                "Root[0]/List[0]/Row[1]",
                "ListItem",
                [0.0, 20.0, 500.0, 60.0],
            ),
        ],
    };
    assert!(picking.answer("so.tilo.chat", request, named));
    let (_, query) = picking.asked();
    assert_eq!(
        query,
        PickQuery::At {
            point: [60.0, 30.0]
        }
    );

    picking.event(&button(true), later, false);
    picking.event(&button(false), later, false);
    assert!(!picking.picker.is_open(), "a click picks and ends");
    let (request, query) = picking.asked();
    assert_eq!(
        query,
        PickQuery::Capture {
            parts: vec![PartToCapture {
                path: "Root[0]/List[0]/Row[1]".into(),
                rect: [0.0, 20.0, 500.0, 60.0],
            }],
        },
        "the deepest part is read again where the window has it"
    );
    assert!(picking.picker.take_report(&cx()).is_none(), "still reading");
    let captured = PickAnswer::Captured {
        parts: vec![CapturedPart {
            path: "Root[0]/List[0]/Row[1]".into(),
            role: Some("ListItem".into()),
            label: None,
            texts: vec![
                CapturedText {
                    text: "Thanks Ada".into(),
                    interface: false,
                    rect: Some([10.0, 25.0, 200.0, 45.0]),
                },
                CapturedText {
                    text: "Reply".into(),
                    interface: true,
                    rect: None,
                },
            ],
            masked: vec![[10.0, 25.0, 200.0, 45.0]],
        }],
        recent: vec![RecentAction {
            at: 5,
            resource: "chat.message".into(),
            action: "send".into(),
            done: true,
        }],
    };
    assert!(picking.answer("so.tilo.chat", request, captured));
    let report = picking.picker.take_report(&cx()).expect("a report");
    let [pick] = report.picks.as_slice() else {
        panic!("one pick: {:?}", report.picks);
    };
    assert_eq!(
        (
            pick.app.as_str(),
            pick.app_name.as_str(),
            pick.role.as_str()
        ),
        ("so.tilo.chat", "Chat", "ListItem")
    );
    assert_eq!(pick.path, "Root[0]/List[0]/Row[1]");
    assert_eq!(pick.record.as_deref(), Some("chat.workspace"));
    assert_eq!(
        (pick.area.x, pick.area.y, pick.area.width),
        (100.0, 120.0, 500.0),
        "on the desktop"
    );
    let texts: Vec<(&str, TextKind)> = pick
        .texts
        .iter()
        .map(|text| (text.text.as_str(), text.kind))
        .collect();
    assert_eq!(
        texts,
        [
            ("Thanks Ada", TextKind::Content),
            ("Reply", TextKind::Interface)
        ]
    );
    let first = pick.texts[0].area.as_ref().expect("placed");
    assert_eq!((first.x, first.y), (110.0, 125.0));
    assert_eq!(report.interactions.len(), 1);
    assert_eq!(report.interactions[0].app, "so.tilo.chat");
    assert_eq!(report.names, ["Ada Lovelace"]);
    assert!(picking.picker.take_report(&cx()).is_none(), "once");
}

#[test]
fn a_dragged_release_waits_for_what_the_window_has_inside() {
    let mut picking = Picking::open();
    let from = Point::new(110.0, 110.0);
    let to = Point::new(400.0, 300.0);
    picking.event(&moved(from), from, false);
    let _ = picking.picker.take_outbox();
    picking.event(&button(true), from, false);
    picking.event(&moved(to), to, false);
    assert!(
        picking.picker.take_outbox().is_empty(),
        "a drag asks nothing on the way"
    );
    picking.event(&button(false), to, false);
    let (request, query) = picking.asked();
    let PickQuery::Within { area, limit } = query else {
        panic!("what is inside: {query:?}");
    };
    assert_eq!(area, [10.0, 10.0, 300.0, 200.0]);
    assert_eq!(limit, 6);
    assert!(picking.picker.is_open());
    let inside = PickAnswer::Targets {
        parts: vec![
            part("Root[0]/Text[0]", "StaticText", [20.0, 20.0, 120.0, 40.0]),
            part("Root[0]/Button[0]", "Button", [20.0, 60.0, 120.0, 90.0]),
        ],
    };
    assert!(picking.answer("so.tilo.chat", request, inside));
    assert!(picking.picker.is_open(), "a drag adds without ending");
    let keys: Vec<&str> = picking
        .picker
        .picker
        .selected()
        .iter()
        .map(|target| target.key.as_str())
        .collect();
    assert_eq!(keys, ["@9:Root[0]/Text[0]", "@9:Root[0]/Button[0]"]);
    assert_eq!(
        picking.picker.picker.selected()[1].caption,
        "Chat \u{b7} Button"
    );
}

#[test]
fn a_window_that_does_not_answer_in_time_still_gives_its_picks_and_the_shell_reads_its_own() {
    let mut picking = Picking::open();
    // The shell's own button, read from its tree at once.
    let bar = Point::new(50.0, 20.0);
    picking.event(&moved(bar), bar, false);
    assert!(
        picking.picker.take_outbox().is_empty(),
        "nothing to ask there"
    );
    picking.event(&button(true), bar, true);
    picking.event(&button(false), bar, true);
    // And a part of Chat's window.
    let at = Point::new(150.0, 130.0);
    picking.event(&moved(at), at, false);
    let (request, _) = picking.asked();
    picking.answer(
        "so.tilo.chat",
        request,
        PickAnswer::Targets {
            parts: vec![part("Root[0]/Row[0]", "ListItem", [0.0, 0.0, 500.0, 50.0])],
        },
    );
    picking.event(&button(true), at, false);
    picking.event(&button(false), at, false);
    let (_, query) = picking.asked();
    assert!(matches!(query, PickQuery::Capture { .. }));
    assert!(picking.picker.take_report(&cx()).is_none());
    let late = picking.now + ANSWER_WAIT;
    assert!(picking.picker.tick(&picking.root, late));
    let report = picking.picker.take_report(&cx()).expect("a report");
    let picked: Vec<(&str, &str, &str)> = report
        .picks
        .iter()
        .map(|pick| {
            (
                pick.app.as_str(),
                pick.app_name.as_str(),
                pick.role.as_str(),
            )
        })
        .collect();
    assert_eq!(
        picked,
        [
            (tilo_desktop::feedback::PACKAGE, "Tilo", "Button"),
            ("so.tilo.chat", "Chat", "ListItem"),
        ]
    );
    assert_eq!(
        report.picks[0]
            .label
            .as_ref()
            .map(|label| (label.text.as_str(), label.kind)),
        Some(("Workspaces", TextKind::Interface)),
        "the shell's own copy"
    );
    assert!(
        report.picks[1].texts.is_empty(),
        "nothing read, nothing sent"
    );
}

#[test]
fn leaving_without_a_pick_reports_nothing() {
    let mut picking = Picking::open();
    // A click where nothing is named picks nothing and keeps picking.
    let empty = Point::new(800.0, 700.0);
    picking.event(&moved(empty), empty, false);
    picking.event(&button(true), empty, false);
    picking.event(&button(false), empty, false);
    assert!(picking.picker.is_open());
    assert!(picking.picker.take_outbox().is_empty());
    assert!(picking.picker.take_report(&cx()).is_none());
}
