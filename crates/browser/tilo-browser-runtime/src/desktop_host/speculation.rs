//! Optimistic local desktop commands: reorders, opens and closes.
//!
//! A queued command is applied to the rendered tree the frame it is queued,
//! with the host's own reducer (`apply_command`), instead of after the owner
//! commits it and the committed room state comes back. The admitted tree
//! stays the durable basis: every admission, and every retirement of an
//! applied entry, rebuilds the rendered tree as
//! `admitted + local view + outstanding entries` (replayed in issue order),
//! so a refused, superseded or lost command snaps back to durable state.
//!
//! * **Reorders** are idempotent (moving a group before its current
//!   successor is a no-op), so re-applying one the durable state already
//!   contains cannot double-apply. A succeeded reorder the admitted tree
//!   does not satisfy retires after [`CONFIRMED_HOLD`] (a later concurrent
//!   move won).
//! * **Opens** (`OpenApp`, the one open for people and agents) resolve
//!   through the host's `open_app_placement` over the host's own topology
//!   namespace and mint with the host's formula
//!   (`mint_in_namespace(salt, max(desktop_counter_v1, max own counter + 1))`),
//!   so the placeholder window lands at the predicted tab id P the owner will
//!   commit. No record is created locally: the tile is a placeholder until the
//!   committed window takes its place. One window per command id:
//!   - the receipt (or committed-desktop notice) binds the entry to tab T; once
//!     T is admitted the entry renders nothing, and until then the placeholder
//!     is drawn at T;
//!   - an echo that arrives first and gains P with the same app is adopted;
//!   - a later receipt naming T != P rebinds (P was another member's window);
//!   - an owner that reports no tab ids (native `opened: []`) relies on
//!     adoption alone; after success, a single new admitted window of the app
//!     that existed at no point before the command is adopted as well.
//! * **Closes** are speculated as removals and retire on refusal or when the
//!   admitted tree lacks the tab (idempotent).
//!
//! Snap back happens only on a terminal refusal or cancellation, on no
//! terminal outcome within [`UNCONFIRMED_DEADLINE`], or when the rebase
//! replay fails against the new admitted base (the owner would refuse too).
//! Focus is per-device local view and never a command: a new window takes
//! focus when first drawn, and focus on a placeholder follows it to the bound
//! window when the entry retires ([`FocusMove`]).
use automerge::{AutoCommit, ReadDoc};
use std::collections::BTreeSet;
use tilo_desktop_model::commands::{
    BoundCommand, DesktopCommand, OpenContext, TabPlacement, apply_command, apply_in_room,
    namespace_counter, open_app_placement,
};
use tilo_desktop_model::{AppKind, DesktopTree, TileContent, mint_in_namespace, peer_salt_of};
use web_time::{Duration, Instant};

/// A succeeded reorder the admitted tree does not satisfy is retired after this.
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

/// The committing owner's topology namespace, read from the admitted room:
/// the salts `contents_owners_v1` assigns to it and `desktop_counter_v1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Namespace {
    salts: BTreeSet<u16>,
    stored: u64,
}

/// Where a window lives: its strip, group and tab ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub strip: u64,
    pub group: u64,
    pub tab: u64,
}

/// Focus that sat on a placeholder group moves to its committed window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FocusMove {
    pub from: Placed,
    pub to: Placed,
}

#[derive(Clone, Debug)]
struct Open {
    app: String,
    /// Tabs of `app` rendered when the command was issued.
    existing: BTreeSet<u64>,
    /// Ids the last replay minted (the predicted window P).
    predicted: Option<Placed>,
    /// Bound by the receipt, a committed notice, or adoption.
    bound: Option<Placed>,
    /// Where the placeholder was last drawn.
    drawn: Option<Placed>,
    focused: bool,
}

#[derive(Clone, Debug)]
struct Entry {
    invocation: uuid::Uuid,
    room: String,
    command: DesktopCommand,
    queued_at: Instant,
    confirmed_at: Option<Instant>,
    /// The entry currently changes the rendered tree.
    applied: bool,
    preview: Option<Preview>,
    open: Option<Open>,
}

#[derive(Default)]
pub(crate) struct Speculations {
    /// Set by the browser host once a command bridge will report outcomes.
    pub enabled: bool,
    entries: Vec<Entry>,
    /// An applied entry was retired or rebound outside admission: rebuild
    /// from durable state.
    snap: bool,
    namespace: Option<Namespace>,
    focus_moves: Vec<FocusMove>,
    /// Windows first drawn by the last `apply` (they took focus).
    fresh: Vec<(String, Placed)>,
}

pub(crate) fn eligible(command: &DesktopCommand) -> bool {
    matches!(
        command,
        DesktopCommand::ReorderGroup { .. }
            | DesktopCommand::ReorderTab { .. }
            | DesktopCommand::CloseTab { .. }
            | DesktopCommand::OpenApp { .. }
    )
}

/// The namespace the owner mints in: the salts of the key that owns the last
/// strip (where `OpenApp` places), or of the only registered key.
pub(crate) fn namespace(doc: &AutoCommit, tree: &DesktopTree) -> Option<Namespace> {
    let (_, owners) = doc.get(automerge::ROOT, "contents_owners_v1").ok()??;
    let registry = doc
        .keys(&owners)
        .filter_map(|entry| {
            let salt = u16::from_str_radix(&entry, 16)
                .ok()
                .filter(|salt| *salt != 0)?;
            let (value, _) = doc.get(&owners, &entry).ok()??;
            Some((salt, value.as_str()?.to_owned()))
        })
        .collect::<Vec<_>>();
    let keys = registry
        .iter()
        .map(|(_, key)| key.as_str())
        .collect::<BTreeSet<_>>();
    let owner = if keys.len() == 1 {
        keys.first().copied()?
    } else {
        let last = tree.workspaces.first()?.strips.last()?.id.0;
        registry
            .iter()
            .find(|(salt, _)| *salt == peer_salt_of(last))
            .map(|(_, key)| key.as_str())?
    };
    let salts = registry
        .iter()
        .filter(|(_, key)| key == owner)
        .map(|(salt, _)| *salt)
        .collect::<BTreeSet<_>>();
    let stored = doc
        .get(automerge::ROOT, "desktop_counter_v1")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.as_u64())
        .unwrap_or(1);
    (!salts.is_empty()).then_some(Namespace { salts, stored })
}

fn workspace_mut<'a>(
    tree: &'a mut DesktopTree,
    room: &str,
) -> Result<&'a mut tilo_desktop_model::Workspace, String> {
    tree.workspaces
        .iter_mut()
        .find(|workspace| workspace.room.to_string() == room)
        .ok_or_else(|| "speculated room is not rendered".into())
}

fn apply(tree: &mut DesktopTree, room: &str, command: &DesktopCommand) -> Result<(), String> {
    apply_in_room(
        tree,
        room,
        command,
        &mut || Err("speculative command cannot mint identities".into()),
        &mut |_, _| Err("speculative command cannot create records".into()),
    )
}

/// The host's `OpenApp` turn over `tree`: placement in the owner's own
/// namespace, the owner's mint formula (or the bound ids), and a placeholder
/// tile in place of the checked record factory.
fn open(
    tree: &mut DesktopTree,
    room: &str,
    app: &str,
    namespace: &Namespace,
    bound: Option<Placed>,
) -> Result<Placed, String> {
    let title = crate::desktop::definition(app)
        .map(|definition| definition.title)
        .ok_or("application cannot be opened here")?;
    let workspace = workspace_mut(tree, room)?;
    let owned = |id: u64| namespace.salts.contains(&peer_salt_of(id));
    let placement = match bound {
        Some(_) if workspace.strips.is_empty() => TabPlacement::EmptyWorkspace,
        Some(bound) => {
            if !workspace
                .strips
                .iter()
                .any(|strip| strip.id.0 == bound.strip)
            {
                return Err("strip was removed".into());
            }
            TabPlacement::NewGroup {
                strip: bound.strip,
                after: None,
            }
        }
        None => open_app_placement(workspace, &OpenContext::default(), owned)?,
    };
    let salt = *namespace.salts.first().ok_or("no topology namespace")?;
    let mut counter = namespace_counter(workspace, salt, namespace.stored);
    let mut fixed = bound.map(|bound| [bound.tab, bound.group, bound.strip].into_iter());
    let mut minted = vec![];
    let strip = match &placement {
        TabPlacement::NewGroup { strip, .. } | TabPlacement::InGroup { strip, .. } => Some(*strip),
        TabPlacement::EmptyWorkspace => None,
    };
    apply_command(
        workspace,
        &DesktopCommand::OpenTab {
            app: app.into(),
            placement,
        },
        &mut || {
            let id = match fixed.as_mut() {
                Some(ids) => ids.next().ok_or("bound window ids exhausted")?,
                None => mint_in_namespace(salt, &mut counter)?,
            };
            minted.push(id);
            Ok(id)
        },
        &mut |app, _| {
            Ok((
                title.into(),
                TileContent::Stub {
                    kind: AppKind::Other(app.into()),
                    label: title.into(),
                },
            ))
        },
    )?;
    Ok(Placed {
        tab: minted[0],
        group: minted[1],
        strip: strip
            .or(minted.get(2).copied())
            .ok_or("strip identity missing")?,
    })
}

type Order = Vec<(u64, Vec<(u64, Vec<u64>)>)>;
fn order(tree: &DesktopTree, room: &str) -> Option<Order> {
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
fn satisfied(admitted: &DesktopTree, room: &str, command: &DesktopCommand) -> bool {
    let Some(before) = order(admitted, room) else {
        return false;
    };
    let mut probe = admitted.clone();
    apply(&mut probe, room, command).is_ok() && order(&probe, room) == Some(before)
}

/// Where `tab` lives in `tree`'s `room`, and its application.
fn locate(tree: &DesktopTree, room: &str, tab: u64) -> Option<(Placed, Option<String>)> {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == room)?;
    workspace.strips.iter().find_map(|strip| {
        strip.window_groups.iter().find_map(|group| {
            group
                .tabs
                .iter()
                .find(|candidate| candidate.id == tab)
                .map(|found| {
                    (
                        Placed {
                            strip: strip.id.0,
                            group: group.id,
                            tab,
                        },
                        tilo_desktop_model::apps::canonical_app_id(&found.tile),
                    )
                })
        })
    })
}

fn tabs_of(tree: &DesktopTree, room: &str, app: &str) -> BTreeSet<u64> {
    tree.workspaces
        .iter()
        .filter(|workspace| workspace.room.to_string() == room)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile).as_deref() == Some(app))
        .map(|tab| tab.id)
        .collect()
}

impl Speculations {
    /// The admitted room changed: re-read the owner's namespace and counter.
    pub(crate) fn set_namespace(&mut self, namespace: Option<Namespace>) {
        self.namespace = namespace;
    }

    /// Record a queued command against the `rendered` tree it was issued
    /// from. Returns whether it will render optimistically.
    pub(crate) fn push(
        &mut self,
        invocation: uuid::Uuid,
        bound: &BoundCommand,
        preview: Option<Preview>,
        rendered: &DesktopTree,
        now: Instant,
    ) -> bool {
        if !self.enabled || !eligible(&bound.command) || self.entries.len() >= MAX_ENTRIES {
            return false;
        }
        let room = bound.source.room.clone();
        let open = match &bound.command {
            DesktopCommand::OpenApp { app, .. } => {
                if self.namespace.is_none() || crate::desktop::definition(app).is_none() {
                    return false;
                }
                Some(Open {
                    app: app.clone(),
                    existing: tabs_of(rendered, &room, app),
                    predicted: None,
                    bound: None,
                    drawn: None,
                    focused: false,
                })
            }
            _ => None,
        };
        self.entries.push(Entry {
            invocation,
            room,
            command: bound.command.clone(),
            queued_at: now,
            confirmed_at: None,
            applied: false,
            preview,
            open,
        });
        true
    }

    /// Terminal outcome (`Some`) or window binding (`opened`) from the command
    /// bridge. Returns whether anything changed.
    pub(crate) fn settle(
        &mut self,
        invocation: uuid::Uuid,
        succeeded: Option<bool>,
        opened: Option<Placed>,
        now: Instant,
    ) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.invocation == invocation)
        else {
            return false;
        };
        if succeeded == Some(false) {
            let entry = self.entries.remove(index);
            self.snap |= entry.applied;
            return true;
        }
        let entry = &mut self.entries[index];
        let mut changed = false;
        if succeeded == Some(true) && entry.confirmed_at.is_none() {
            entry.confirmed_at = Some(now);
            changed = true;
        }
        if let (Some(open), Some(opened)) = (entry.open.as_mut(), opened)
            && open.bound != Some(opened)
        {
            open.bound = Some(opened);
            // The placeholder moves to (or disappears at) the bound window.
            self.snap |= entry.applied;
            changed = true;
        }
        changed
    }

    /// Retire entries per the module rules against the new `admitted` tree.
    /// Returns whether an applied entry was retired or rebound (the rendered
    /// tree must be rebuilt from durable state).
    pub(crate) fn retire(&mut self, admitted: &DesktopTree, now: Instant) -> bool {
        let mut rebuilt = false;
        let mut claimed = BTreeSet::new();
        let mut moves = vec![];
        self.entries.retain_mut(|entry| {
            let expired = now.duration_since(entry.queued_at) >= UNCONFIRMED_DEADLINE;
            let keep = match (&entry.command, entry.open.as_mut()) {
                (DesktopCommand::OpenApp { .. }, Some(open)) => {
                    // Adopt the predicted window once the echo shows it.
                    if open.bound.is_none()
                        && let Some(predicted) = open.predicted
                        && let Some((placed, app)) = locate(admitted, &entry.room, predicted.tab)
                        && app.as_deref() == Some(open.app.as_str())
                        && !open.existing.contains(&predicted.tab)
                    {
                        open.bound = Some(placed);
                    }
                    // A succeeded owner that reports no ids: one new window of
                    // the app that no earlier entry claimed is this command's.
                    if open.bound.is_none() && entry.confirmed_at.is_some() {
                        let fresh = tabs_of(admitted, &entry.room, &open.app)
                            .into_iter()
                            .filter(|tab| !open.existing.contains(tab) && !claimed.contains(tab))
                            .collect::<Vec<_>>();
                        if let [tab] = fresh[..] {
                            open.bound =
                                locate(admitted, &entry.room, tab).map(|(placed, _)| placed);
                        }
                    }
                    let committed = open
                        .bound
                        .and_then(|bound| locate(admitted, &entry.room, bound.tab))
                        .map(|(placed, _)| placed);
                    if let Some(placed) = committed {
                        claimed.insert(placed.tab);
                        if let Some(drawn) = open.drawn.filter(|drawn| drawn.group != placed.group)
                        {
                            moves.push(FocusMove {
                                from: drawn,
                                to: placed,
                            });
                        }
                    }
                    // Replaying an entry whose bound tab is admitted renders
                    // nothing; keep it until the outcome for a later rebind.
                    !(committed.is_some() && entry.confirmed_at.is_some()) && !expired
                }
                (DesktopCommand::CloseTab { location }, _) => {
                    locate(admitted, &entry.room, location.tab).is_some()
                        && match entry.confirmed_at {
                            Some(at) => now.duration_since(at) < CONFIRMED_HOLD,
                            None => !expired,
                        }
                }
                _ => match entry.confirmed_at {
                    Some(at) => {
                        now.duration_since(at) < CONFIRMED_HOLD
                            && !satisfied(admitted, &entry.room, &entry.command)
                    }
                    None => !expired,
                },
            };
            rebuilt |= !keep && entry.applied;
            keep
        });
        self.focus_moves.extend(moves);
        rebuilt | std::mem::take(&mut self.snap)
    }

    /// Apply entries to `tree`: all of them after a rebuild, else only new
    /// ones. An entry the current tree refuses is dropped (the owner would
    /// refuse it too). A newly drawn window takes local focus. Returns whether
    /// the tree changed.
    pub(crate) fn apply(&mut self, tree: &mut DesktopTree, all: bool) -> bool {
        let mut changed = false;
        let namespace = self.namespace.clone();
        let fresh = &mut self.fresh;
        fresh.clear();
        self.entries.retain_mut(|entry| {
            if entry.applied && !all {
                return true;
            }
            let rendered = match (entry.open.as_mut(), &entry.command) {
                (Some(state), DesktopCommand::OpenApp { app, .. }) => {
                    let admitted = state
                        .bound
                        .is_some_and(|bound| locate(tree, &entry.room, bound.tab).is_some());
                    if admitted {
                        state.drawn = None;
                        Ok(false)
                    } else if let Some(namespace) = &namespace {
                        open(tree, &entry.room, app, namespace, state.bound).map(|placed| {
                            if state.bound.is_none() {
                                state.predicted = Some(placed);
                            }
                            state.drawn = Some(placed);
                            if !state.focused {
                                state.focused = true;
                                focus(tree, &entry.room, placed);
                                fresh.push((entry.room.clone(), placed));
                            }
                            true
                        })
                    } else {
                        Err("owner namespace unavailable".into())
                    }
                }
                _ => apply(tree, &entry.room, &entry.command).map(|()| true),
            };
            match rendered {
                Ok(rendered) => {
                    changed |= rendered != entry.applied;
                    entry.applied = rendered;
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

    /// Re-select the windows the last `apply` drew for the first time, after
    /// the host restored the saved local view over them.
    pub(crate) fn refocus_fresh(&self, tree: &mut DesktopTree) {
        for (room, placed) in &self.fresh {
            focus(tree, room, *placed);
        }
    }

    /// Focus moves owed by retired or bound placeholders since the last call.
    pub(crate) fn take_focus_moves(&mut self) -> Vec<FocusMove> {
        std::mem::take(&mut self.focus_moves)
    }

    /// Tabs currently drawn as placeholders (no record is mounted for them).
    pub(crate) fn placeholders(&self) -> BTreeSet<u64> {
        self.entries
            .iter()
            .filter_map(|entry| entry.open.as_ref()?.drawn.map(|drawn| drawn.tab))
            .collect()
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

/// Local view only: select the new window's group in its strip.
fn focus(tree: &mut DesktopTree, room: &str, placed: Placed) {
    let Some(workspace) = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == room)
        .map(|workspace| workspace.id)
    else {
        return;
    };
    tree.active_strip
        .insert(workspace, tilo_desktop_model::StripId(placed.strip));
    tree.active_group
        .insert(tilo_desktop_model::StripId(placed.strip), placed.group);
}

/// The first window a desktop command receipt (`opened`, decimal-string ids)
/// or a committed-desktop notice (`opened` object) names.
pub(crate) fn opened(value: &serde_json::Value) -> Option<Placed> {
    let row = match &value["opened"] {
        serde_json::Value::Array(rows) => rows.first()?,
        row @ serde_json::Value::Object(_) => row,
        _ => return None,
    };
    let id = |key: &str| {
        let text = row[key].as_str()?;
        text.parse::<u64>().ok().filter(|id| id.to_string() == text)
    };
    Some(Placed {
        strip: id("strip")?,
        group: id("group")?,
        tab: id("tab")?,
    })
}

/// Where the group will land, from the layout drawn before the reorder: the
/// same displacement the overlay's `commandDrag` derives for remote ghosts.
pub(crate) fn preview(
    tree: &DesktopTree,
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
                .next_back()?;
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
    use tilo_desktop_model::commands::CommandSource;
    use tilo_desktop_model::{
        AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };

    fn tree() -> DesktopTree {
        let mut tree = DesktopTree::default();
        let mut workspace = Workspace::new(WorkspaceId(1), "Main");
        let mut strip = Strip::new(StripId(10), "Strip");
        for id in [100u64, 200, 300] {
            let tab = Tab {
                id: id + 1,
                title: "Tab".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("so.tilo.notes".into()),
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
    fn room(tree: &DesktopTree) -> String {
        tree.workspaces[0].room.to_string()
    }
    fn groups(tree: &DesktopTree) -> Vec<u64> {
        tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .map(|group| group.id)
            .collect()
    }
    fn bound(tree: &DesktopTree, group: u64, before: Option<u64>) -> BoundCommand {
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
            &admitted,
            now
        ));
        let mut spec = enabled();
        let open = BoundCommand {
            source: bound(&admitted, 100, None).source,
            command: DesktopCommand::RenameWorkspace { name: "x".into() },
        };
        assert!(!spec.push(uuid::Uuid::new_v4(), &open, None, &admitted, now));
        assert!(spec.is_empty());
    }

    #[test]
    fn a_queued_reorder_renders_at_once_and_the_echo_does_not_double_apply() {
        let admitted = tree();
        let mut rendered = admitted.clone();
        let now = Instant::now();
        let mut spec = enabled();
        let id = uuid::Uuid::new_v4();
        assert!(spec.push(id, &bound(&admitted, 100, Some(300)), None, &admitted, now));
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
        assert!(spec.settle(id, Some(true), None, now));
        assert!(!spec.retire(&echoed, now) || groups(&echoed) == [200, 100, 300]);
        assert!(spec.is_empty());
    }

    #[test]
    fn success_before_the_echo_holds_the_local_order_until_it_lands() {
        let admitted = tree();
        let now = Instant::now();
        let mut spec = enabled();
        let id = uuid::Uuid::new_v4();
        spec.push(id, &bound(&admitted, 100, Some(300)), None, &admitted, now);
        let mut rendered = admitted.clone();
        spec.apply(&mut rendered, false);
        spec.settle(id, Some(true), None, now);
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
        spec.push(id, &bound(&admitted, 100, Some(300)), None, &admitted, now);
        let mut rendered = admitted.clone();
        spec.apply(&mut rendered, false);
        assert!(spec.settle(id, Some(false), None, now));
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
        spec.push(id, &bound(&admitted, 100, None), None, &admitted, now);
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
        spec.settle(id, Some(true), None, now);
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
            &admitted,
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
            &admitted,
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
                legacy: None,
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

#[cfg(test)]
mod open_tests {
    use super::*;
    use tilo_desktop_model::commands::{CommandSource, TabLocation};
    use tilo_desktop_model::{
        Strip, StripId, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    };

    const SALT: u64 = 5 << 48;
    const STORED: u64 = 40;
    const NOTES: &str = "so.tilo.notes";
    const CALCULATOR: &str = "so.tilo.calculator";

    fn tab(id: u64, app: &str) -> Tab {
        Tab {
            id,
            title: "App".into(),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Other(app.into()),
                label: "App".into(),
            },
        }
    }
    /// One strip in the owner's namespace (salt 5) with two windows.
    fn admitted() -> DesktopTree {
        let mut tree = DesktopTree::default();
        let mut workspace = Workspace::new(WorkspaceId(1), "Main");
        let mut strip = Strip::new(StripId(SALT | 1), "Strip");
        for (group, id) in [(SALT | 2, SALT | 3), (SALT | 4, SALT | 5)] {
            strip.window_groups.push(WindowGroup {
                id: group,
                active_tab: id,
                tabs: vec![tab(id, NOTES)],
                width: WindowWidth::Normal,
                maximized: false,
            });
        }
        workspace.strips.push(strip);
        tree.active = workspace.id;
        tree.active_strip.insert(workspace.id, StripId(SALT | 1));
        tree.active_group.insert(StripId(SALT | 1), SALT | 2);
        tree.workspaces.push(workspace);
        tree
    }
    fn room(tree: &DesktopTree) -> String {
        tree.workspaces[0].room.to_string()
    }
    fn namespace(stored: u64) -> Namespace {
        Namespace {
            salts: BTreeSet::from([5]),
            stored,
        }
    }
    fn speculations(stored: u64) -> Speculations {
        let mut spec = Speculations {
            enabled: true,
            ..Default::default()
        };
        spec.set_namespace(Some(namespace(stored)));
        spec
    }
    fn bound(tree: &DesktopTree, command: DesktopCommand) -> BoundCommand {
        BoundCommand {
            source: CommandSource {
                room: room(tree),
                heads: vec![[1; 32]],
            },
            command,
        }
    }
    fn open_app(tree: &DesktopTree, app: &str) -> BoundCommand {
        bound(
            tree,
            DesktopCommand::OpenApp {
                app: app.into(),
                beside: None,
            },
        )
    }
    /// The owner's own turn: `open_app_placement` over its namespace, its
    /// mint formula, and a checked record tile. Returns the committed window.
    fn host_open(tree: &mut DesktopTree, app: &str, stored: &mut u64) -> Placed {
        let workspace = &mut tree.workspaces[0];
        let placement = open_app_placement(workspace, &OpenContext::default(), |id| {
            peer_salt_of(id) == 5
        })
        .unwrap();
        let mut counter = namespace_counter(workspace, 5, *stored);
        let mut minted = vec![];
        apply_command(
            workspace,
            &DesktopCommand::OpenTab {
                app: app.into(),
                placement,
            },
            &mut || {
                let id = mint_in_namespace(5, &mut counter)?;
                minted.push(id);
                Ok(id)
            },
            &mut |app, _| {
                Ok((
                    "App".into(),
                    TileContent::AppPlaceholder {
                        kind: AppKind::Other(app.into()),
                        label: "App".into(),
                    },
                ))
            },
        )
        .unwrap();
        *stored = counter;
        Placed {
            strip: SALT | 1,
            group: minted[1],
            tab: minted[0],
        }
    }
    /// Rebuild the rendered tree from durable state, as `rebase_speculation`
    /// does on every admission.
    fn rebase(spec: &mut Speculations, admitted: &DesktopTree, now: Instant) -> DesktopTree {
        spec.retire(admitted, now);
        let mut rendered = admitted.clone();
        spec.apply(&mut rendered, true);
        rendered
    }
    fn windows(tree: &DesktopTree, app: &str) -> Vec<(u64, bool)> {
        tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter(|tab| {
                tilo_desktop_model::apps::canonical_app_id(&tab.tile).as_deref() == Some(app)
            })
            .map(|tab| (tab.id, matches!(tab.tile, TileContent::Stub { .. })))
            .collect()
    }

    #[test]
    fn an_open_renders_at_the_predicted_id_and_the_echo_adopts_it_without_a_duplicate() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let id = uuid::Uuid::new_v4();
        let mut rendered = admitted.clone();
        assert!(spec.push(id, &open_app(&admitted, NOTES), None, &rendered, now));
        assert!(spec.apply(&mut rendered, false), "drawn on the next frame");
        let predicted = SALT | STORED;
        assert_eq!(
            windows(&rendered, NOTES),
            [(SALT | 3, false), (SALT | 5, false), (predicted, true)]
        );
        assert_eq!(spec.placeholders(), BTreeSet::from([predicted]));
        // The new window takes local focus.
        assert_eq!(
            rendered.active_group[&StripId(SALT | 1)],
            SALT | (STORED + 1)
        );
        // The owner commits the same window: same ids, same placement.
        let mut echoed = admitted.clone();
        let mut stored = STORED;
        let committed = host_open(&mut echoed, NOTES, &mut stored);
        assert_eq!(
            committed.tab, predicted,
            "the browser predicts the owner's mint"
        );
        // The echo lands before the receipt: P is adopted, no placeholder.
        let rendered = rebase(&mut spec, &echoed, now);
        assert_eq!(
            windows(&rendered, NOTES),
            [(SALT | 3, false), (SALT | 5, false), (predicted, false)],
            "one window per command: the committed one"
        );
        assert!(spec.placeholders().is_empty());
        assert_eq!(spec.len(), 1, "kept (rendering nothing) until the outcome");
        // Replays of an adopted entry are no-ops.
        let mut again = rendered.clone();
        assert!(!spec.apply(&mut again, false));
        assert_eq!(windows(&again, NOTES), windows(&rendered, NOTES));
        // The receipt names the same window: retire with no visible change.
        assert!(spec.settle(id, Some(true), Some(committed), now));
        assert!(!spec.retire(&echoed, now), "no rebuild for an adopted open");
        assert!(spec.is_empty());
    }

    #[test]
    fn a_receipt_before_the_echo_keeps_the_placeholder_until_the_window_lands() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let id = uuid::Uuid::new_v4();
        let mut rendered = admitted.clone();
        spec.push(id, &open_app(&admitted, NOTES), None, &rendered, now);
        spec.apply(&mut rendered, false);
        let mut echoed = admitted.clone();
        let committed = host_open(&mut echoed, NOTES, &mut STORED.clone());
        spec.settle(id, Some(true), Some(committed), now);
        // A slow echo is not a snap: an unrelated admission keeps the window.
        let later = now + Duration::from_secs(10);
        let rendered = rebase(&mut spec, &admitted, later);
        assert_eq!(
            windows(&rendered, NOTES).last(),
            Some(&(committed.tab, true))
        );
        let rendered = rebase(&mut spec, &echoed, later);
        assert_eq!(
            windows(&rendered, NOTES).last(),
            Some(&(committed.tab, false))
        );
        assert_eq!(windows(&rendered, NOTES).len(), 3);
        assert!(spec.is_empty());
    }

    #[test]
    fn a_refused_open_snaps_back() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let id = uuid::Uuid::new_v4();
        let mut rendered = admitted.clone();
        spec.push(id, &open_app(&admitted, NOTES), None, &rendered, now);
        spec.apply(&mut rendered, false);
        assert_eq!(windows(&rendered, NOTES).len(), 3);
        // desktop_command_rejected (e.g. not_permitted): terminal refusal.
        assert!(spec.settle(id, Some(false), None, now));
        assert!(
            spec.retire(&admitted, now),
            "an applied refusal needs a rebuild"
        );
        let mut rebuilt = admitted.clone();
        spec.apply(&mut rebuilt, true);
        assert_eq!(windows(&rebuilt, NOTES), windows(&admitted, NOTES));
        assert!(spec.is_empty());
        // A receipt after the snap changes nothing locally.
        assert!(!spec.settle(id, Some(true), None, now));
    }

    #[test]
    fn a_concurrent_remote_open_rebases_the_placeholder_onto_the_next_id() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let id = uuid::Uuid::new_v4();
        let mut rendered = admitted.clone();
        spec.push(id, &open_app(&admitted, NOTES), None, &rendered, now);
        spec.apply(&mut rendered, false);
        let predicted = windows(&rendered, NOTES).last().unwrap().0;
        // Another member's Calculator commits first and takes P.
        let mut durable = admitted.clone();
        let mut stored = STORED;
        let theirs = host_open(&mut durable, CALCULATOR, &mut stored);
        assert_eq!(theirs.tab, predicted);
        let rendered = rebase(&mut spec, &durable, now);
        let replayed = windows(&rendered, NOTES);
        assert_eq!(replayed.len(), 3, "still exactly one placeholder");
        let (moved, placeholder) = *replayed.last().unwrap();
        assert!(placeholder);
        assert_ne!(moved, predicted, "P is theirs; the replay predicts anew");
        assert_eq!(windows(&rendered, CALCULATOR), [(theirs.tab, false)]);
        // The owner serialized our open after theirs: it lands at the new P.
        let ours = host_open(&mut durable, NOTES, &mut stored);
        assert_eq!(ours.tab, moved);
        let rendered = rebase(&mut spec, &durable, now);
        assert_eq!(windows(&rendered, NOTES).last(), Some(&(moved, false)));
        assert_eq!(windows(&rendered, NOTES).len(), 3);
        assert_eq!(windows(&rendered, CALCULATOR).len(), 1);
    }

    #[test]
    fn a_receipt_naming_another_window_rebinds_a_mistaken_adoption() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let id = uuid::Uuid::new_v4();
        let mut rendered = admitted.clone();
        spec.push(id, &open_app(&admitted, NOTES), None, &rendered, now);
        spec.apply(&mut rendered, false);
        // Another member opened Notes first and got P: adopted by mistake.
        let mut durable = admitted.clone();
        let mut stored = STORED;
        let theirs = host_open(&mut durable, NOTES, &mut stored);
        let rendered = rebase(&mut spec, &durable, now);
        assert_eq!(windows(&rendered, NOTES).len(), 3);
        assert!(spec.placeholders().is_empty());
        // Our receipt names T: the placeholder is drawn at T until it lands.
        let mut next = durable.clone();
        let ours = host_open(&mut next, NOTES, &mut stored);
        assert_ne!(ours.tab, theirs.tab);
        assert!(spec.settle(id, Some(true), Some(ours), now));
        let rendered = rebase(&mut spec, &durable, now);
        assert_eq!(
            windows(&rendered, NOTES)[2..],
            [(theirs.tab, false), (ours.tab, true)],
            "P stays theirs; one placeholder at T"
        );
        let rendered = rebase(&mut spec, &next, now);
        assert_eq!(
            windows(&rendered, NOTES)[2..],
            [(theirs.tab, false), (ours.tab, false)]
        );
        assert!(spec.is_empty());
    }

    #[test]
    fn an_owner_without_window_ids_is_matched_after_success() {
        // The native owner's counter is ahead of what this peer admitted, so
        // P is wrong, and its receipt reports `opened: []`.
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let id = uuid::Uuid::new_v4();
        let mut rendered = admitted.clone();
        spec.push(id, &open_app(&admitted, NOTES), None, &rendered, now);
        spec.apply(&mut rendered, false);
        let mut durable = admitted.clone();
        let committed = host_open(&mut durable, NOTES, &mut (STORED + 7));
        spec.settle(id, Some(true), None, now);
        let rendered = rebase(&mut spec, &durable, now);
        assert_eq!(windows(&rendered, NOTES).len(), 3, "no duplicate");
        assert_eq!(
            windows(&rendered, NOTES).last(),
            Some(&(committed.tab, false))
        );
        assert!(spec.is_empty());
        assert_eq!(
            spec.take_focus_moves().last().map(|moved| moved.to),
            Some(committed),
            "focus follows the placeholder to the committed window"
        );
    }

    #[test]
    fn an_unsettled_open_expires_at_the_deadline() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let mut rendered = admitted.clone();
        spec.push(
            uuid::Uuid::new_v4(),
            &open_app(&admitted, NOTES),
            None,
            &rendered,
            now,
        );
        spec.apply(&mut rendered, false);
        assert!(!spec.retire(&admitted, now + Duration::from_secs(30)));
        assert!(spec.retire(&admitted, now + UNCONFIRMED_DEADLINE));
        let mut rebuilt = admitted.clone();
        spec.apply(&mut rebuilt, true);
        assert_eq!(windows(&rebuilt, NOTES).len(), 2);
    }

    #[test]
    fn opens_need_the_owner_namespace_and_an_installed_application() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        spec.set_namespace(None);
        assert!(!spec.push(
            uuid::Uuid::new_v4(),
            &open_app(&admitted, NOTES),
            None,
            &admitted,
            now
        ));
        spec.set_namespace(Some(namespace(STORED)));
        assert!(!spec.push(
            uuid::Uuid::new_v4(),
            &open_app(&admitted, "dev.example.nope"),
            None,
            &admitted,
            now
        ));
        // Two opens in one frame mint distinct windows, as two owner turns do.
        let mut rendered = admitted.clone();
        spec.push(
            uuid::Uuid::new_v4(),
            &open_app(&admitted, NOTES),
            None,
            &rendered,
            now,
        );
        spec.push(
            uuid::Uuid::new_v4(),
            &open_app(&admitted, CALCULATOR),
            None,
            &rendered,
            now,
        );
        spec.apply(&mut rendered, false);
        let mut durable = admitted.clone();
        let mut stored = STORED;
        let first = host_open(&mut durable, NOTES, &mut stored);
        let second = host_open(&mut durable, CALCULATOR, &mut stored);
        assert_eq!(windows(&rendered, NOTES).last(), Some(&(first.tab, true)));
        assert_eq!(windows(&rendered, CALCULATOR), [(second.tab, true)]);
    }

    #[test]
    fn a_close_is_a_removal_that_retires_when_durable_state_lacks_the_tab() {
        let admitted = admitted();
        let now = Instant::now();
        let mut spec = speculations(STORED);
        let close = bound(
            &admitted,
            DesktopCommand::CloseTab {
                location: TabLocation {
                    strip: SALT | 1,
                    group: SALT | 4,
                    tab: SALT | 5,
                },
            },
        );
        let id = uuid::Uuid::new_v4();
        let mut rendered = admitted.clone();
        assert!(spec.push(id, &close, None, &rendered, now));
        spec.apply(&mut rendered, false);
        assert_eq!(windows(&rendered, NOTES), [(SALT | 3, false)]);
        // Refused: the window comes back.
        spec.settle(id, Some(false), None, now);
        assert_eq!(windows(&rebase(&mut spec, &admitted, now), NOTES).len(), 2);
        // Again, and the echo lands without the tab: retired, idempotent.
        spec.push(id, &close, None, &admitted, now);
        let mut durable = admitted.clone();
        durable.workspaces[0].strips[0].window_groups.pop();
        let rendered = rebase(&mut spec, &durable, now);
        assert!(spec.is_empty());
        assert_eq!(windows(&rendered, NOTES), [(SALT | 3, false)]);
    }

    #[test]
    fn the_namespace_and_receipts_are_read_like_the_owner_writes_them() {
        use automerge::transaction::Transactable;
        let tree = admitted();
        let mut doc = AutoCommit::new();
        let owners = doc
            .put_object(
                automerge::ROOT,
                "contents_owners_v1",
                automerge::ObjType::Map,
            )
            .unwrap();
        doc.put(&owners, "0005", "ab".repeat(32)).unwrap();
        doc.put(&owners, "0009", "ab".repeat(32)).unwrap();
        doc.put(&owners, "0007", "cd".repeat(32)).unwrap();
        doc.put(automerge::ROOT, "desktop_counter_v1", 77_u64)
            .unwrap();
        assert_eq!(
            super::namespace(&doc, &tree),
            Some(Namespace {
                salts: BTreeSet::from([5, 9]),
                stored: 77
            }),
            "the key owning the last strip, with all its salts"
        );
        let receipt = serde_json::json!({"kind": "desktop.command", "command": "x", "generation": 3,
            "opened": [{"appId": NOTES, "recordId": "r", "strip": u64::MAX.to_string(), "group": "2", "tab": "3"}]});
        assert_eq!(
            opened(&receipt),
            Some(Placed {
                strip: u64::MAX,
                group: 2,
                tab: 3
            })
        );
        let notice = serde_json::json!({"opened": {"strip": "1", "group": "2", "tab": "3"}});
        assert_eq!(opened(&notice).map(|placed| placed.tab), Some(3));
        assert_eq!(opened(&serde_json::json!({"opened": []})), None);
        assert_eq!(
            opened(&serde_json::json!({"opened": [{"strip": "01", "group": "2", "tab": "3"}]})),
            None
        );
    }
}
