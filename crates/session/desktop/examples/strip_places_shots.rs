//! Captures of the top bar's strips: a workspace's named places with how
//! many people are on each, switching to one, and naming a new one.
//! Run with an output directory.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a capture treats a failed setup as fatal"
)]

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use tilo_desktop::chrome::{ChromeIntent, DesktopChromeSnapshot, DesktopChromeSource, StripPlaces};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{Collaborator, DesktopTree, Strip, StripId, Workspace, WorkspaceId};
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget as _;

/// A page's desktop: its tree, and who of the room is on which strip.
struct Room {
    tree: RefCell<DesktopTree>,
    others: Vec<StripId>,
    /// Moves whenever the desktop changes, as a page's does.
    revision: Cell<u64>,
}

impl DesktopChromeSource for Room {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        DesktopChromeSnapshot {
            strip_places: Some(StripPlaces::in_active_workspace(
                &self.tree.borrow(),
                &self.others,
            )),
            revision: self.revision.get(),
            ..Default::default()
        }
    }

    fn tree(&self) -> DesktopTree {
        self.tree.borrow().clone()
    }

    fn intent(&self, intent: ChromeIntent) -> bool {
        self.revision.set(self.revision.get().wrapping_add(1));
        let mut tree = self.tree.borrow_mut();
        let workspace = tree.active;
        match intent {
            ChromeIntent::SwitchStrip(strip) => {
                tree.active_strip.insert(workspace, strip);
                true
            }
            ChromeIntent::AddStrip { name } => {
                let id = StripId(self.revision.get().wrapping_add(100));
                if let Some(first) = tree.workspaces.first_mut() {
                    first.strips.push(Strip::new(id, name));
                }
                tree.active_strip.insert(workspace, id);
                true
            }
            _ => false,
        }
    }
}

fn tree() -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(1), "Hangout");
    workspace.strips = vec![
        Strip::new(StripId(10), "Main"),
        Strip::new(StripId(11), "Games"),
        Strip::new(StripId(12), "Quiet"),
    ];
    workspace.collaborators = vec![
        Collaborator::new("ada", "Ada").with_session(7),
        Collaborator::new("lin", "Lin").with_session(9),
        Collaborator::new("sam", "Sam").with_session(11),
    ];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(10));
    tree
}

fn bar(width: u32, theme: Theme) -> Framed<TopBarChrome> {
    let room = Rc::new(Room {
        tree: RefCell::new(tree()),
        // Ada with me on Main; Lin and Sam on Games.
        others: vec![StripId(10), StripId(11), StripId(11)],
        revision: Cell::new(1),
    });
    let bar = TopBarChrome::from_tree_with_local_user(&room.tree(), "me".into()).with_chrome(room);
    Framed::new(ui::State::new(), bar, (width, 56), theme)
}

fn rect(frame: &Framed<TopBarChrome>, name: &str) -> vello::kurbo::Rect {
    frame
        .test_rects()
        .into_iter()
        .find_map(|(key, rect)| (key == name).then_some(rect))
        .expect("the bar shows it")
}

fn click(frame: &mut Framed<TopBarChrome>, name: &str) {
    let at = rect(frame, name).center();
    Framed::harness_mut(frame).click_at(at.x, at.y);
    Framed::harness_mut(frame).poll_frame().unwrap();
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("provide output directory")?;
    let out = Path::new(&directory);
    std::fs::create_dir_all(out)?;
    for (name, width, theme) in [
        ("dark", 1440, Theme::dark()),
        ("light", 1440, Theme::light()),
        ("narrow", 420, Theme::dark()),
    ] {
        let mut frame = bar(width, theme);
        Framed::harness(&frame).render_png(&out.join(format!("{name}-counts.png")));
        if name == "narrow" {
            continue;
        }
        click(&mut frame, "topbar_strip_Games");
        Framed::harness(&frame).render_png(&out.join(format!("{name}-switched.png")));
        // New desk, as the workspace menu asks for it: named in place.
        frame.begin_new_desk();
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        click(&mut frame, "topbar_strip_name_field");
        Framed::harness_mut(&mut frame).type_text("Games night");

        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        Framed::harness(&frame).render_png(&out.join(format!("{name}-naming.png")));
        Framed::harness_mut(&mut frame).key("Enter");
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        Framed::harness(&frame).render_png(&out.join(format!("{name}-added.png")));
    }
    // Less room than the strips need: the ones left out are a "+N" chip.
    for width in [1180, 1280] {
        let frame = bar(width, Theme::dark());
        Framed::harness(&frame).render_png(&out.join(format!("tight-{width}.png")));
    }
    Ok(())
}
