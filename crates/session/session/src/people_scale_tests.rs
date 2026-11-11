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
            notes: Rc::new(BTreeMap::new),
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
            notes: Rc::new(BTreeMap::new),
            apps: None,
            handle,
            sync: None,
            // The page writes the agent in, as it does for the bar.
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
    let panel = placed.0.take().expect("the roster places its layer");
    let mut frame = ui::headless::FrameHarness::new(ui::State::new(), panel, (1280, 800));
    let labels = |frame: &ui::headless::FrameHarness| -> Vec<String> {
        frame
            .a11y_tree_update()
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect()
    };
    people.borrow().toggle();
    settle(&mut frame);
    let listed = labels(&frame);
    assert!(
        listed.iter().any(|label| label == "Follow Claude Code"),
        "the roster lists the agent the bar shows: {listed:?}"
    );
}
