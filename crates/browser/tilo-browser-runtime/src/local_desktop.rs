//! A desktop the page owns: in-memory room documents, one per workspace,
//! that nothing replicates. This peer is their only topology owner, so a
//! desktop command the canonical shell binds is reduced here, with the same
//! command reduction, projection and record routes a workspace host runs,
//! and comes back as the next admission.
use crate::desktop::{self, Admission, Requirement};
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::caps::{CapOverride, Principal, Scope, Subject, Verb};
use std::collections::BTreeSet;
use tilo_desktop::intents::{LocalViewIntent, apply_local_view};
use tilo_desktop_model::{
    AppKind, DesktopTree, IdMinter, RoomId, Strip, StripId, Tab, TileContent, WindowGroup,
    WindowWidth,
    apps::{bind_tab_record, canonical_app_id, record_routes},
    commands::{BoundCommand, DesktopCommand, apply_command, check_owned, resolve_open_app},
    peer_salt_of,
    projection::{self, EmptyStrips},
};

/// Workspaces one page may seed, and windows per workspace.
const MAX_WORKSPACES: usize = 16;
const MAX_WINDOWS: usize = 64;
const MAX_GRANTS: usize = 64;

/// What the page seeds (`MountOptions.local` in `host.d.ts`).
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Seed {
    #[serde(default)]
    pub workspaces: Vec<SeedWorkspace>,
    /// Apps the page links before mounting, so the launcher lists them.
    #[serde(default)]
    pub apps: Vec<String>,
}
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedWorkspace {
    #[serde(default, alias = "label")]
    pub title: Option<String>,
    #[serde(default)]
    pub strips: Vec<SeedStrip>,
    /// People and agents granted room membership, as the room's owner (the
    /// mount's author) granted them.
    #[serde(default)]
    pub members: Vec<SeedMember>,
    /// Capabilities granted to apps in the workspace.
    #[serde(default)]
    pub grants: Vec<SeedGrant>,
}
#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Access {
    Read,
    Comment,
    #[default]
    Write,
    Invite,
    Admin,
}
impl From<Access> for Verb {
    fn from(access: Access) -> Self {
        match access {
            Access::Read => Verb::Read,
            Access::Comment => Verb::Comment,
            Access::Write => Verb::Write,
            Access::Invite => Verb::Invite,
            Access::Admin => Verb::Admin,
        }
    }
}
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedMember {
    pub name: String,
    #[serde(default)]
    pub access: Access,
    #[serde(default)]
    pub agent: bool,
}
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedGrant {
    pub app: String,
    #[serde(default)]
    pub access: Access,
    /// The app whose artifacts the grant ranges over; the whole workspace
    /// without it.
    #[serde(default)]
    pub over: Option<String>,
}
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedStrip {
    #[serde(default, alias = "label")]
    pub title: Option<String>,
    #[serde(default)]
    pub windows: Vec<SeedWindow>,
}
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SeedWindow {
    pub app: String,
    /// An existing record of the app in the mount's room, such as one a
    /// standalone mount of the same profile, device and room opened.
    #[serde(default)]
    pub record: Option<String>,
    #[serde(default, alias = "label")]
    pub title: Option<String>,
    /// Select this window when the desktop first presents.
    #[serde(default)]
    pub focused: bool,
}

struct Room {
    id: RoomId,
    /// This owner's topology namespace in the room.
    salt: u16,
    doc: AutoCommit,
    /// Every permission in the room, as the workspace settings show them.
    capabilities: Vec<CapOverride>,
}

pub(crate) struct LocalDesktop {
    rooms: Vec<Room>,
    /// Tabs this owner created whose records do not exist yet: the desktop
    /// creates each through the app's own new-record path when it first
    /// mounts it, and never infers creation for any other tab.
    fresh: BTreeSet<u64>,
    focus: Option<u64>,
}

/// The `n`th workspace's room: the mount's room first, then rooms derived
/// from it, still version-4 UUIDs.
fn room_id(room: RoomId, n: usize) -> RoomId {
    let mut bytes = room.as_uuid().into_bytes();
    bytes[0] ^= n as u8;
    bytes[15] ^= n as u8;
    RoomId::from(uuid::Uuid::from_bytes(bytes))
}
fn salt(n: usize) -> u16 {
    n as u16 + 1
}
/// The record a new tab of `definition` is bound to, as a workspace host's
/// checked record factory names it.
fn fresh_route(definition: &ui::application::ApplicationDefinition, tab: u64) -> String {
    let checkout = tilo_desktop::records::component_record(
        definition.app_id,
        tab,
        definition.record_kind.clone(),
    );
    if checkout.id.is_package_primary() {
        String::new()
    } else {
        checkout.id.as_str().into()
    }
}
fn tile(definition: &ui::application::ApplicationDefinition) -> TileContent {
    TileContent::AppPlaceholder {
        kind: AppKind::Other(definition.app_id.into()),
        label: definition.title.into(),
    }
}
fn installed(app: &str) -> Result<&'static ui::application::ApplicationDefinition, String> {
    desktop::definition(app).ok_or_else(|| format!("application {app:?} is not available"))
}
/// Commit what the owner changed at the time it did: `AutoCommit::commit`
/// stamps a change at the epoch, which Activity would show as its time.
fn commit(doc: &mut AutoCommit) {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));
    doc.commit_with(automerge::transaction::CommitOptions::default().with_time(now));
}

impl LocalDesktop {
    pub(crate) fn new(room: &str, author: &str, seed: &Seed) -> Result<Self, String> {
        let room = RoomId::parse(room).ok_or("invalid room identity")?;
        if seed.workspaces.len() > MAX_WORKSPACES {
            return Err(format!("at most {MAX_WORKSPACES} workspaces"));
        }
        for app in &seed.apps {
            installed(app)?;
        }
        let default = [SeedWorkspace::default()];
        let workspaces = if seed.workspaces.is_empty() {
            &default[..]
        } else {
            &seed.workspaces
        };
        let windows = workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .map(|s| s.windows.len())
            .sum::<usize>();
        if windows > MAX_WINDOWS {
            return Err(format!("at most {MAX_WINDOWS} windows"));
        }
        let actor = automerge::ActorId::from(author.as_bytes());
        let mut desktop = Self {
            rooms: vec![],
            fresh: BTreeSet::new(),
            focus: None,
        };
        for (n, seeded) in workspaces.iter().enumerate() {
            let id = room_id(room, n);
            let salt = salt(n);
            let mut ids = IdMinter::resume(salt, 1);
            let mut routes = vec![];
            let mut strips = vec![];
            for strip in &seeded.strips {
                let mut groups = vec![];
                for window in &strip.windows {
                    let definition = installed(&window.app)?;
                    let group = ids.mint_in_namespace()?;
                    let tab = ids.mint_in_namespace()?;
                    match &window.record {
                        Some(record) => {
                            let checkout = desktop::checkout(
                                definition,
                                tab,
                                &[(tab, record.clone())].into(),
                            )?;
                            routes.push((tab, checkout.id.as_str().to_owned()));
                        }
                        None => {
                            routes.push((tab, fresh_route(definition, tab)));
                            desktop.fresh.insert(tab);
                        }
                    }
                    if window.focused {
                        desktop.focus = Some(tab);
                    }
                    groups.push(WindowGroup {
                        id: group,
                        active_tab: tab,
                        tabs: vec![Tab {
                            id: tab,
                            title: window
                                .title
                                .clone()
                                .unwrap_or_else(|| definition.title.into()),
                            tile: tile(definition),
                        }],
                        width: WindowWidth::Normal,
                        maximized: false,
                    });
                }
                strips.push(Strip {
                    id: StripId(ids.mint_in_namespace()?),
                    name: strip.title.clone().unwrap_or_else(|| "Main".into()),
                    window_groups: groups,
                });
            }
            let mut doc = AutoCommit::new();
            doc.set_actor(actor.clone());
            let owner = hex::encode(id.as_uuid().into_bytes().repeat(2));
            let put = |doc: &mut AutoCommit, key: &str, value: String| {
                doc.put(ROOT, key, value).map_err(|e| e.to_string())
            };
            put(&mut doc, projection::ROOM_ID_KEY, id.to_string())?;
            put(
                &mut doc,
                projection::ROOM_NAME_KEY,
                seeded
                    .title
                    .clone()
                    .unwrap_or_else(|| format!("Workspace {}", n + 1)),
            )?;
            put(&mut doc, projection::ROOM_OWNER_KEY, author.into())?;
            let owners = doc
                .put_object(
                    ROOT,
                    projection::ROOM_CONTENTS_OWNERS_KEY,
                    automerge::ObjType::Map,
                )
                .map_err(|e| e.to_string())?;
            doc.put(&owners, format!("{salt:04x}"), owner)
                .map_err(|e| e.to_string())?;
            if seeded.members.len() + seeded.grants.len() > MAX_GRANTS {
                return Err(format!(
                    "at most {MAX_GRANTS} members and grants a workspace"
                ));
            }
            let owner = Principal::Person(author.into());
            let mut capabilities = vec![];
            for (i, member) in seeded.members.iter().enumerate() {
                let who = if member.agent {
                    Principal::Agent(member.name.clone())
                } else {
                    Principal::Person(member.name.clone())
                };
                capabilities.push(
                    CapOverride::membership(
                        Subject::Principal(who),
                        id.to_string(),
                        member.access.into(),
                    )
                    .with_id(format!("member-{i}"))
                    .granted_by(owner.clone()),
                );
            }
            for (i, grant) in seeded.grants.iter().enumerate() {
                let scope = match &grant.over {
                    Some(app) => Scope::App(app.clone()),
                    None => Scope::Workspace,
                };
                capabilities.push(
                    CapOverride::grant(
                        Subject::Principal(Principal::App(grant.app.clone())),
                        scope,
                        grant.access.into(),
                    )
                    .with_id(format!("grant-{i}"))
                    .granted_by(owner.clone()),
                );
            }
            let mut room = Room {
                id,
                salt,
                doc,
                capabilities,
            };
            room.project(&strips, routes, ids.next_counter(), None)?;
            desktop.rooms.push(room);
        }
        Ok(desktop)
    }

    /// Whether `tab` is a window this owner created whose record the
    /// desktop must still create.
    pub(crate) fn is_fresh(&self, tab: u64) -> bool {
        self.fresh.contains(&tab)
    }
    /// The desktop created (or found) `tab`'s record.
    pub(crate) fn settle(&mut self, tab: u64) {
        self.fresh.remove(&tab);
    }
    /// The window to select once the next admission is applied.
    pub(crate) fn take_focus(&mut self) -> Option<u64> {
        self.focus.take()
    }
    /// The admitted document of `room`, for its workspace history.
    pub(crate) fn room(&self, room: RoomId) -> Option<&AutoCommit> {
        self.rooms.iter().find(|r| r.id == room).map(|r| &r.doc)
    }

    /// Reduce one bound command against its room, as the room's topology
    /// owner. An open selects the window it created in the next admission;
    /// with `record` (an existing record of the app in the room, as a seeded
    /// window's) the window shows it instead of a new one.
    pub(crate) fn apply(
        &mut self,
        bound: &BoundCommand,
        record: Option<&str>,
    ) -> Result<(), String> {
        if record.is_some()
            && !matches!(
                bound.command,
                DesktopCommand::OpenApp { .. } | DesktopCommand::OpenTab { .. }
            )
        {
            return Err("only an open names a record".into());
        }
        let room = self
            .rooms
            .iter_mut()
            .find(|room| room.id.to_string() == bound.source.room)
            .ok_or("command room is not in this desktop")?;
        let mut tree = DesktopTree::default();
        projection::apply_admitted_room(&mut tree, room.id, &room.doc)?;
        let workspace = tree.workspaces.first_mut().ok_or("workspace missing")?;
        let salt = room.salt;
        let owned = |id: u64| peer_salt_of(id) == salt;
        let resolved = resolve_open_app(workspace, bound.command.clone(), owned)?;
        check_owned(&resolved, owned)?;
        let stored = projection::desktop_counter_of(&room.doc).unwrap_or(1);
        let mut ids = IdMinter::resume(
            salt,
            tilo_desktop_model::commands::namespace_counter(workspace, salt, stored),
        );
        let mut routes = record_routes(&room.doc).into_iter().collect::<Vec<_>>();
        let (mut created, mut opened) = (vec![], vec![]);
        apply_command(
            workspace,
            &resolved,
            &mut || ids.mint_in_namespace(),
            &mut |app, tab| {
                let definition =
                    desktop::definition(app).ok_or("application cannot be opened here")?;
                match record {
                    Some(record) => {
                        let checkout =
                            desktop::checkout(definition, tab, &[(tab, record.into())].into())?;
                        routes.push((tab, checkout.id.as_str().into()));
                    }
                    None => {
                        routes.push((tab, fresh_route(definition, tab)));
                        created.push(tab);
                    }
                }
                opened.push(tab);
                Ok((definition.title.into(), tile(definition)))
            },
        )?;
        let renamed = matches!(resolved, DesktopCommand::RenameWorkspace { .. })
            .then(|| workspace.name.clone());
        let shortcuts = matches!(
            resolved,
            DesktopCommand::SaveShortcut { .. } | DesktopCommand::DeleteShortcut { .. }
        )
        .then(|| workspace.shortcuts.clone());
        room.project(&workspace.strips, routes, ids.next_counter(), renamed)?;
        if let Some(shortcuts) = shortcuts {
            projection::project_workspace_shortcuts(&mut room.doc, &shortcuts)
                .map_err(|e| e.to_string())?;
            commit(&mut room.doc);
        }
        self.fresh.extend(created.iter().copied());
        if matches!(
            resolved,
            DesktopCommand::OpenTab { .. } | DesktopCommand::DuplicateTab { .. }
        ) && let Some(tab) = opened.last()
        {
            self.focus = Some(*tab);
        }
        Ok(())
    }

    /// The desktop as admitted now: every room's workspace, in seed order.
    pub(crate) fn admission(&self) -> Result<Admission, String> {
        let mut tree = DesktopTree::default();
        let mut routes = std::collections::BTreeMap::new();
        for room in &self.rooms {
            projection::apply_admitted_room(&mut tree, room.id, &room.doc)?;
            routes.extend(record_routes(&room.doc));
            if let Some(workspace) = tree.workspaces.iter_mut().find(|w| w.room == room.id) {
                workspace.capabilities.clone_from(&room.capabilities);
            }
        }
        let mut required = vec![];
        let mut unsupported = vec![];
        for tab in tree
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
            .flat_map(|g| &g.tabs)
        {
            let Some(app) = canonical_app_id(&tab.tile) else {
                continue;
            };
            let Some(definition) = desktop::definition(&app) else {
                if !unsupported.contains(&app) {
                    unsupported.push(app);
                }
                continue;
            };
            let checkout = desktop::checkout(definition, tab.id, &routes)?;
            let requirement = Requirement {
                app_id: definition.app_id.into(),
                record_id: if checkout.id.is_package_primary() {
                    String::new()
                } else {
                    checkout.id.as_str().into()
                },
            };
            if !required.contains(&requirement) {
                required.push(requirement);
            }
        }
        let primary = &self.rooms.first().ok_or("no workspace")?.doc;
        Ok(Admission {
            admitted_room: primary.clone(),
            source_heads: primary.clone().get_heads().iter().map(|h| h.0).collect(),
            tree,
            routes,
            required,
            missing: vec![],
            unsupported,
            pending: 0,
        })
    }
}

/// Select `tab`'s window: its workspace, strip, group and the tab itself.
pub(crate) fn select(tree: &mut DesktopTree, tab: u64) -> Option<StripId> {
    let (workspace, strip, group) = tree.workspaces.iter().find_map(|w| {
        w.strips.iter().find_map(|s| {
            s.window_groups
                .iter()
                .find(|g| g.tabs.iter().any(|t| t.id == tab))
                .map(|g| (w.id, s.id, g.id))
        })
    })?;
    apply_local_view(tree, LocalViewIntent::Workspace(workspace));
    apply_local_view(tree, LocalViewIntent::Tab { group, tab });
    Some(strip)
}

impl Room {
    /// Project `strips` (all in this owner's namespace) into the room with
    /// the record each tab shows, as the room's topology owner commits it.
    fn project(
        &mut self,
        strips: &[Strip],
        routes: Vec<(u64, String)>,
        counter: u64,
        name: Option<String>,
    ) -> Result<(), String> {
        let doc = &mut self.doc;
        projection::project_owned_contents(doc, strips, self.salt, None, EmptyStrips::Retain)
            .map_err(|e| e.to_string())?;
        for strip in strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if let Some((_, record)) = routes.iter().find(|(id, _)| *id == tab.id) {
                        bind_tab_record(doc, strip.id, group.id, tab.id, record)
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        doc.put(ROOT, projection::ROOM_DESKTOP_COUNTER_KEY, counter)
            .map_err(|e| e.to_string())?;
        if let Some(name) = name {
            doc.put(ROOT, projection::ROOM_NAME_KEY, name)
                .map_err(|e| e.to_string())?;
        }
        commit(doc);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
