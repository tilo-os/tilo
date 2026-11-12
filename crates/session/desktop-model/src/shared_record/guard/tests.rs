use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::ownership::{Act, Authors, SharedDesktop};
use crate::shared_record;
use crate::{DesktopTree, StripId, WindowWidth, Workspace, WorkspaceSlot};

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
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// A desktop of one strip (10, nobody's) holding a window per
/// `(tab, group, who)`.
fn desktop(windows: &[(u64, u64, &str)]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Lounge");
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
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        authors,
        records: Vec::new(),
        opened_by: std::collections::BTreeMap::new(),
    }
}

/// The windows on the workspace's desktop, in order.
fn windows(guard: &DesktopGuard) -> Vec<u64> {
    let desktop = shared_record::read(&guard.doc, None).unwrap();
    desktop.tree.workspaces[0].strips[0]
        .window_groups
        .iter()
        .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
        .collect()
}

/// A member's copy of the workspace's desktop record, as it was when made.
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

    /// Write `desktop` as one change: what the workspace gets, incrementally.
    fn write(&mut self, desktop: &SharedDesktop) -> DocumentMutation {
        let heads = self.0.get_heads();
        shared_record::write(&mut self.0, desktop).unwrap();
        self.0.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
    }
}

/// A workspace whose desktop holds Ada's window 1 and Ben's window 2.
fn workspace() -> DesktopGuard {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![0; 16]));
    shared_record::write(&mut doc, &desktop(&[(1, 100, ADA), (2, 200, BEN)])).unwrap();
    doc.commit();
    DesktopGuard::new(doc)
}

fn writer<'a>(
    person: &'a str,
    members: Option<&'a std::collections::BTreeSet<String>>,
    allows: &'a dyn Fn(&Touched) -> bool,
) -> Writer<'a> {
    Writer {
        person,
        members,
        allows,
    }
}

/// Only the workspace's moderator may act on what someone else made.
fn moderates(writer: &'static str) -> impl Fn(&Touched) -> bool {
    move |_| writer == MOD
}

#[test]
fn a_guest_closes_their_own_window_but_not_someone_elses() {
    let mut guard = workspace();
    let closes_bens = Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA)]));
    let refused = guard
        .decide(&closes_bens, &writer(ADA, None, &moderates(ADA)))
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
    guard
        .decide(&closes_hers, &writer(ADA, None, &moderates(ADA)))
        .unwrap();
    assert_eq!(windows(&guard), vec![2]);
}

#[test]
fn the_owner_closes_their_window_and_a_moderator_closes_anyones() {
    let mut guard = workspace();
    let closes_his = Page::of(&guard, 2).write(&desktop(&[(1, 100, ADA)]));
    guard
        .decide(&closes_his, &writer(BEN, None, &moderates(BEN)))
        .unwrap();
    let closes_adas = Page::of(&guard, 3).write(&desktop(&[]));
    guard
        .decide(&closes_adas, &writer(MOD, None, &moderates(MOD)))
        .unwrap();
    assert!(windows(&guard).is_empty());
}

#[test]
fn moving_someone_elses_window_is_refused() {
    let mut guard = workspace();
    // Ben's window put in front of Ada's: his order among others' is the
    // same, so this reads as Ada moving hers, and is admitted.
    let swapped = Page::of(&guard, 1).write(&desktop(&[(2, 200, BEN), (1, 100, ADA)]));
    guard
        .decide(&swapped, &writer(ADA, None, &moderates(ADA)))
        .unwrap();
    // Ben's window into Ada's group is a move.
    let moved = Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA), (2, 100, BEN)]));
    let refused = guard
        .decide(&moved, &writer(ADA, None, &moderates(ADA)))
        .unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. } if act.id == 2 && act.act == Act::Move)
    );
}

#[test]
fn a_page_writing_over_an_old_desktop_is_judged_by_what_it_changed() {
    let mut guard = workspace();
    let mut ada = Page::of(&guard, 1);
    // Ben opens window 3 while Ada's page is behind.
    let opens = Page::of(&guard, 2).write(&desktop(&[(1, 100, ADA), (2, 200, BEN), (3, 300, BEN)]));
    guard
        .decide(&opens, &writer(BEN, None, &moderates(BEN)))
        .unwrap();
    // Ada, not having seen it, closes her own: admitted, and Ben's new
    // window is untouched, since her change wrote only her entries.
    let closes = ada.write(&desktop(&[(2, 200, BEN)]));
    guard
        .decide(&closes, &writer(ADA, None, &moderates(ADA)))
        .unwrap();
    assert_eq!(windows(&guard), vec![2, 3]);
}

#[test]
fn claiming_someone_elses_window_is_refused() {
    let mut guard = workspace();
    let claims = Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA), (2, 200, ADA)]));
    let refused = guard
        .decide(&claims, &writer(ADA, None, &moderates(ADA)))
        .unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. } if act.id == 2 && act.act == Act::Attribute)
    );
}

/// A window opened in someone else's name would be theirs to keep: Ada may
/// not open one as Ben's.
#[test]
fn opening_a_window_in_someone_elses_name_is_refused() {
    let mut guard = workspace();
    let forged =
        Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA), (2, 200, BEN), (3, 300, BEN)]));
    let refused = guard
        .decide(&forged, &writer(ADA, None, &moderates(ADA)))
        .unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. } if act.id == 3 && act.act == Act::Attribute)
    );
    let opened =
        Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA), (2, 200, BEN), (3, 300, ADA)]));
    guard
        .decide(&opened, &writer(ADA, None, &moderates(ADA)))
        .unwrap();
    assert_eq!(windows(&guard), vec![1, 2, 3]);
}

/// A record route is a window's identity: once a window shows a record,
/// not even its maker binds it to another one, and the workspace keeps the one
/// it showed.
#[test]
fn not_even_its_maker_rebinds_a_windows_record() {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![0; 16]));
    let mut bound = desktop(&[(1, 100, ADA), (2, 200, BEN)]);
    bound.records = vec![(1, "tab:0000000000000001".into())];
    shared_record::write(&mut doc, &bound).unwrap();
    doc.commit();
    let mut guard = DesktopGuard::new(doc);
    let mut rebound = bound.clone();
    rebound.records = vec![(1, "tab:0000000000000012".into())];
    let rebinds = Page::of(&guard, 1).write(&rebound);
    assert_eq!(
        guard.decide(&rebinds, &writer(ADA, None, &moderates(MOD))),
        Err(DesktopRefusal::Rebound {
            writer: ADA.into(),
            window: 1,
        })
    );
    let held = shared_record::read(&guard.doc, None).unwrap();
    assert_eq!(
        held.records, bound.records,
        "the refused write left nothing"
    );
    // Closing the window lets go of its record; that is no rebinding.
    let closes = Page::of(&guard, 1).write(&desktop(&[(2, 200, BEN)]));
    guard
        .decide(&closes, &writer(ADA, None, &moderates(ADA)))
        .unwrap();
    assert_eq!(windows(&guard), vec![2]);
}

#[test]
fn a_write_that_is_not_automerge_changes_nothing() {
    let mut guard = workspace();
    let _ = guard.decide(
        &DocumentMutation::Incremental(vec![1, 2, 3]),
        &writer(ADA, None, &moderates(ADA)),
    );
    assert_eq!(windows(&guard), vec![1, 2]);
}

/// Ben has left the workspace: his window holds nobody, and Ada, who writes
/// there, may close it without moderating.
#[test]
fn a_window_of_someone_who_left_the_workspace_is_anyones_to_close() {
    let mut guard = workspace();
    let members: std::collections::BTreeSet<String> = [ADA.to_owned()].into();
    let closes_bens = Page::of(&guard, 1).write(&desktop(&[(1, 100, ADA)]));
    guard
        .decide(&closes_bens, &writer(ADA, Some(&members), &moderates(ADA)))
        .unwrap();
    assert_eq!(windows(&guard), vec![1]);
}

/// A write that builds on a change the holder has not received waits, and
/// is decided as its own author's once that change is here.
#[test]
fn a_write_ahead_of_what_it_builds_on_waits_and_is_decided_as_its_authors() {
    let mut held = workspace().doc().clone();
    let mut ben = Page::of(&DesktopGuard::new(held.clone()), 2);
    let opens = ben.write(&desktop(&[(1, 100, ADA), (2, 200, BEN), (3, 300, BEN)]));
    let closes_adas = ben.write(&desktop(&[(2, 200, BEN), (3, 300, BEN)]));
    assert_eq!(
        admit_complete(&mut held, &closes_adas, &writer(BEN, None, &moderates(BEN))),
        Ok(Admitted::Waiting),
        "it builds on Ben's opening, which has not arrived"
    );
    assert_eq!(
        admit_complete(&mut held, &opens, &writer(BEN, None, &moderates(BEN))),
        Ok(Admitted::Merged(true))
    );
    assert!(matches!(
        admit_complete(&mut held, &closes_adas, &writer(BEN, None, &moderates(BEN))),
        Err(DesktopRefusal::NotTheirs { act, .. }) if act.id == 1 && act.act == Act::Close
    ));
}

/// What the door's own device writes (a window a member's agent opened through the host) is
/// followed, never decided: a guest whose page holds it and sends everything it holds is judged
/// by what the guest changed. Unfollowed, Ben's window would read as one the guest attributed
/// to him.
#[test]
fn a_window_the_door_itself_opened_is_not_judged_as_the_next_writers() {
    const GUEST: &str = "guest";
    let base = workspace();
    let mut followed = DesktopGuard::new(base.doc().clone());
    let mut unfollowed = DesktopGuard::new(base.doc().clone());
    // The host writes Ben's agent's window 3 itself.
    let mut host = Page::of(&base, 9);
    let opened = host.write(&desktop(&[(1, 100, ADA), (2, 200, BEN), (3, 300, BEN)]));
    followed.absorb_write(&opened).unwrap();
    // A guest's page holds it and sends the whole document, nothing of its own in it.
    let mut guest = Page(host.0.with_actor(automerge::ActorId::from(vec![5; 16])));
    let whole = DocumentMutation::Snapshot(guest.0.save());
    let refused = unfollowed
        .decide(&whole, &writer(GUEST, None, &moderates(GUEST)))
        .unwrap_err();
    assert!(
        matches!(&refused, DesktopRefusal::NotTheirs { act, .. }
            if act.id == 3 && act.act == Act::Attribute && act.owner.as_deref() == Some(BEN)),
        "{refused}"
    );
    followed
        .decide(&whole, &writer(GUEST, None, &moderates(GUEST)))
        .unwrap();
    assert_eq!(windows(&followed), vec![1, 2, 3]);
}

/// A window `id` opened in `strip`'s first group, as a member's page adds it.
fn open_window(desktop: &mut SharedDesktop, id: u64, who: &str, record: &str) {
    let strip = &mut desktop.tree.workspaces[0].strips[0];
    strip.window_groups.push(WindowGroup {
        // A group id of its own per window; test window ids are small.
        id: id.saturating_mul(100),
        tabs: vec![tab(id)],
        active_tab: id,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    desktop.authors.insert(id, who.to_owned());
    desktop.records.push((id, record.to_owned()));
}

/// A desk holding Ada's window 1 on the package primary (`""`, the primary
/// record's route).
fn primary_workspace() -> DesktopGuard {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![0; 16]));
    let mut desk = desktop(&[(1, 100, ADA)]);
    desk.records = vec![(1, String::new())];
    shared_record::write(&mut doc, &desk).unwrap();
    doc.commit();
    DesktopGuard::new(doc)
}

/// Two people open one shared desk: Ben joins, takes the records the desk
/// shows for its windows, opens one of his own, and writes it back. The
/// window Ada opened arrives with its record, and both can keep working.
#[test]
fn a_member_who_joins_a_shared_desk_opens_a_window_on_top_of_its_records() {
    let mut guard = primary_workspace();
    let mut ben = Page::of(&guard, 2);
    let mut adopted = shared_record::read(guard.doc(), None).unwrap();
    assert_eq!(adopted.records, vec![(1, String::new())]);
    open_window(&mut adopted, 3, BEN, "derived:default@tab:0000000000000003");
    let opens = ben.write(&adopted);
    guard
        .decide(&opens, &writer(BEN, None, &moderates(BEN)))
        .unwrap();
    assert_eq!(windows(&guard), vec![1, 3], "Ben sees Ada's window too");
    let seen = shared_record::read(guard.doc(), None).unwrap();
    assert_eq!(
        seen.records, adopted.records,
        "the desk holds each window's record as it showed it"
    );

    // Ada's page, joining on the desk Ben left, writes her own change.
    let mut ada = Page::of(&guard, 1);
    let mut adopted = shared_record::read(guard.doc(), None).unwrap();
    adopted.tree.workspaces[0].strips[0].window_groups[0].tabs[0].title = "Lounge renamed".into();
    let renames = ada.write(&adopted);
    guard
        .decide(&renames, &writer(ADA, None, &moderates(ADA)))
        .unwrap();
    assert_eq!(windows(&guard), vec![1, 3]);
}

/// A page that leaves out a window's record binds it to another one (none):
/// refused, as before.
#[test]
fn a_member_who_joins_without_the_desks_records_is_refused() {
    let mut guard = primary_workspace();
    let mut ben = Page::of(&guard, 2);
    let mut adopted = shared_record::read(guard.doc(), None).unwrap();
    adopted.records.clear();
    open_window(&mut adopted, 3, BEN, "derived:default@tab:0000000000000003");
    let forgets = ben.write(&adopted);
    assert_eq!(
        guard.decide(&forgets, &writer(BEN, None, &moderates(BEN))),
        Err(DesktopRefusal::Rebound {
            writer: BEN.into(),
            window: 1,
        })
    );
    assert_eq!(windows(&guard), vec![1], "the refused write left nothing");
}

/// A desk written before pages named records by their route holds the package
/// primary's custody key for it. It reads as the primary's route, so the page
/// that writes the desk next restates the same record instead of rebinding it.
#[test]
fn a_desk_holding_the_primarys_custody_key_reads_and_heals_to_its_route() {
    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(vec![0; 16]));
    let mut desk = desktop(&[(1, 100, ADA)]);
    desk.records = vec![(1, "@tilo:package-primary".into())];
    shared_record::write(&mut doc, &desk).unwrap();
    doc.commit();
    let mut guard = DesktopGuard::new(doc);
    let mut ada = Page::of(&guard, 1);
    let adopted = shared_record::read(guard.doc(), None).unwrap();
    assert_eq!(
        adopted.records,
        vec![(1, String::new())],
        "the custody key reads as the primary's route"
    );
    let heals = ada.write(&adopted);
    guard
        .decide(&heals, &writer(ADA, None, &moderates(ADA)))
        .unwrap();
    assert_eq!(
        shared_record::read(guard.doc(), None).unwrap().records,
        vec![(1, String::new())]
    );
}
