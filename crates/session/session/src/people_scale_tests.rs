//! The roster's own buttons under a pointer on a screen at twice the scale: the panel is handed
//! window events in physical pixels and lays out in logical ones, and kept on screen it stands
//! where its controls are said to stand before the next press.

use super::*;
use crate::platform::surfaces::{Heard, Layer, Placement, Surfaces};
use tilo_desktop_model::{Collaborator, DesktopTree, Tab, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_desktop_surface::{DesktopSurface, TileSource};
use ui::widget::{TileContent, TileHost};

/// The host's side of the one layer the roster places: the tree it hands over, held for the
/// frame below.
#[derive(Default)]
struct Placed(Option<Box<dyn Widget>>);

struct Quiet;

impl Layer for Quiet {
    fn relayout(&self) {}
    fn set_theme(&self, _theme: Theme) {}
    fn revision(&self) -> u64 {
        0
    }
    fn a11y_tree(&self) -> Option<ui::accesskit::TreeUpdate> {
        None
    }
    fn a11y_action(&self, _target: ui::accesskit::NodeId, _action: &ui::a11y::Action) -> bool {
        false
    }
    fn accessible_labels(&self) -> Vec<String> {
        Vec::new()
    }
    fn controls(&self) -> Vec<(String, (f64, f64, f64, f64))> {
        Vec::new()
    }
    fn press(&self, name: &str) -> Result<(), String> {
        Err(format!("no control named {name}"))
    }
}

impl Surfaces for Placed {
    fn place(
        &mut self,
        _name: &str,
        tree: Box<dyn Widget>,
        _placement: Placement,
        _heard: Heard,
    ) -> Rc<dyn Layer> {
        self.0 = Some(tree);
        Rc::new(Quiet)
    }

    fn publish(&mut self, _published: Rc<RefCell<dyn Published>>) {}
}

struct Blank;

impl TileSource for Blank {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

/// Where the control `label` stands, in the logical pixels a click is given in.
fn holding(frame: &ui::headless::FrameHarness, label: &str) -> Option<Point> {
    let tree = frame.a11y_tree_update();
    tree.nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label))
        .and_then(|(_, node)| node.bounds())
        .map(|bounds| {
            Point::new(
                f64::midpoint(bounds.x0, bounds.x1),
                f64::midpoint(bounds.y0, bounds.y1),
            )
        })
}

fn settle(frame: &mut ui::headless::FrameHarness) {
    for _ in 0..4 {
        let _ = frame.poll_frame();
    }
}

/// Keep on screen and Put back in the bar, each pressed by the pointer where the panel says it
/// stands: a press read in physical pixels missed the first, and the panel moving to where its
/// drawn size centres it only on the press's own frame moved the second from under the pointer.
#[test]
fn keep_on_screen_and_put_back_take_the_pointer_at_twice_the_scale() {
    let _settled = ui::motion::testing::settled();
    // The pin is kept in the person's settings: this test's own, never the machine's.
    let home = tempfile::tempdir().expect("a directory for the settings");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", home.path().join("config"))
        .set("XDG_STATE_HOME", home.path().join("state"));

    // Someone else is here: kept on screen with nobody to show, the roster waits away from the
    // desk, and there is no panel of it whose Put back in the bar a pointer could press.
    let here = WorkspaceId::new();
    let mut desk = Workspace::in_workspace(WorkspaceSlot::of_workspace(here), here, "Desk");
    desk.collaborators.push(Collaborator {
        id: "ada".to_owned(),
        name: "Ada".to_owned(),
        ..Collaborator::default()
    });
    let (_desk, handle) = DesktopSurface::new(
        DesktopTree {
            active: desk.slot,
            workspaces: vec![desk],
            ..DesktopTree::default()
        },
        7,
        Box::new(Blank),
    );
    let mut placed = Placed::default();
    let people = mount(
        &mut placed,
        Given {
            scale: 2.0,
            seated: "Quinten".into(),
            notes: Rc::new(Vec::new),
            apps: None,
            handle,
            sync: None,
            here: Rc::new(|_: &mut DesktopTree| {}),
            intents: Rc::default(),
            invitable: Rc::new(Cell::new(false)),
        },
    );
    let panel = placed.0.take().expect("the roster places its layer");
    let mut frame = ui::headless::FrameHarness::new(ui::State::new(), panel, (1280, 800));
    frame.set_input_scale(2.0).expect("a scale");
    people.borrow().toggle();
    settle(&mut frame);

    let keep = holding(&frame, "Keep on screen").expect("the open roster offers to stay");
    frame.click_at(keep.x, keep.y);
    settle(&mut frame);
    let put = holding(&frame, "Put back in the bar")
        .expect("the press on Keep on screen kept the roster on screen");

    frame.click_at(put.x, put.y);
    settle(&mut frame);
    assert!(
        people.borrow().shown.borrow().pinned.is_none(),
        "the press on Put back in the bar, where it is said to stand, took it off the desk"
    );
}

/// The roster lists whoever the bar's faces are drawn from: on a page, an agent's run the
/// workspace's host speaks for is heard on a track of its own and is in the desk's collaborators
/// only as the page writes them in, and the roster lists it with its Follow.
#[test]
fn the_roster_lists_who_the_bar_shows() {
    let _settled = ui::motion::testing::settled();
    let (people, mut frame) = agent_here().expect("the roster places its layer");
    people.borrow().toggle();
    settle(&mut frame);
    let listed = labels(&frame);
    assert!(
        listed.iter().any(|label| label == "Follow Claude Code"),
        "the roster lists the agent the bar shows: {listed:?}"
    );
}

/// A press on someone's face opens their card where the roster stands; a second press on the same
/// face, their card standing, brings everyone here in its place. The bar has no people button while
/// it shows a face, so this is how a pointer reaches the roster.
#[test]
fn a_face_pressed_again_brings_everyone_here() {
    let _settled = ui::motion::testing::settled();
    let (people, mut frame) = agent_here().expect("the roster places its layer");
    let card = |labels: &[String]| labels.iter().any(|label| label == "Claude Code’s profile");
    people.borrow().show_person("run");
    settle(&mut frame);
    let shown = labels(&frame);
    assert!(
        card(&shown),
        "the first press opens the agent's card: {shown:?}"
    );

    people.borrow().show_person("run");
    settle(&mut frame);
    let shown = labels(&frame);
    assert!(!card(&shown), "the second press closes the card: {shown:?}");
    assert!(
        shown.iter().any(|label| label == "Follow Claude Code"),
        "and everyone here stands in its place: {shown:?}"
    );
    assert!(people.borrow().is_open());
}

/// Kept on screen on a phone, the roster stands centred across the desk with equal gutters, inside
/// it, whichever side edge it was kept at; on a wide desk it keeps to its edge.
#[test]
fn the_pinned_roster_is_centred_on_a_phone() {
    let theme = Theme::light();
    let now = ui::time::Instant::now();
    for edge in [DockEdge::Right, DockEdge::Left, DockEdge::BottomRight] {
        let mut pin = Pinned::new(edge, true);
        pin.panel = Rect::from_origin_size(Point::ZERO, Size::new(319.0, 300.0));
        Panel::place_pinned(&mut pin, Rect::new(0.0, 0.0, 390.0, 844.0), &theme, now);
        let (left, right) = (pin.panel.x0, 390.0 - pin.panel.x1);
        assert!(
            (left - right).abs() < 1.0 && left >= 0.0,
            "kept at {edge:?} on a phone, the roster stands centred inside the desk: {:?}",
            pin.panel
        );
    }
    let mut pin = Pinned::new(DockEdge::Right, true);
    pin.panel = Rect::from_origin_size(Point::ZERO, Size::new(319.0, 300.0));
    Panel::place_pinned(&mut pin, Rect::new(0.0, 0.0, 1280.0, 800.0), &theme, now);
    assert!(
        pin.panel.x1 > 1200.0,
        "on a wide desk it keeps to its edge: {:?}",
        pin.panel
    );
}

/// A desk whose page writes an agent in as here, as it does for the bar, and its roster's layer.
fn agent_here() -> Option<(Rc<RefCell<People>>, ui::headless::FrameHarness)> {
    let here = WorkspaceId::new();
    let desk = Workspace::in_workspace(WorkspaceSlot::of_workspace(here), here, "Desk");
    let (_desk, handle) = DesktopSurface::new(
        DesktopTree {
            active: desk.slot,
            workspaces: vec![desk],
            ..DesktopTree::default()
        },
        7,
        Box::new(Blank),
    );
    let mut placed = Placed::default();
    let people = mount(
        &mut placed,
        Given {
            scale: 1.0,
            seated: "Quinten".into(),
            notes: Rc::new(Vec::new),
            apps: None,
            handle,
            // A card is read from the session's model of the person.
            sync: Some(Rc::new(crate::sync::Client::untethered(
                iroh::SecretKey::from_bytes(&[0x51; 32]).public(),
                crate::sync::OwnPresence::default(),
            ))),
            here: Rc::new(|tree: &mut DesktopTree| {
                if let Some(workspace) = tree.workspaces.first_mut() {
                    workspace.collaborators = vec![Collaborator {
                        id: "run".to_owned(),
                        name: "Claude Code".to_owned(),
                        role_id: Some("agent".to_owned()),
                        ..Collaborator::default()
                    }];
                }
            }),
            intents: Rc::default(),
            invitable: Rc::new(Cell::new(false)),
        },
    );
    let panel = placed.0.take()?;
    let frame = ui::headless::FrameHarness::new(ui::State::new(), panel, (1280, 800));
    Some((people, frame))
}

fn labels(frame: &ui::headless::FrameHarness) -> Vec<String> {
    frame
        .a11y_tree_update()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}
