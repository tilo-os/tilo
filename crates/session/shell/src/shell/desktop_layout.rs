//! The desktop's layout, changed by id: open a window, make, name and fill
//! strips, lay a strip's windows out side by side, and take a run of such
//! changes back as one.
//!
//! One path for every caller. The command palette's `strip.*` and
//! `group.move-to-strip` apply these to the focused ids; the
//! `so.tilo.desktop` actions an agent invokes through the broker
//! ([`crate::shell::desktop_door`]) apply them to the ids it names. Each is
//! the desktop model's own reduction ([`DesktopCommand`]) committed through
//! the workspace tree, so the room's document, its peers and the restart
//! snapshot see a change the same way whoever made it.
//!
//! Every change an actor makes records what takes it back in the session's
//! [`Journal`], grouped by run: an agent's changes in one run of an issue
//! are one entry, a person's each change its own. `undo` takes an entry back
//! whole, newest change first.
//!
//! [`DesktopCommand`]: tilo_desktop_model::commands::DesktopCommand

use std::collections::HashMap;

use tilo_desktop_model::commands::DesktopCommand;

use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{GroupId, RoomId, StripId, TabId, WindowWidth, WorkspaceId};
use crate::shell::tab_binding::Placement;

/// How many runs the journal keeps to take back; an older one is
/// forgotten, as a closed tab is after its undo window.
const KEPT_RUNS: usize = 64;

/// What an [`open`] opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// A resource an app publishes (`mail.message:…`), opened in that app.
    Resource(String),
    /// Workspace content, by its BLAKE3 hash, opened in the app registered
    /// for its media type.
    Content(String),
    /// A web address, opened in the browser with the person's sign-ins.
    Url(String),
    /// A web address, opened in the browser as any visitor sees it: in a
    /// window of its own profile, without the person's cookies, sign-ins
    /// or saved passwords. What an agent opens unless it asks for the
    /// person's sign-ins; reading its page takes no authority beyond a
    /// visitor's.
    Visit(String),
}

impl Target {
    /// What `text` names: `tilo.content:<hash>`, an `http(s)` address, or
    /// any other resource address.
    ///
    /// # Errors
    ///
    /// `text` is empty, malformed content, or names no resource.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if let Some(hash) = tilo_actions::content_hash(text) {
            return if tilo_content::valid_hash(hash) {
                Ok(Self::Content(hash.to_owned()))
            } else {
                Err(format!("{text} is not a content address"))
            };
        }
        if text.starts_with("https://") || text.starts_with("http://") {
            return Ok(Self::Url(text.to_owned()));
        }
        match text.split_once(':') {
            Some((kind, id)) if !kind.is_empty() && !id.is_empty() => {
                Ok(Self::Resource(text.to_owned()))
            }
            _ => Err(format!(
                "{text:?} is not a resource address, tilo.content:<hash> or web address"
            )),
        }
    }

    /// The same web address, opened as a visitor ([`Self::Visit`]);
    /// anything else as it is.
    #[must_use]
    pub(crate) fn into_visit(self) -> Self {
        match self {
            Self::Url(url) => Self::Visit(url),
            other => other,
        }
    }

    /// The address as the caller named it.
    pub(crate) fn address(&self) -> String {
        match self {
            Self::Resource(address) | Self::Url(address) | Self::Visit(address) => address.clone(),
            Self::Content(hash) => format!("tilo.content:{hash}"),
        }
    }
}

/// Where an [`open`] puts its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// A window of its own: at the end of `strip`, else beside the window
    /// in focus.
    NewWindow { strip: Option<StripId> },
    /// A tab of the window `group`.
    TabOf(GroupId),
    /// A window of its own just right of `group`.
    Beside(GroupId),
}

/// How [`arrange`] lays a strip's windows out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Columns,
    Rows,
    Grid,
}

/// A window an [`open`] made, where it is, and the app it opens in.
pub(crate) struct Opened {
    pub tab: TabId,
    pub group: GroupId,
    pub strip: StripId,
    pub app: String,
    /// The launch's startup, which says whether it failed.
    pub startup: tilo_action_broker::services::LaunchReadiness,
}

/// Who changed the layout, and the run the change belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Actor {
    /// The policy principal (`agent:…`, a person's id).
    pub id: String,
    /// The issue whose run an agent works in. An agent without one groups
    /// its changes until another actor changes the layout.
    pub run: Option<String>,
    pub is_agent: bool,
}

impl Actor {
    /// The person at this device, in `room`: each of their changes is its
    /// own to take back.
    pub(crate) fn person(state: &ChromeState, room: RoomId) -> Self {
        Self {
            id: state.local_user_for_room(&room),
            run: None,
            is_agent: false,
        }
    }
}

/// The room of the workspace on screen.
pub(crate) fn shown_room(state: &ChromeState) -> Result<RoomId, String> {
    state
        .tree()
        .desktop
        .active_workspace()
        .map(crate::shell::state::Workspace::room)
        .ok_or_else(|| "there is no active workspace".to_owned())
}

/// What takes one change back.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Undo {
    Close(TabId),
    RemoveStrip(StripId),
    Rename {
        strip: StripId,
        name: String,
    },
    Move {
        group: GroupId,
        strip: StripId,
        before: Option<GroupId>,
    },
    Widths(Vec<(GroupId, WindowWidth)>),
}

/// One run's changes in one room, oldest first.
#[derive(Clone, Debug)]
struct Entry {
    room: RoomId,
    actor: String,
    run: Option<String>,
    steps: Vec<Undo>,
}

/// A window an [`open`] launched to show a resource: opened at it once the
/// window publishes its `open`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Opening {
    pub room: RoomId,
    pub tab: TabId,
    pub app: String,
    pub resource: String,
}

/// The session's record of layout changes to take back, and of the windows
/// opened at a resource.
#[derive(Debug, Default)]
pub(crate) struct Journal {
    entries: Vec<Entry>,
    /// Windows still to be opened at their resource.
    openings: Vec<Opening>,
    /// What each window an [`open`] made was opened with, for
    /// [`list_windows`] before the window says what it shows.
    opened: HashMap<TabId, String>,
    /// The broker's publications when the windows still to be opened at a
    /// resource were last tried: they are tried again once it moves.
    openings_tried_at: Option<u64>,
}

impl Journal {
    fn record(&mut self, room: RoomId, actor: &Actor, step: Undo) {
        let joins = |entry: &Entry| {
            entry.room == room
                && entry.actor == actor.id
                && entry.run == actor.run
                && (actor.run.is_some() || actor.is_agent)
        };
        // An agent with no run groups its changes until anyone else's.
        match self.entries.last_mut() {
            Some(entry) if joins(entry) => entry.steps.push(step),
            _ => {
                if let Some(entry) = (actor.run.is_some())
                    .then(|| self.entries.iter_mut().rev().find(|entry| joins(entry)))
                    .flatten()
                {
                    entry.steps.push(step);
                    return;
                }
                self.entries.push(Entry {
                    room,
                    actor: actor.id.clone(),
                    run: actor.run.clone(),
                    steps: vec![step],
                });
                if self.entries.len() > KEPT_RUNS {
                    self.entries.remove(0);
                }
            }
        }
    }

    /// The entry `run` names in `room`, else `actor`'s latest there.
    fn take(&mut self, room: RoomId, run: Option<&str>, actor: &str) -> Option<Entry> {
        let index = self.entries.iter().rposition(|entry| {
            entry.room == room
                && match run {
                    Some(run) => entry.run.as_deref() == Some(run),
                    None => entry.actor == actor,
                }
        })?;
        Some(self.entries.remove(index))
    }

    /// The windows still to be opened at a resource, when the broker's
    /// publications moved to `generation` since they were last tried (a
    /// window publishes its `open` once it is up), or one was added since.
    pub(crate) fn openings_due(&mut self, generation: u64) -> Vec<Opening> {
        if self.openings.is_empty() || self.openings_tried_at == Some(generation) {
            return Vec::new();
        }
        self.openings_tried_at = Some(generation);
        self.openings.clone()
    }

    /// The windows still to be opened at their resource.
    pub(crate) fn openings_pending(&self) -> impl Iterator<Item = TabId> + '_ {
        self.openings.iter().map(|opening| opening.tab)
    }

    /// `tab` is open at its resource, or gone: nothing more to do for it.
    pub(crate) fn opened_at(&mut self, tab: TabId) {
        self.openings.retain(|opening| opening.tab != tab);
    }

    /// What `tab` was opened with, when an [`open`] made it.
    pub(crate) fn opened_with(&self, tab: TabId) -> Option<&str> {
        self.opened.get(&tab).map(String::as_str)
    }
}

/// The workspace of `room`.
fn workspace_of(state: &ChromeState, room: RoomId) -> Result<WorkspaceId, String> {
    state
        .tree()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.id)
        .ok_or_else(|| "this workspace is not open on this device".to_owned())
}

/// Where `group` is in `room`: its strip and the group after it.
fn group_position(
    state: &ChromeState,
    room: RoomId,
    group: GroupId,
) -> Result<(StripId, Option<GroupId>), String> {
    let tree = state.tree();
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .find_map(|strip| {
            let index = strip.window_groups.iter().position(|g| g.id == group)?;
            let next = strip
                .window_groups
                .get(index.saturating_add(1))
                .map(|g| g.id);
            Some((strip.id, next))
        })
        .ok_or_else(|| format!("there is no window {group} in this workspace"))
}

/// The groups of `strip` in `room`, in order.
fn strip_groups(state: &ChromeState, room: RoomId, strip: StripId) -> Result<Vec<GroupId>, String> {
    let tree = state.tree();
    tree.desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .find(|candidate| candidate.id == strip)
        .map(|strip| strip.window_groups.iter().map(|g| g.id).collect())
        .ok_or_else(|| format!("there is no strip {} in this workspace", strip.raw()))
}

/// Keep the focus `room`'s workspace had before a change that moved it,
/// for a change made for someone other than the person at the screen.
struct KeptFocus {
    workspace: WorkspaceId,
    strip: Option<StripId>,
    group: Option<(StripId, Option<GroupId>)>,
}

impl KeptFocus {
    fn of(state: &ChromeState, workspace: WorkspaceId) -> Self {
        let tree = state.tree();
        let strip = tree.desktop.active_strip.get(&workspace).copied();
        let group = strip.map(|strip| (strip, tree.desktop.active_group.get(&strip).copied()));
        drop(tree);
        Self {
            workspace,
            strip,
            group,
        }
    }

    fn restore(self, state: &ChromeState) {
        let mut tree = state.tree_mut();
        match self.strip {
            Some(strip) => tree.desktop.active_strip.insert(self.workspace, strip),
            None => tree.desktop.active_strip.remove(&self.workspace),
        };
        if let Some((strip, group)) = self.group {
            match group {
                Some(group) => tree.desktop.active_group.insert(strip, group),
                None => tree.desktop.active_group.remove(&strip),
            };
        }
    }
}

/// Add a strip named `name` to `room`'s workspace; its id. `focus` moves
/// the person's view to it, as their own `strip.create` does; an agent's
/// leaves the view where it is.
///
/// # Errors
///
/// The name is empty, too long or holds control characters, or the
/// workspace is not here.
pub(crate) fn create_strip(
    state: &mut ChromeState,
    room: RoomId,
    name: &str,
    actor: &Actor,
    focus: bool,
) -> Result<StripId, String> {
    let name = crate::shell::actions::valid_strip_name(name)
        .ok_or("a strip name is 1 to 120 characters without control characters")?;
    let workspace = workspace_of(state, room)?;
    let kept = (!focus).then(|| KeptFocus::of(state, workspace));
    let strip = state.tree_mut().commit_strip_command_in(
        workspace,
        &DesktopCommand::CreateStrip {
            name: name.to_owned(),
        },
    )?;
    if let Some(kept) = kept {
        kept.restore(state);
    }
    state
        .desktop_layout
        .record(room, actor, Undo::RemoveStrip(strip));
    state.bump();
    Ok(strip)
}

/// Name `strip` of `room`'s workspace `name`.
///
/// # Errors
///
/// The name is invalid, or the strip is gone or another device's.
pub(crate) fn rename_strip(
    state: &mut ChromeState,
    room: RoomId,
    strip: StripId,
    name: &str,
    actor: &Actor,
) -> Result<(), String> {
    let name = crate::shell::actions::valid_strip_name(name)
        .ok_or("a strip name is 1 to 120 characters without control characters")?;
    let workspace = workspace_of(state, room)?;
    let before = {
        let tree = state.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.strips.iter().find(|s| s.id == strip))
            .map(|strip| strip.name.clone())
            .ok_or_else(|| format!("there is no strip {} in this workspace", strip.raw()))?
    };
    if before == name {
        return Ok(());
    }
    state.tree_mut().commit_strip_command_in(
        workspace,
        &DesktopCommand::RenameStrip {
            strip: strip.raw(),
            name: name.to_owned(),
        },
    )?;
    state.desktop_layout.record(
        room,
        actor,
        Undo::Rename {
            strip,
            name: before,
        },
    );
    state.bump();
    Ok(())
}

/// Move window `group`, whole, into `strip` of `room`'s workspace at
/// `index` among that strip's other windows (the end when `None` or past
/// it). Whether anything moved.
///
/// # Errors
///
/// The window or strip is not in the workspace, or the room's document
/// refused the move.
pub(crate) fn move_group(
    state: &mut ChromeState,
    room: RoomId,
    group: GroupId,
    strip: StripId,
    index: Option<usize>,
    actor: &Actor,
) -> Result<bool, String> {
    let (from, after) = group_position(state, room, group)?;
    let others: Vec<GroupId> = strip_groups(state, room, strip)?
        .into_iter()
        .filter(|candidate| *candidate != group)
        .collect();
    let before = index.and_then(|index| others.get(index).copied());
    let moved = commit_move(state, group, strip, before)?;
    if moved {
        state.desktop_layout.record(
            room,
            actor,
            Undo::Move {
                group,
                strip: from,
                before: after,
            },
        );
        state.bump();
    }
    Ok(moved)
}

fn commit_move(
    state: &ChromeState,
    group: GroupId,
    strip: StripId,
    before: Option<GroupId>,
) -> Result<bool, String> {
    state.tree_mut().commit_command(&DesktopCommand::MoveGroup {
        group,
        strip: strip.raw(),
        before,
    })
}

/// Lay `strip`'s windows out as `layout`: side by side, in the order
/// `order` names (every window of the strip, once) or as they are, in equal
/// shares or the shares `widths` gives (per mille of the strip, one per
/// window in order). The widths it set, for the caller to show.
///
/// # Errors
///
/// The strip is gone, `order` is not the strip's windows, `widths` does not
/// give one share per window or a share outside 1..=1000, or the layout is
/// one a strip does not have.
pub(crate) fn arrange(
    state: &mut ChromeState,
    room: RoomId,
    strip: StripId,
    layout: Layout,
    (order, widths): (Option<&[GroupId]>, Option<&[u32]>),
    actor: &Actor,
) -> Result<Vec<(GroupId, WindowWidth)>, String> {
    if layout != Layout::Columns {
        return Err(
            "a strip shows its windows side by side: arrange it in columns, and put windows \
             that belong apart on strips of their own"
                .into(),
        );
    }
    let mut groups = strip_groups(state, room, strip)?;
    if let Some(order) = order {
        let mut wanted = order.to_vec();
        let mut held = groups.clone();
        wanted.sort_unstable();
        held.sort_unstable();
        if wanted != held {
            return Err("`order` names every window of the strip, each once".into());
        }
        for (index, group) in order.iter().enumerate() {
            move_group(state, room, *group, strip, Some(index), actor)?;
        }
        groups = order.to_vec();
    }
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let shares: Vec<u32> = if let Some(widths) = widths {
        if widths.len() != groups.len() {
            return Err(format!(
                "the strip holds {} windows; give one width for each",
                groups.len()
            ));
        }
        if widths.iter().any(|width| !(1..=1000).contains(width)) {
            return Err("each width is a share of the strip from 1 to 1000 per mille".into());
        }
        widths.to_vec()
    } else {
        let count = u32::try_from(groups.len()).unwrap_or(u32::MAX);
        vec![1000_u32.checked_div(count).unwrap_or(1).max(1); groups.len()]
    };
    let wanted: Vec<(GroupId, WindowWidth)> = groups
        .iter()
        .zip(shares)
        .map(|(group, share)| (*group, WindowWidth::Fraction(share)))
        .collect();
    let before = set_widths(state, &wanted);
    if before.iter().zip(&wanted).all(|(old, new)| old == new) {
        return Ok(wanted);
    }
    state
        .desktop_layout
        .record(room, actor, Undo::Widths(before));
    state.bump();
    Ok(wanted)
}

/// Give each group its width in the tree; the widths they had.
fn set_widths(
    state: &ChromeState,
    widths: &[(GroupId, WindowWidth)],
) -> Vec<(GroupId, WindowWidth)> {
    let mut before = Vec::with_capacity(widths.len());
    let mut tree = state.tree_mut();
    for workspace in &mut tree.desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                if let Some((_, width)) = widths.iter().find(|(id, _)| *id == group.id) {
                    before.push((group.id, group.width));
                    group.width = *width;
                }
            }
        }
    }
    tree.queue_save();
    drop(tree);
    // In the order asked, so a caller compares like with like.
    widths
        .iter()
        .filter_map(|(id, _)| before.iter().find(|(old, _)| old == id).copied())
        .collect()
}

/// The app that shows `target`, and the launch that opens it there: the
/// browser for a web address, the app registered for content's media type
/// (with the content staged for it), the app publishing a resource.
struct Launch {
    app: String,
    document: Option<std::path::PathBuf>,
    uri: Option<String>,
    /// The resource to open the window at once it publishes its `open`.
    resource: Option<String>,
    /// The browser window browses as a visitor ([`Target::Visit`]).
    visitor: bool,
}

fn resolve(
    state: &ChromeState,
    room: RoomId,
    workspace: &mut crate::shell::state::Workspace,
    target: &Target,
    publisher: &dyn Fn(&str) -> Option<String>,
) -> Result<Launch, String> {
    match target {
        Target::Url(url) | Target::Visit(url) => {
            let resource = crate::shell::uri_open::resource_for_uri(url)
                .map_err(|error| error.user_message())?;
            let visitor = matches!(target, Target::Visit(_));
            // Only Browser opens a window without the person's sign-ins: a
            // visit is never handed to another app that might use them.
            let app = if visitor {
                crate::shell::launcher::gui_apps()
                    .into_iter()
                    .find(|app| app.package_id == ui::webview::BROWSER_APP_ID)
                    .ok_or("Browser is not installed, so nothing here opens web addresses as a visitor")?
                    .package_id
            } else {
                crate::shell::launcher::app_for_resource_with_policy(
                    &resource,
                    &state.local_user_for_room(&room),
                    Some(&workspace.default_apps),
                )
                .ok_or("no installed app opens web addresses")?
                .package_id
            };
            Ok(Launch {
                app,
                document: None,
                uri: Some(url.clone()),
                resource: None,
                visitor,
            })
        }
        Target::Content(hash) => {
            let bytes = crate::shell::broker_services::read_room_content(&room.to_string(), hash)?;
            let (_, media) = tilo_content::opened_as(&bytes);
            let kind = desktop_policy::ResourceKind::mime(media)
                .map_err(|_| format!("tilo.content:{hash} is {media}, which nothing opens"))?;
            let app = crate::shell::launcher::app_for_resource_with_policy(
                &kind,
                &state.local_user_for_room(&room),
                Some(&workspace.default_apps),
            )
            .ok_or_else(|| format!("no installed app opens {media}"))?;
            let path = crate::shell::broker_services::stage_content_bytes(
                &app.package_id,
                hash,
                &bytes,
                workspace,
            )?;
            Ok(Launch {
                app: app.package_id,
                document: Some(path),
                uri: None,
                resource: None,
                visitor: false,
            })
        }
        Target::Resource(resource) => {
            if let Some(app) = publisher(resource) {
                return Ok(Launch {
                    app,
                    document: None,
                    uri: None,
                    resource: Some(resource.clone()),
                    visitor: false,
                });
            }
            // An address in a scheme an installed app claims (its manifest's
            // `[providers] schemes`, such as Tasks' `tilo-tasks:chat/<issue>`)
            // launches that app on it, as opening the address anywhere does:
            // one window, opened at what the address names.
            let app = crate::shell::uri_open::resolve_for(
                resource,
                &state.local_user_for_room(&room),
                Some(&workspace.default_apps),
            )
            .map_err(|_| {
                format!(
                    "no app in this workspace publishes {resource} or opens its address: find \
                     it in the action catalogue first"
                )
            })?;
            Ok(Launch {
                app: app.package_id,
                document: None,
                uri: Some(resource.clone()),
                resource: None,
                visitor: false,
            })
        }
    }
}

/// Open `target` in a new window at `place` in `room`'s workspace, which
/// must be the one on screen: the window's place is held at once, so its
/// id is known before the app has started, and the app's window takes it
/// when it arrives. `background` keeps the person's focus where it is.
/// `publisher` names the app publishing a resource.
///
/// # Errors
///
/// The target cannot be opened (nothing opens it, its content is not
/// here), the place names a window or strip that is not there, the
/// workspace is not the one on screen, or the launch was refused.
#[allow(
    clippy::too_many_lines,
    reason = "one open: the place, the app and launch, the held place, and the record of it"
)]
pub(crate) fn open(
    state: &mut ChromeState,
    room: RoomId,
    target: &Target,
    place: Place,
    background: bool,
    actor: &Actor,
    publisher: &dyn Fn(&str) -> Option<String>,
) -> Result<Opened, String> {
    let workspace_id = workspace_of(state, room)?;
    if state.tree().desktop.active != workspace_id {
        return Err(
            "windows open in the workspace on screen; this workspace is not shown now".into(),
        );
    }
    let placement = match place {
        Place::NewWindow { strip: Some(strip) } => {
            let count = strip_groups(state, room, strip)?.len();
            Placement::GroupAt(strip, count)
        }
        Place::NewWindow { strip: None } => {
            let focused = crate::shell::placement::aside(&state.tree());
            match focused {
                Some(Placement::Beside(group)) if !background => Placement::RightOf(group),
                Some(placement) => placement,
                None => Placement::NewGroup,
            }
        }
        Place::TabOf(group) => {
            group_position(state, room, group)?;
            Placement::TabIn(group)
        }
        Place::Beside(group) => {
            group_position(state, room, group)?;
            if background {
                Placement::Beside(group)
            } else {
                Placement::RightOf(group)
            }
        }
    };
    let mut workspace = state
        .tree()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .cloned()
        .ok_or("this workspace is not open on this device")?;
    // A document has one writer: content already open in a window here is
    // that window, not a second process contending for its record.
    if let Target::Content(_) = target
        && let Some(opened) = already_open(state, room, &target.address())
    {
        return Ok(opened);
    }
    let launch = resolve(state, room, &mut workspace, target, publisher)?;
    if let Some(document) = &launch.document {
        // The app reads the staged content under a grant the live tree
        // holds, as a document opened from Files is: the launch's sandbox
        // is judged against the policy the tree publishes, which a grant
        // made only on this copy of the workspace never reaches.
        let folder = document
            .parent()
            .ok_or("the staged content has no folder")?
            .to_path_buf();
        let granted = crate::shell::doc_open::grant_document_read(
            &mut state.tree_mut(),
            workspace_id,
            &launch.app,
            &folder,
        );
        if granted != Some(true) {
            return Err(format!(
                "{} could not be granted the content to read",
                crate::shell::launcher::display_name_for_package_id(&launch.app)
            ));
        }
        workspace = state
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .cloned()
            .ok_or("this workspace is not open on this device")?;
    }
    let label = crate::shell::launcher::display_name_for_package_id(&launch.app);
    let kept = background.then(|| KeptFocus::of(state, workspace_id));
    let tab = {
        let mut tree = state.tree_mut();
        crate::shell::tab_binding::hold_place_for_launch(&mut tree, &launch.app, &label, placement)
    };
    if let Some(kept) = kept {
        kept.restore(state);
    }
    let held = state
        .tree()
        .desktop
        .workspaces
        .iter()
        .find_map(|workspace| {
            workspace.strips.iter().find_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .find(|group| group.tabs.iter().any(|candidate| candidate.id == tab))
                    .map(|group| (strip.id, group.id))
            })
        });
    let Some((strip, group)) = held else {
        crate::shell::launcher::clear_pending_placement(&launch.app);
        return Err("the window's place could not be held".into());
    };
    // What the launch carries is the workspace's accepted policy, as a
    // document opened from Files is: a read of the content it opens is
    // granted in the tree itself (with this device's provenance for it), and
    // the launch takes the workspace as it stands after that, so the
    // launch's authority gate finds every grant it was handed.
    let workspace = {
        let mut tree = state.tree_mut();
        if let Some(path) = &launch.document {
            let granted = crate::shell::doc_open::grant_document_read(
                &mut tree,
                workspace_id,
                &launch.app,
                path,
            );
            if granted != Some(true) {
                drop(tree);
                if let Err(gone) = crate::shell::window_actions::close_tab(state, tab) {
                    tracing::debug!(
                        tab,
                        ?gone,
                        "the place held for refused content was already gone"
                    );
                }
                return Err(format!(
                    "{} could not be handed {}",
                    label,
                    target.address()
                ));
            }
        }
        tree.queue_save();
        tree.desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace_id)
            .cloned()
            .unwrap_or(workspace)
    };
    // This launch's own startup is observed: its failure is this open's,
    // whatever another launch of the same app does.
    let started = crate::shell::launch_readiness::observe(|| {
        crate::shell::launcher::queue_package_open(
            launch.app.clone(),
            Some(label),
            &workspace,
            launch.document,
            launch.uri,
            launch.visitor,
        )
        .map_err(|error| error.user_message())
    });
    let startup = match started {
        Ok(startup) => startup,
        Err(error) => {
            // Nothing will arrive for the place: it goes, as a refused launch's does.
            if let Err(gone) = crate::shell::window_actions::close_tab(state, tab) {
                tracing::debug!(
                    tab,
                    ?gone,
                    "the place held for a refused launch was already gone"
                );
            }
            return Err(error);
        }
    };
    let journal = &mut state.desktop_layout;
    journal.opened.insert(tab, target.address());
    if let Some(resource) = launch.resource {
        journal.openings_tried_at = None;
        journal.openings.push(Opening {
            room,
            tab,
            app: launch.app.clone(),
            resource,
        });
    }
    journal.record(room, actor, Undo::Close(tab));
    state.bump();
    Ok(Opened {
        tab,
        group,
        strip,
        app: launch.app,
        startup,
    })
}

/// The window an earlier [`open`] made for `address` in `room`, while it
/// is still there.
fn already_open(state: &ChromeState, room: RoomId, address: &str) -> Option<Opened> {
    let tree = state.tree();
    let found = tree
        .desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group
                    .tabs
                    .iter()
                    .find(|tab| state.desktop_layout.opened_with(tab.id) == Some(address))
                    .map(|tab| Opened {
                        tab: tab.id,
                        group: group.id,
                        strip: strip.id,
                        app: tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                            .unwrap_or_default(),
                        startup: started_already(),
                    })
            })
        });
    drop(tree);
    found
}

/// The startup of a window already running: it started, so it cannot fail.
fn started_already() -> tilo_action_broker::services::LaunchReadiness {
    let (started, readiness) = tokio::sync::oneshot::channel();
    if started.send(Ok(())).is_err() {
        tracing::debug!("a startup observed as done had nobody to tell");
    }
    tilo_action_broker::services::LaunchReadiness::new(readiness, || {})
}

/// `tab`'s launch failed with `why`: its place goes, it is not opened at a
/// resource, and the open that held it is told so.
pub(crate) fn launch_failed(state: &mut ChromeState, tab: TabId, why: &str) -> String {
    state.desktop_layout.opened_at(tab);
    state.desktop_layout.opened.remove(&tab);
    if let Err(gone) = crate::shell::window_actions::close_tab(state, tab) {
        tracing::debug!(
            tab,
            ?gone,
            "the place held for a failed launch was already gone"
        );
    }
    format!("the window did not open: {why}")
}

/// One window of a workspace, as [`list_windows`] tells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    pub tab: TabId,
    pub app: Option<String>,
    /// What the window shows, when it says, else what it was opened with.
    pub resource: Option<String>,
    pub title: String,
    pub group: GroupId,
    pub strip: StripId,
    pub strip_name: String,
    /// Whether it is the tab its window shows.
    pub shown: bool,
}

/// Every window of `room`'s workspace, strip by strip in order. `shows`
/// says what a window shows, when its bar says.
///
/// # Errors
///
/// The workspace is not here.
pub(crate) fn list_windows(
    state: &ChromeState,
    room: RoomId,
    shows: &dyn Fn(TabId) -> Option<String>,
) -> Result<Vec<Window>, String> {
    workspace_of(state, room)?;
    let tree = state.tree();
    let windows = tree
        .desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.room() == room)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| {
            strip
                .window_groups
                .iter()
                .flat_map(move |group| group.tabs.iter().map(move |tab| (strip, group, tab)))
        })
        .map(|(strip, group, tab)| Window {
            tab: tab.id,
            app: tilo_desktop_model::apps::canonical_app_id(&tab.tile),
            resource: shows(tab.id)
                .or_else(|| state.desktop_layout.opened_with(tab.id).map(str::to_owned)),
            title: tab.title.clone(),
            group: group.id,
            strip: strip.id,
            strip_name: strip.name.clone(),
            shown: group.active_tab == tab.id,
        })
        .collect();
    drop(tree);
    Ok(windows)
}

/// What [`undo`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Undone {
    /// Changes taken back.
    pub changes: usize,
    /// Changes left as they are, with why: a strip someone put a window in
    /// since, a window already closed.
    pub kept: Vec<String>,
    /// The run taken back, when it had one.
    pub run: Option<String>,
}

/// Take back the layout changes of `run` in `room`, else `actor`'s latest
/// run there, newest change first. The widths it set back, for the caller
/// to show.
///
/// # Errors
///
/// There is no such run to take back.
pub(crate) fn undo(
    state: &mut ChromeState,
    room: RoomId,
    run: Option<&str>,
    actor: &Actor,
) -> Result<(Undone, Vec<(GroupId, WindowWidth)>), String> {
    let entry = state
        .desktop_layout
        .take(room, run, &actor.id)
        .ok_or_else(|| match run {
            Some(run) => format!("there are no layout changes of {run} to undo"),
            None => "there are no layout changes of yours to undo".to_owned(),
        })?;
    let mut done = Undone {
        run: entry.run.clone(),
        ..Undone::default()
    };
    let mut widths = Vec::new();
    for step in entry.steps.into_iter().rev() {
        match undo_step(state, room, step, &mut widths) {
            Ok(()) => done.changes = done.changes.saturating_add(1),
            Err(kept) => done.kept.push(kept),
        }
    }
    state.bump();
    Ok((done, widths))
}

fn undo_step(
    state: &mut ChromeState,
    room: RoomId,
    step: Undo,
    widths: &mut Vec<(GroupId, WindowWidth)>,
) -> Result<(), String> {
    match step {
        Undo::Close(tab) => {
            state.desktop_layout.opened_at(tab);
            state.desktop_layout.opened.remove(&tab);
            crate::shell::window_actions::close_tab(state, tab)
                .map_err(|_| format!("window {tab} was already closed"))
        }
        Undo::RemoveStrip(strip) => {
            let workspace = workspace_of(state, room)?;
            let groups = strip_groups(state, room, strip)?;
            if !groups.is_empty() {
                return Err(format!(
                    "strip {} holds windows put there since, so it stays",
                    strip.raw()
                ));
            }
            let kept = KeptFocus::of(state, workspace);
            state
                .tree_mut()
                .commit_strip_command_in(
                    workspace,
                    &DesktopCommand::RemoveStrip { strip: strip.raw() },
                )
                .map(drop)?;
            if kept.strip != Some(strip) {
                kept.restore(state);
            }
            Ok(())
        }
        Undo::Rename { strip, name } => {
            let workspace = workspace_of(state, room)?;
            state
                .tree_mut()
                .commit_strip_command_in(
                    workspace,
                    &DesktopCommand::RenameStrip {
                        strip: strip.raw(),
                        name,
                    },
                )
                .map(drop)
        }
        Undo::Move {
            group,
            strip,
            before,
        } => {
            group_position(state, room, group)?;
            let before = before.filter(|before| {
                strip_groups(state, room, strip).is_ok_and(|groups| groups.contains(before))
            });
            commit_move(state, group, strip, before).map(drop)
        }
        Undo::Widths(before) => {
            set_widths(state, &before);
            widths.extend(before);
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "desktop_layout_tests.rs"]
mod tests;
