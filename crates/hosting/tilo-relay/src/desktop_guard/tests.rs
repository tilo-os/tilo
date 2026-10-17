use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{Act, Authors};
use tilo_desktop_model::{DesktopTree, StripId, WindowWidth, Workspace, WorkspaceId};

use super::*;

const ADA: &str = "ada";
const BEN: &str = "ben";
const MOD: &str = "moderator";

fn tab(id: u64) -> Tab {
    Tab {
        id,
        title: format!("tab {id}"),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
    }
}

/// A desktop of one strip (10, nobody's) holding a window per
/// `(tab, group, who)`.
fn desktop(windows: &[(u64, u64, &str)]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
    let mut strip = Strip::new(StripId(10), "Lounge");
    for (id, group, _) in windows {
        match strip.window_groups.iter_mut().find(|g| g.id == *group) {
            Some(existing) => existing.tabs.push(tab(*id)),
            None => strip.window_groups.push(WindowGroup {
                id: *group,
                tabs: vec![tab(*id)],
                active_tab: *id,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }),
        }
    }
    workspace.strips.push(strip);
    let authors: Authors = windows
        .iter()
        .map(|(id, _, who)| (*id, (*who).to_owned()))
        .collect();
    SharedDesktop {
        tree: DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        authors,
        records: Vec::new(),
    }
}

/// The windows on the room's desktop, in order.
fn windows(guard: &DesktopGuard) -> Vec<u64> {
    let desktop = shared_record::read(&guard.doc, None).unwrap();
    desktop.tree.workspaces[0].strips[0]
        .window_groups
        .iter()
        .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
        .collect()
}

/// A member's copy of the room's desktop record, as it was when made.
struct Page(AutoCommit);

impl Page {
    fn of(guard: &DesktopGuard, actor: u8) -> Self {
        Self(
            guard
                .doc
                .clone()
                .with_actor(automerge::ActorId::from(vec![actor; 16])),
        )
    }

    /// Write `desktop` as one change: what the room gets, incrementally.
    fn write(&mut self, desktop: &SharedDesktop) -> DocumentMutation {
        let heads = self.0.get_heads();
        shared_record::write(&mut self.0, desktop).unwrap();
        self.0.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
    }
}

/// A room whose desktop holds Ada's window 1 and Ben's window 2.
fn room() -> DesktopGuard {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![0; 16]));
    shared_record::write(&mut doc, &desktop(&[(1, 100, ADA), (2, 200, BEN)])).unwrap();
    doc.commit();
    DesktopGuard::new(doc)
}

/// Only the room's moderator may act on what someone else made.
fn moderates(writer: &'static str) -> impl Fn(&Touched) -> bool {
    move |_| writer == MOD
}

#[test]
fn a_guest_closes_their_own_window_but_not_someone_elses() {
    let mut guard = room();
    let closes_bens = Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA)]));
    let refused = guard
        .decide(&closes_bens, ADA, &moderates(ADA))
        .unwrap_err();
    assert!(matches!(
        &refused,
        DesktopRefusal::NotTheirs { act, .. }
            if act.id == 2 && act.act == Act::Close && act.owner.as_deref() == Some(BEN)
    ));
    assert_eq!(
        windows(&guard),
        vec![1, 2],
        "refused, the desktop is as it was"
    );

    let closes_hers = Page::of(&guard, 1).write(&desktop(&[(2, 200, BEN)]));
    guard.decide(&closes_hers, ADA, &moderates(ADA)).unwrap();
    assert_eq!(windows(&guard), vec![2]);
}

#[test]
fn the_owner_closes_their_window_and_a_moderator_closes_anyones() {
    let mut guard = room();
    let closes_his = Page::of(&guard, 2).write(&desktop(&[(1, 100, ADA)]));
    guard.decide(&closes_his, BEN, &moderates(BEN)).unwrap();
    let closes_adas = Page::of(&guard, 3).write(&desktop(&[]));
    guard.decide(&closes_adas, MOD, &moderates(MOD)).unwrap();
    assert!(windows(&guard).is_empty());
}

#[test]
fn moving_someone_elses_window_is_refused() {
    let mut guard = room();
    // Ben's window put in front of Ada's: his order among others' is the
    // same, so this reads as Ada moving hers, and is admitted.
    let swapped = Page::of(&guard, 1).write(&desktop(&[(2, 200, BEN), (1, 100, ADA)]));
    guard.decide(&swapped, ADA, &moderates(ADA)).unwrap();
    // Ben's window into Ada's group is a move.
    let moved = Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA), (2, 100, BEN)]));
    let refused = guard.decide(&moved, ADA, &moderates(ADA)).unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. } if act.id == 2 && act.act == Act::Move)
    );
}

#[test]
fn a_page_writing_over_an_old_desktop_is_judged_by_what_it_changed() {
    let mut guard = room();
    let mut ada = Page::of(&guard, 1);
    // Ben opens window 3 while Ada's page is behind.
    let opens = Page::of(&guard, 2).write(&desktop(&[(1, 100, ADA), (2, 200, BEN), (3, 300, BEN)]));
    guard.decide(&opens, BEN, &moderates(BEN)).unwrap();
    // Ada, not having seen it, closes her own: admitted, and Ben's new
    // window is untouched, since her change wrote only her entries.
    let closes = ada.write(&desktop(&[(2, 200, BEN)]));
    guard.decide(&closes, ADA, &moderates(ADA)).unwrap();
    assert_eq!(windows(&guard), vec![2, 3]);
}

#[test]
fn claiming_someone_elses_window_is_refused() {
    let mut guard = room();
    let claims = Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA), (2, 200, ADA)]));
    let refused = guard.decide(&claims, ADA, &moderates(ADA)).unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. } if act.id == 2 && act.act == Act::Attribute)
    );
}

#[test]
fn a_write_that_is_not_automerge_changes_nothing() {
    let mut guard = room();
    let _ = guard.decide(
        &DocumentMutation::Incremental(vec![1, 2, 3]),
        ADA,
        &moderates(ADA),
    );
    assert_eq!(windows(&guard), vec![1, 2]);
}
