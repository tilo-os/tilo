//! Naming a host window's tab on the production host: the window menu
//! offers Rename tab for it alone, choosing it opens the name field in the
//! tab's own pill, typing goes to that field, and Enter records the name in
//! the tree while Escape leaves the title as it was.

use super::overlay_event_absorb_tests::lay_out_host_with_theme;
use super::tab_menu::TabMenuTarget;
use super::*;
use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::state::WorkspaceTree;
use crate::shell::window_menu::{WindowMenuAction, WindowMenuRow};
use taffy::{NodeId, TaffyTree};
use winit::event::WindowEvent;
use winit::keyboard::NamedKey;

const SURFACE: u64 = 0x5151;

struct Fixture {
    host: ShellOverlayHost,
    chrome: Chrome,
    taffy: TaffyTree<()>,
    root: NodeId,
    doc: AutoCommit,
    /// The terminal window's tab.
    host_tab: TabMenuTarget,
    /// A Tilo app's tab in the same strip.
    app_tab: TabMenuTarget,
}

/// The demo desktop with a terminal window adopted beside its Tilo apps.
fn fixture() -> Fixture {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let tab =
        crate::shell::tab_binding::adopt_toplevel(&mut tree, SURFACE, Some("Alacritty".into()));
    assert!(crate::shell::tab_binding::rename_tab_for_title(
        &mut tree, SURFACE, "~/src"
    ));
    let strip = &tree.desktop.workspaces[0].strips[0];
    let group_of = |id: u64| {
        strip
            .window_groups
            .iter()
            .find(|group| group.tabs.iter().any(|t| t.id == id))
            .map(|group| group.id)
            .expect("the tab is in the strip")
    };
    let app = strip.window_groups[0].tabs[0].id;
    let host_tab = TabMenuTarget {
        strip: strip.id,
        group: group_of(tab),
        tab,
    };
    let app_tab = TabMenuTarget {
        strip: strip.id,
        group: group_of(app),
        tab: app,
    };
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let (taffy, root) = lay_out_host_with_theme(&mut host, 1280.0, 800.0, &Theme::dark());
    let _ = host.take_structure_dirty();
    Fixture {
        host,
        chrome,
        taffy,
        root,
        doc: AutoCommit::new(),
        host_tab,
        app_tab,
    }
}

impl Fixture {
    fn send(&mut self, event: &WindowEvent) -> bool {
        ui::testing::Dispatch::new()
            .doc(&mut self.doc)
            .layout(&self.taffy, self.root)
            .send(&mut self.host, event)
            .handled
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            assert!(
                self.send(&ui::synthetic_input::char_key(&c.to_string())),
                "the name field takes {c:?}"
            );
        }
    }

    fn renaming(&mut self) -> Option<u64> {
        let target = self.host_tab;
        let index = self.chrome.borrow().tree().desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .position(|group| group.id == target.group)?;
        self.host
            .group_at_for_tests(target.strip, index)?
            .renaming_tab()
    }

    fn frame(&mut self) {
        let root = automerge::ROOT;
        self.host.update(&self.doc, &root);
    }

    fn title(&self) -> (String, bool) {
        let state = self.chrome.borrow();
        let tree = state.tree();
        let tab = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .flat_map(|group| &group.tabs)
            .find(|tab| tab.id == self.host_tab.tab)
            .expect("the terminal's tab is there");
        (tab.title.clone(), tab.is_named())
    }
}

fn offers_rename(rows: &[WindowMenuRow]) -> bool {
    rows.iter()
        .any(|row| row.action() == Some(WindowMenuAction::RenameTab))
}

#[test]
fn the_window_menu_offers_rename_for_a_host_window_and_not_for_a_tilo_app() {
    let f = fixture();
    assert!(offers_rename(
        &f.host.window_menu_rows(f.host_tab, 1, false)
    ));
    assert!(!offers_rename(
        &f.host.window_menu_rows(f.app_tab, 4, false)
    ));
}

#[test]
fn rename_tab_opens_the_field_in_the_pill_and_enter_names_the_tab() {
    let mut f = fixture();
    assert!(
        f.host
            .perform_tab_menu_action(f.host_tab, WindowMenuAction::RenameTab)
    );
    assert_eq!(f.renaming(), Some(f.host_tab.tab));
    assert!(f.host.tab_renaming(), "the shell holds the keyboard for it");

    f.type_text("Build");
    assert!(f.send(&ui::synthetic_input::named_key(NamedKey::Enter)));
    assert_eq!(f.renaming(), None);
    f.frame();
    assert_eq!(f.title(), ("Build".to_owned(), true));

    // The terminal goes on titling itself; the name stands.
    {
        let state = f.chrome.borrow();
        let mut tree = state.tree_mut();
        assert!(crate::shell::tab_binding::rename_tab_for_title(
            &mut tree, SURFACE, "vim"
        ));
    }
    assert_eq!(f.title(), ("Build".to_owned(), true));

    // An emptied field gives the window its own title back.
    assert!(
        f.host
            .perform_tab_menu_action(f.host_tab, WindowMenuAction::RenameTab)
    );
    assert!(f.send(&ui::synthetic_input::named_key(NamedKey::Backspace)));
    assert!(f.send(&ui::synthetic_input::named_key(NamedKey::Enter)));
    f.frame();
    assert_eq!(f.title(), ("vim".to_owned(), false));
}

#[test]
fn escape_leaves_the_title_as_it_was() {
    let mut f = fixture();
    assert!(
        f.host
            .perform_tab_menu_action(f.host_tab, WindowMenuAction::RenameTab)
    );
    f.type_text("Nope");
    assert!(f.send(&ui::synthetic_input::named_key(NamedKey::Escape)));
    assert_eq!(f.renaming(), None);
    f.frame();
    assert_eq!(f.title(), ("~/src".to_owned(), false));
}
