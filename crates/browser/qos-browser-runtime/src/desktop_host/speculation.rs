//! Optimistic local window reorders.
//!
//! A pure reorder (`ReorderGroup`, `ReorderTab`) is applied to the rendered
//! tree the frame it is queued, instead of after the owner commits it and the
//! committed room state comes back. The admitted tree stays the durable
//! basis: every admission, and every retirement of an applied entry, rebuilds
//! the rendered tree as `admitted + local view + outstanding entries`, so a
//! refused, superseded or lost command snaps back to durable state.
//!
//! Reorders are idempotent (moving a group before its current successor is a
//! no-op), so re-applying an entry the durable state already contains cannot
//! double-apply. An entry is retired when:
//! * its command terminally failed (refused, cancelled, deadline): at once;
//! * it succeeded and the admitted tree already satisfies it (no flicker when
//!   the echo arrives before or after the success);
//! * it succeeded [`CONFIRMED_HOLD`] ago and the admitted tree still does not
//!   satisfy it: a concurrent later move from another member won, and the
//!   local view converges on the durable order;
//! * it never reached a terminal state within [`UNCONFIRMED_DEADLINE`].
//!
//! Commands needing fresh identities or record construction (open, move to a
//! new group, close) are never speculated.
use qos_desktop::{
    WorkspaceTree,
    session_commands::{BoundCommand, DesktopCommand, apply_command},
};
use web_time::{Duration, Instant};

/// A succeeded entry the admitted tree does not satisfy is retired after this.
pub(crate) const CONFIRMED_HOLD: Duration = Duration::from_secs(3);
/// The command bridge gives up after 30 s; an entry never settled by then is
/// retired shortly after (its later durable result arrives by admission).
pub(crate) const UNCONFIRMED_DEADLINE: Duration = Duration::from_secs(45);
/// Bound on outstanding entries (the request queue holds at most 32).
const MAX_ENTRIES: usize = 32;

/// Live-overlay ghost for remote members: the group's horizontal
/// displacement as laid out before the local reorder (CSS px).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Preview {
    pub group: u64,
    pub dx: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug)]
struct Entry {
    invocation: uuid::Uuid,
    room: String,
    command: DesktopCommand,
    queued_at: Instant,
    confirmed_at: Option<Instant>,
    applied: bool,
    preview: Option<Preview>,
}

#[derive(Default)]
pub(crate) struct Speculations {
    /// Set by the browser host once a command bridge will report outcomes.
    pub enabled: bool,
    entries: Vec<Entry>,
    /// An applied entry was retired outside admission: rebuild from durable.
    snap: bool,
}

pub(crate) fn eligible(command: &DesktopCommand) -> bool {
    matches!(
        command,
        DesktopCommand::ReorderGroup { .. } | DesktopCommand::ReorderTab { .. }
    )
}

fn apply(tree: &mut WorkspaceTree, room: &str, command: &DesktopCommand) -> Result<(), String> {
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room.to_string() == room)
        .ok_or("speculated room is not rendered")?;
    apply_command(
        workspace,
        command,
        &mut || Err("speculative reorder cannot mint identities".into()),
        &mut |_, _| Err("speculative reorder cannot create records".into()),
    )
}

type Order = Vec<(u64, Vec<(u64, Vec<u64>)>)>;
fn order(tree: &WorkspaceTree, room: &str) -> Option<Order> {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == room)?;
    Some(
        workspace
            .strips
            .iter()
            .map(|strip| {
                (
                    strip.id.0,
                    strip
                        .window_groups
                        .iter()
                        .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                        .collect(),
                )
            })
            .collect(),
    )
}

/// The durable tree already has the order this entry asks for.
fn satisfied(admitted: &WorkspaceTree, room: &str, command: &DesktopCommand) -> bool {
    let Some(before) = order(admitted, room) else {
        return false;
    };
    let mut probe = admitted.clone();
    apply(&mut probe, room, command).is_ok() && order(&probe, room) == Some(before)
}

impl Speculations {
    /// Record a queued command. Returns whether it will render optimistically.
    pub(crate) fn push(
        &mut self,
        invocation: uuid::Uuid,
        bound: &BoundCommand,
        preview: Option<Preview>,
        now: Instant,
    ) -> bool {
        if !self.enabled || !eligible(&bound.command) || self.entries.len() >= MAX_ENTRIES {
            return false;
        }
        self.entries.push(Entry {
            invocation,
            room: bound.source.room.clone(),
            command: bound.command.clone(),
            queued_at: now,
            confirmed_at: None,
            applied: false,
            preview,
        });
        true
    }

    /// Terminal outcome from the command bridge. Returns whether it changed.
    pub(crate) fn settle(&mut self, invocation: uuid::Uuid, succeeded: bool, now: Instant) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.invocation == invocation)
        else {
            return false;
        };
        if succeeded {
            self.entries[index].confirmed_at.get_or_insert(now);
        } else {
            let entry = self.entries.remove(index);
            self.snap |= entry.applied;
        }
        true
    }

    /// Retire entries per the module rules. Returns whether an applied entry
    /// was retired (the rendered tree must be rebuilt from durable state).
    pub(crate) fn retire(&mut self, admitted: &WorkspaceTree, now: Instant) -> bool {
        let mut rebuilt = false;
        self.entries.retain(|entry| {
            let keep = match entry.confirmed_at {
                Some(at) => {
                    now.duration_since(at) < CONFIRMED_HOLD
                        && !satisfied(admitted, &entry.room, &entry.command)
                }
                None => now.duration_since(entry.queued_at) < UNCONFIRMED_DEADLINE,
            };
            rebuilt |= !keep && entry.applied;
            keep
        });
        rebuilt | std::mem::take(&mut self.snap)
    }

    /// Apply entries to `tree`: all of them after a rebuild, else only new
    /// ones. An entry the current tree refuses is dropped (the owner would
    /// refuse it too). Returns whether the tree changed.
    pub(crate) fn apply(&mut self, tree: &mut WorkspaceTree, all: bool) -> bool {
        let mut changed = false;
        self.entries.retain_mut(|entry| {
            if entry.applied && !all {
                return true;
            }
            match apply(tree, &entry.room, &entry.command) {
                Ok(()) => {
                    changed |= !entry.applied;
                    entry.applied = true;
                    true
                }
                Err(_) => {
                    changed |= entry.applied;
                    false
                }
            }
        });
        changed || (all && !self.entries.is_empty())
    }

    pub(crate) fn preview(&self, invocation: uuid::Uuid) -> Option<&Preview> {
        self.entries
            .iter()
            .find(|entry| entry.invocation == invocation)
            .and_then(|entry| entry.preview.as_ref())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Nothing outstanding and no snap back owed to the rendered tree.
    pub(crate) fn idle(&self) -> bool {
        self.entries.is_empty() && !self.snap
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Where the group will land, from the layout drawn before the reorder: the
/// same displacement the overlay's `commandDrag` derives for remote ghosts.
pub(crate) fn preview(
    tree: &WorkspaceTree,
    layout: &crate::live_view::LiveLayout,
    command: &DesktopCommand,
) -> Option<Preview> {
    let DesktopCommand::ReorderGroup {
        strip,
        group,
        before,
    } = command
    else {
        return None;
    };
    let members = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find(|candidate| candidate.id.0 == *strip)?;
    let rect = |id: u64| {
        layout
            .groups
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.rect)
            .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
    };
    let own = rect(*group)?;
    let target = match before {
        None => {
            let last = members
                .window_groups
                .iter()
                .filter(|candidate| candidate.id != *group)
                .filter_map(|candidate| rect(candidate.id))
                .last()?;
            last.x1 - own.width()
        }
        Some(anchor) => {
            let anchor = rect(*anchor)?;
            if anchor.x0 > own.x0 {
                anchor.x0 - own.width()
            } else {
                anchor.x0
            }
        }
    };
    let dx = ((target - own.x0) * 100.0).round() / 100.0;
    (dx.abs() >= 1.0).then(|| Preview {
        group: *group,
        dx,
        width: (own.width() * 100.0).round() / 100.0,
        height: (own.height() * 100.0).round() / 100.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_desktop::session_commands::CommandSource;
    use qos_desktop::{
        AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };

    fn tree() -> WorkspaceTree {
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::new(WorkspaceId(1), "Main");
        let mut strip = Strip::new(StripId(10), "Strip");
        for id in [100u64, 200, 300] {
            let tab = Tab {
                id: id + 1,
                title: "Tab".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("dev.qos.notes".into()),
                    label: "Notes".into(),
                },
            };
            strip.window_groups.push(WindowGroup {
                id,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            });
        }
        workspace.strips.push(strip);
        tree.active = workspace.id;
        tree.workspaces.push(workspace);
        tree
    }
    fn room(tree: &WorkspaceTree) -> String {
        tree.workspaces[0].room.to_string()
    }
    fn groups(tree: &WorkspaceTree) -> Vec<u64> {
        tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|group| group.id)
            .collect()
    }
    fn bound(tree: &WorkspaceTree, group: u64, before: Option<u64>) -> BoundCommand {
        BoundCommand {
            source: CommandSource {
                room: room(tree),
                heads: vec![[1; 32]],
            },
            command: DesktopCommand::ReorderGroup {
                strip: 10,
                group,
                before,
            },
        }
    }
    fn enabled() -> Speculations {
        Speculations {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_or_non_reorder_commands_are_not_speculated() {
        let admitted = tree();
        let now = Instant::now();
        let mut spec = Speculations::default();
        assert!(!spec.push(
            uuid::Uuid::new_v4(),
            &bound(&admitted, 100, Some(300)),
            None,
            now
        ));
        let mut spec = enabled();
        let open = BoundCommand {
            source: bound(&admitted, 100, None).source,
            command: DesktopCommand::RenameWorkspace { name: "x".into() },
        };
        assert!(!spec.push(uuid::Uuid::new_v4(), &open, None, now));
        assert!(spec.is_empty());
    }

    #[test]
    fn a_queued_reorder_renders_at_once_and_the_echo_does_not_double_apply() {
        let admitted = tree();
        let mut rendered = admitted.clone();
        let now = Instant::now();
        let mut spec = enabled();
        let id = uuid::Uuid::new_v4();
        assert!(spec.push(id, &bound(&admitted, 100, Some(300)), None, now));
        assert!(spec.apply(&mut rendered, false));
        assert_eq!(groups(&rendered), [200, 100, 300]);
        // Nothing new: an incremental pass leaves the tree alone.
        assert!(!spec.apply(&mut rendered, false));
        assert_eq!(groups(&rendered), [200, 100, 300]);
        // The committed echo arrives before the success: rebuild + re-apply
        // is idempotent.
        let mut echoed = admitted.clone();
        apply(
            &mut echoed,
            &room(&admitted),
            &bound(&admitted, 100, Some(300)).command,
        )
        .unwrap();
        assert!(!spec.retire(&echoed, now), "unconfirmed entries stay");
        let mut rebuilt = echoed.clone();
        spec.apply(&mut rebuilt, true);
        assert_eq!(groups(&rebuilt), [200, 100, 300]);
        // Success with the echo already admitted retires it with no change.
        assert!(spec.settle(id, true, now));
        assert!(!spec.retire(&echoed, now) || groups(&echoed) == [200, 100, 300]);
        assert!(spec.is_empty());
    }

    #[test]
    fn success_before_the_echo_holds_the_local_order_until_it_lands() {
        let admitted = tree();
        let now = Instant::now();
        let mut spec = enabled();
        let id = uuid::Uuid::new_v4();
        spec.push(id, &bound(&admitted, 100, Some(300)), None, now);
        let mut rendered = admitted.clone();
        spec.apply(&mut rendered, false);
        spec.settle(id, true, now);
        // An unrelated admission (old order) must not flicker the move back.
        assert!(!spec.retire(&admitted, now + Duration::from_millis(500)));
        let mut rebuilt = admitted.clone();
        spec.apply(&mut rebuilt, true);
        assert_eq!(groups(&rebuilt), [200, 100, 300]);
        assert_eq!(spec.len(), 1);
    }

    #[test]
    fn refusal_snaps_back_to_durable_state() {
        let admitted = tree();
        let now = Instant::now();
        let mut spec = enabled();
        let id = uuid::Uuid::new_v4();
        spec.push(id, &bound(&admitted, 100, Some(300)), None, now);
        let mut rendered = admitted.clone();
        spec.apply(&mut rendered, false);
        assert!(spec.settle(id, false, now));
        assert!(
            spec.retire(&admitted, now),
            "an applied refusal requires a rebuild"
        );
        let mut rebuilt = admitted.clone();
        spec.apply(&mut rebuilt, true);
        assert_eq!(groups(&rebuilt), [100, 200, 300]);
        assert!(spec.is_empty());
    }

    #[test]
    fn a_concurrent_later_move_wins_after_the_hold() {
        // A moves 100 to the end; B's later move of 100 to the front commits
        // after A's. A's success arrives while the admitted state shows B's.
        let admitted = tree();
        let now = Instant::now();
        let mut spec = enabled();
        let id = uuid::Uuid::new_v4();
        spec.push(id, &bound(&admitted, 100, None), None, now);
        let mut rendered = admitted.clone();
        spec.apply(&mut rendered, false);
        assert_eq!(groups(&rendered), [200, 300, 100]);
        let mut durable = admitted.clone();
        apply(
            &mut durable,
            &room(&admitted),
            &bound(&admitted, 100, None).command,
        )
        .unwrap();
        apply(
            &mut durable,
            &room(&admitted),
            &bound(&admitted, 100, Some(200)).command,
        )
        .unwrap();
        assert_eq!(groups(&durable), [100, 200, 300]);
        spec.settle(id, true, now);
        assert!(!spec.retire(&durable, now + Duration::from_secs(1)));
        assert!(spec.retire(&durable, now + CONFIRMED_HOLD));
        let mut rebuilt = durable.clone();
        spec.apply(&mut rebuilt, true);
        assert_eq!(groups(&rebuilt), [100, 200, 300], "both members converge");
    }

    #[test]
    fn unsettled_entries_expire_and_stale_entries_are_dropped() {
        let admitted = tree();
        let now = Instant::now();
        let mut spec = enabled();
        spec.push(
            uuid::Uuid::new_v4(),
            &bound(&admitted, 100, Some(300)),
            None,
            now,
        );
        let mut rendered = admitted.clone();
        spec.apply(&mut rendered, false);
        assert!(!spec.retire(&admitted, now + Duration::from_secs(10)));
        assert!(spec.retire(&admitted, now + UNCONFIRMED_DEADLINE));
        assert!(spec.is_empty());
        // A reorder whose anchor was closed by durable state is dropped.
        spec.push(
            uuid::Uuid::new_v4(),
            &bound(&admitted, 100, Some(300)),
            None,
            now,
        );
        let mut closed = admitted.clone();
        closed.workspaces[0].strips[0]
            .window_groups
            .retain(|group| group.id != 300);
        spec.apply(&mut closed, true);
        assert!(spec.is_empty());
        assert_eq!(groups(&closed), [100, 200]);
    }

    #[test]
    fn preview_matches_the_overlay_displacement() {
        let admitted = tree();
        let mut layout = crate::live_view::LiveLayout::default();
        for (index, id) in [100u64, 200, 300].into_iter().enumerate() {
            let x = index as f64 * 400.0;
            layout.groups.push(crate::live_view::LiveGroup {
                id,
                rect: vello::kurbo::Rect::new(x, 0.0, x + 400.0, 300.0),
                tab: id + 1,
                scroll: None,
            });
        }
        let right = preview(
            &admitted,
            &layout,
            &bound(&admitted, 100, Some(300)).command,
        )
        .unwrap();
        assert_eq!(
            right,
            Preview {
                group: 100,
                dx: 400.0,
                width: 400.0,
                height: 300.0
            }
        );
        let end = preview(&admitted, &layout, &bound(&admitted, 100, None).command).unwrap();
        assert_eq!(end.dx, 800.0);
        let left = preview(
            &admitted,
            &layout,
            &bound(&admitted, 300, Some(200)).command,
        )
        .unwrap();
        assert_eq!(left.dx, -400.0);
    }
}
