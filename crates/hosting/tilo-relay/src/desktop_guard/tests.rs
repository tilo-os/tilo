use automerge::transaction::Transactable as _;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::Authors;
use tilo_desktop_model::{DesktopTree, WindowWidth, Workspace};

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

/// A desktop of one strip (10, nobody's) holding a window per `(tab, who)`,
/// each in its own group (`tab * 100`).
fn desktop(windows: &[(u64, &str)]) -> String {
    let mut workspace = Workspace::new(WorkspaceId(1), "Lounge");
    let mut strip = Strip::new(StripId(10), "Lounge");
    strip.window_groups = windows
        .iter()
        .map(|(id, _)| WindowGroup {
            id: id.saturating_mul(100),
            tabs: vec![tab(*id)],
            active_tab: *id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        })
        .collect();
    workspace.strips.push(strip);
    let tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    let authors: Authors = windows
        .iter()
        .map(|(id, who)| (*id, (*who).to_owned()))
        .collect();
    serde_json::json!({ "tree": tree, "records": [], "authors": authors }).to_string()
}

/// The windows on the room's desktop, in order.
fn windows(guard: &DesktopGuard) -> Vec<u64> {
    let desktop = desktop_at(&guard.doc, None).unwrap().unwrap();
    desktop.tree.workspaces[0].strips[0]
        .window_groups
        .iter()
        .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
        .collect()
}

/// A member's copy of the room's desktop record.
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
    fn write(&mut self, desktop: &str) -> DocumentMutation {
        let heads = self.0.get_heads();
        self.0.put(ROOT, DESKTOP_KEY, desktop).unwrap();
        self.0.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
    }
}

/// A room whose desktop holds Ada's window 1 and Ben's window 2, settled.
fn room() -> DesktopGuard {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![0; 16]));
    doc.put(ROOT, DESKTOP_KEY, desktop(&[(1, ADA), (2, BEN)]))
        .unwrap();
    doc.commit();
    DesktopGuard::new(doc)
}

/// Only the room's moderator may act on what someone else made.
fn moderates(writer: &'static str) -> impl Fn(&Touched) -> bool {
    move |_| writer == MOD
}

const LATER: u64 = 1_000_000;

#[test]
fn a_guest_closes_their_own_window_but_not_someone_elses() {
    let mut guard = room();
    let mut ada = Page::of(&guard, 1);
    let closes_bens = ada.write(&desktop(&[(1, ADA)]));
    let refused = guard
        .decide(&closes_bens, ADA, LATER, &moderates(ADA))
        .unwrap_err();
    assert!(matches!(
        &refused,
        DesktopRefusal::NotTheirs { act, .. }
            if act.id == 2 && act.act == Act::Close && act.owner.as_deref() == Some(BEN)
    ));
    // Refused, the room's desktop is as it was.
    assert_eq!(windows(&guard), vec![1, 2]);

    let mut ada = Page::of(&guard, 1);
    let closes_hers = ada.write(&desktop(&[(2, BEN)]));
    guard
        .decide(&closes_hers, ADA, LATER, &moderates(ADA))
        .unwrap();
    assert_eq!(windows(&guard), vec![2]);
}

#[test]
fn the_owner_closes_their_window_and_a_moderator_closes_anyones() {
    let mut guard = room();
    let mut ben = Page::of(&guard, 2);
    let closes_his = ben.write(&desktop(&[(1, ADA)]));
    guard
        .decide(&closes_his, BEN, LATER, &moderates(BEN))
        .unwrap();

    let mut moderator = Page::of(&guard, 3);
    let closes_adas = moderator.write(&desktop(&[]));
    guard
        .decide(&closes_adas, MOD, LATER, &moderates(MOD))
        .unwrap();
    assert!(windows(&guard).is_empty());
}

#[test]
fn moving_someone_elses_window_is_refused() {
    let mut guard = room();
    let mut ada = Page::of(&guard, 1);
    // Ben's window put in front of Ada's: his order among others' is the
    // same, so this alone reads as Ada moving hers, and is admitted.
    let swapped = ada.write(&desktop(&[(2, BEN), (1, ADA)]));
    guard.decide(&swapped, ADA, LATER, &moderates(ADA)).unwrap();

    // Ben's window into Ada's group is a move.
    let mut value: serde_json::Value =
        serde_json::from_str(&desktop(&[(1, ADA), (2, BEN)])).unwrap();
    let groups = &mut value["tree"]["workspaces"][0]["strips"][0]["window_groups"];
    let bens = groups[1]["tabs"][0].clone();
    groups[0]["tabs"].as_array_mut().unwrap().push(bens);
    groups.as_array_mut().unwrap().remove(1);
    let mut ada = Page::of(&guard, 1);
    let moved = ada.write(&value.to_string());
    let refused = guard
        .decide(&moved, ADA, LATER, &moderates(ADA))
        .unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. } if act.id == 2 && act.act == Act::Move)
    );
}

/// Ada's page (actor 0xee) wins a last-writer race against Ben's (actor 9):
/// both changes follow the same desktop, so the higher actor's value is the
/// room's.
#[test]
fn a_race_against_someone_opening_a_window_is_not_their_act() {
    let mut guard = room();
    let mut ada = Page::of(&guard, 0xee);
    let mut ben = Page::of(&guard, 9);
    let opens = ben.write(&desktop(&[(1, ADA), (2, BEN), (3, BEN)]));
    guard.decide(&opens, BEN, LATER, &moderates(BEN)).unwrap();
    // Ada, not having seen window 3, closes her own. Her value replaces
    // Ben's, so window 3 is gone: not her act, and it only just appeared.
    let closes = ada.write(&desktop(&[(2, BEN)]));
    guard
        .decide(&closes, ADA, LATER + 1000, &moderates(ADA))
        .unwrap();
    assert_eq!(windows(&guard), vec![2]);
}

#[test]
fn replaying_an_old_desktop_over_settled_windows_is_refused() {
    let mut guard = room();
    let mut ada = Page::of(&guard, 0xee);
    let mut ben = Page::of(&guard, 9);
    // Ben opens window 3, and it stays there.
    let opens = ben.write(&desktop(&[(1, ADA), (2, BEN), (3, BEN)]));
    guard.decide(&opens, BEN, LATER, &moderates(BEN)).unwrap();
    // Long after, Ada writes over the desktop as it was before window 3.
    let stale = ada.write(&desktop(&[(1, ADA), (2, BEN)]));
    let settle = u64::try_from(SETTLE.as_millis()).unwrap();
    let mut later = DesktopGuard {
        doc: guard.doc.clone(),
        placed: guard.placed.clone(),
    };
    let refused = later
        .decide(&stale, ADA, LATER + settle + 1, &moderates(ADA))
        .unwrap_err();
    assert!(matches!(
        &refused,
        DesktopRefusal::NotTheirs { act, .. } if act.id == 3 && act.act == Act::Close
    ));
    // A moderator's stale write may, and within the settling time it is
    // an ordinary race.
    later
        .decide(&stale, MOD, LATER + settle + 1, &moderates(MOD))
        .unwrap();
    guard
        .decide(&stale, ADA, LATER + 1, &moderates(ADA))
        .unwrap();
}

#[test]
fn claiming_someone_elses_window_is_refused() {
    let mut guard = room();
    let mut ada = Page::of(&guard, 1);
    let claims = ada.write(&desktop(&[(1, ADA), (2, ADA)]));
    let refused = guard
        .decide(&claims, ADA, LATER, &moderates(ADA))
        .unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. } if act.id == 2 && act.act == Act::Attribute)
    );
}

#[test]
fn a_write_that_is_not_a_desktop_is_refused() {
    let mut guard = room();
    let mut ada = Page::of(&guard, 1);
    let garbage = ada.write("{not a desktop");
    assert!(matches!(
        guard.decide(&garbage, ADA, LATER, &moderates(ADA)),
        Err(DesktopRefusal::Unreadable(_))
    ));
}
