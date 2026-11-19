//! Who is here, as the page's own frames read them: the workspace's presence
//! board, merged from every member's word ([`crate::peer`]), written into the
//! shell's members and into the frame's presence, so editors draw each
//! member's caret under their name and the desktop's roster lists them. The
//! pointers themselves are the session's to draw ([`tilo_session::people`]),
//! as on every host.

use tilo_desktop::here::LiveMember;
use ui::permissions::MemberId;
use ui::presence::PresenceKey;
use ui::tokens::PresenceHue;
use wasm_bindgen::prelude::*;

use crate::peer_sync::BoardView;
use tilo_server_protocol::WorkspaceId;

use super::{Mount, error, with_mount_quietly};

/// What this mount keeps of who is here between frames: how far it read the board, the names and
/// carets it drew, what people say beside their pointer, and the frames it still owes.
#[derive(Default)]
pub(crate) struct Presence {
    /// How many times the board's contents had changed when it was last read.
    pub(crate) revision: u64,
    /// Who was here, by publisher.
    pub(crate) seen: Vec<MemberId>,
    /// Who a caret is drawn for, by publisher: a member who stops selecting has theirs taken away.
    pub(crate) carets: Vec<MemberId>,
    /// The names of the workspace's people as its governance last gave them, as this mount put
    /// them on its presence.
    pub(crate) named: std::collections::BTreeMap<String, String>,
    /// Who the workspace lists as its members, as this mount put them on its presence: who its
    /// windows offer as people, whether or not each is here now.
    pub(crate) members: Option<std::collections::BTreeSet<String>>,
    /// The workspace's agents, as Chat's agents resource names each, and as this mount put them
    /// on its presence: a cursor named, and an agent a retired one no longer is.
    pub(crate) agents: Vec<(MemberId, String, PresenceHue)>,
}

impl Mount {
    /// Who is here changed: the next frame puts them on the windows they are
    /// in. Presence arrives many times a frame while people type and move,
    /// so it is left to that frame, never laid out on arrival.
    pub(super) fn apply_here(&mut self) {
        self.here_stale = true;
        self.pending = true;
        self.window.request_redraw();
    }

    /// Put who is here on the windows they are in, as the frame begins.
    /// Remote carets clear a member's registrations as they move, so this
    /// runs in each frame after one arrived. The frame lays the change out.
    pub(super) fn place_here(&mut self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        // When the workspace admitted each of its people numbers the namesakes among them: the
        // governance's times, projected onto the shell, which the roster numbers them by too.
        let joined = desktop.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id().to_string() == desktop.workspace)
                .map(|held| held.policy.joined.clone())
                .unwrap_or_default()
        });
        let numbered = desktop.here.borrow_mut().set_joined(&joined);
        let renamed = *desktop.shared.names.borrow() != self.presence.named;
        let members = desktop.shared.members.borrow().clone();
        let admitted = members != self.presence.members;
        if !std::mem::take(&mut self.here_stale) && !numbered && !renamed && !admitted {
            return;
        }
        self.presence.named = desktop.shared.names.borrow().clone();
        self.presence.members.clone_from(&members);
        let tree = desktop.handle.read(Clone::clone);
        let people = crate::shell::here_people(&desktop.here, &desktop.shared, &tree);
        let mut here = desktop.here.borrow_mut();
        let own = here.own_user().map(str::to_owned);
        let own_label = here.own_label().map(str::to_owned);
        // A host serves workspaces for people and is nobody in them itself: the person its own
        // device acts for is no row of a list of the workspace's people, unless it is this
        // page's own.
        let host = self.peer.and_then(crate::peer_sync::local_person);
        let offered =
            |person: &str| host.as_deref() != Some(person) || own.as_deref() == Some(person);
        let named = &self.presence.named;
        self.frame.update_presence(|presence| {
            // Who the workspace says is in it: its windows offer exactly these as its people,
            // and a device that named someone it never admitted is nobody's row.
            if let Some(members) = &members {
                presence.set_workspace_members(
                    members
                        .iter()
                        .filter(|person| offered(person))
                        .map(|person| MemberId::person(person.clone())),
                );
            }
            // This page's own person is one of the workspace's members: its list names them, so
            // a window shows them as themselves after a reload too, not only once the board
            // says who is here.
            if let Some(own) = own.as_deref() {
                let name = named
                    .get(own)
                    .cloned()
                    .or_else(|| own_label.clone())
                    .unwrap_or_default();
                if !name.trim().is_empty() {
                    presence.set_self_member(MemberId::person(own), name);
                }
            }
            // Everyone the workspace lists is named as its governance calls them, here or not:
            // a change someone made while away reads as theirs, never as their key. Who is here
            // is then named as their presence says, as everywhere else.
            for (person, name) in named {
                if own.as_deref() != Some(person.as_str())
                    && offered(person)
                    && let Some(member) = MemberId::parsed_person(person)
                {
                    presence.set_display_name(member, name);
                }
            }
            here.show_waves(presence);
            here.register_windows(&people, presence);
            true
        });
    }

    /// Name the workspace's agents as the agents resource names them, and forget each the
    /// resource retired, so an app's people list offers exactly those it keeps now.
    fn place_agents(&mut self) {
        let Some(desktop) = &mut self.desktop else {
            return;
        };
        if desktop.shared.agents.is_none() {
            // The resource arrives after the page does: open it again until the store has it,
            // as the windows' own resources are opened again when they arrive.
            let shared = &mut desktop.shared;
            let opened = shared.owners.as_ref().and_then(|owners| {
                crate::page_agents::open(
                    owners,
                    &shared.stores,
                    &shared.workspace,
                    desktop.author.clone(),
                )
            });
            if opened.is_some() {
                shared.agents = opened;
            }
        }
        let Some(resource) = &desktop.shared.agents else {
            return;
        };
        let Some(state) = resource.try_get() else {
            return;
        };
        let mut agents: Vec<(MemberId, String, PresenceHue)> = state
            .active()
            .filter_map(|agent| {
                let name = agent.name.trim();
                (!name.is_empty()).then(|| {
                    let id = MemberId::agent(agent.id.0.clone());
                    let hue = PresenceHue::of_member(&id);
                    (id, name.to_owned(), hue)
                })
            })
            .collect();
        agents.sort_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then(a.0.cmp(&b.0))
        });
        if agents == self.presence.agents {
            return;
        }
        let retired: Vec<MemberId> = self
            .presence
            .agents
            .iter()
            .map(|(agent, _, _)| agent.clone())
            .filter(|agent| !agents.iter().any(|(kept, _, _)| kept == agent))
            .collect();
        self.presence.agents.clone_from(&agents);
        self.frame.update_presence(|presence| {
            for agent in &retired {
                presence.unregister_peer(agent);
            }
            for (agent, name, hue) in agents {
                presence.set_display_name(agent.clone(), name);
                presence.set_presence_hue(agent, hue);
            }
            true
        });
        self.pending = true;
        self.window.request_redraw();
    }

    /// The tiles this mount shows, where the workspace's pointers land.
    fn tiles(&self) -> Vec<tilo_live::presence::Tile> {
        self.desktop.as_ref().map_or_else(Vec::new, |desktop| {
            desktop
                .handle
                .shown_tabs()
                .into_iter()
                .map(|(tab, area)| {
                    tilo_live::presence::Tile::new(
                        tab,
                        area.x0,
                        area.y0,
                        area.width(),
                        area.height(),
                    )
                })
                .collect()
        })
    }

    /// Who is here, placed: what each member is, what they are called, and where their caret is,
    /// for the shells and editors that draw them. Runs at the start of every frame and after each
    /// drawn one.
    pub(super) fn place_presence(&mut self) {
        // The workspace's agents are members of it like people are, named whether or not a
        // device of theirs works here now: an app's people list offers them by their names.
        self.place_agents();
        let Some(peer) = self.peer else {
            return;
        };
        let workspace = self.workspace.clone();
        let Some(view) = crate::peer_sync::view(peer, &workspace) else {
            // Not in the workspace's board yet: the page still names its own
            // person as the session does, so the bar never shows them as a
            // guest while the board catches up.
            self.own_before_board(peer);
            return;
        };
        // A board nobody has said anything on yet stands at the revision a
        // desktop starts from, so the page's own word is taken while the
        // desktop has none: until then the bar would show its person as a
        // guest.
        let unnamed = self
            .desktop
            .as_ref()
            .is_some_and(|desktop| desktop.here.borrow().own_label().is_none())
            && !view.own.label.trim().is_empty();
        if view.revision != self.presence.revision || unnamed {
            self.presence.revision = view.revision;
            self.here_from(&view);
            self.names_from(&view);
        }
        // What the workspace's agents are doing: their cursor travels to the
        // control each action is bound to, over the window that shows it.
        let narrated = crate::peer_sync::take_narrated(peer, &workspace);
        // An agent acting for this person shows them where it acts, even where it already was.
        if let Some(desktop) = &self.desktop {
            tilo_desktop::here::note_narrated(&mut desktop.here.borrow_mut(), &narrated);
        }
        if !narrated.is_empty() {
            self.frame.update_presence(|presence| {
                tilo_desktop::here::apply_narrated(presence, &narrated)
            });
            self.pending = true;
            self.window.request_redraw();
        }
    }

    /// While this page follows an agent, the view inside the window it
    /// works in goes where it acts: the words its caret sits in scroll into
    /// view, else the bound control of its newest action, in the sheet, tab or
    /// page that shows it. Nobody followed, nothing moves the person's view.
    pub(super) fn follow_view(&mut self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        let followed = {
            let here = desktop.here.borrow();
            here.following()
                .and_then(|publisher| here.member_of(publisher))
        };
        let Some(followed) = followed else {
            self.followed_view = ui::headless::FollowedView::default();
            return;
        };
        if self.frame.follow_view(&followed, &mut self.followed_view) {
            self.pending = true;
            self.window.request_redraw();
        }
    }

    /// This page's own person, as its session names them, while the
    /// workspace's board has not come in: who else is here is not known yet.
    fn own_before_board(&mut self, peer: u32) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        if desktop.here.borrow().own_label().is_some() {
            return;
        }
        let Some(own) = crate::peer_sync::own(peer) else {
            return;
        };
        let own = LiveMember::of(&own);
        if own.label.trim().is_empty() {
            return;
        }
        let changed = desktop.here.borrow_mut().set_members(Some(own), None);
        if changed {
            desktop.shared.wake();
            self.apply_here();
        }
    }

    /// The board's members as the desktop's roster reads them.
    fn here_from(&mut self, view: &BoardView) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        let members = view.members.iter().map(LiveMember::of).collect();
        let own = LiveMember::of(&view.own);
        let changed = {
            let mut here = desktop.here.borrow_mut();
            let members = here.set_members(Some(own), Some(members));
            let note = here.set_unavailable(view.note.clone());
            members || note
        };
        if changed {
            // The bar reads the strips' counts in its update pass, which a
            // bare redraw skips: without the wake it would paint them only on
            // its next other change, such as the clock's minute.
            desktop.shared.wake();
            self.apply_here();
        }
    }

    /// The members' names, kinds and carets, as their presence says them:
    /// editors draw the caret, labelled, in the next frame. Its colour is the
    /// workspace's to settle, never the one the member publishes, so two
    /// people here at once are never drawn alike.
    fn names_from(&mut self, view: &BoardView) {
        let members: Vec<(MemberId, &collab_types::presence::Shown)> = view
            .members
            .iter()
            .filter_map(|member| Some((member.member_id()?, member)))
            .collect();
        let present: Vec<MemberId> = members.iter().map(|(id, _)| id.clone()).collect();
        let gone: Vec<MemberId> = self
            .presence
            .seen
            .iter()
            .filter(|member| !present.contains(member))
            .cloned()
            .collect();
        self.presence.seen = present;
        if view.members.is_empty() && gone.is_empty() {
            return;
        }
        let had_caret = std::mem::take(&mut self.presence.carets);
        let mut carets = Vec::new();
        let mut cleared = !gone.is_empty();
        self.frame.update_presence(|presence| {
            for member in &gone {
                presence.unregister_peer(member);
            }
            for (user, member) in &members {
                // A device names the member behind it. The workspace's own host is a device of
                // nobody in it: its label is no name a list of the workspace's people shows.
                let admitted = !presence.knows_members()
                    || presence.is_member(user)
                    || presence.is_own_identity(user)
                    || user.is_agent();
                if admitted && !member.label.is_empty() {
                    presence.set_display_name(user.clone(), member.label.clone());
                }
                match selection_anchors(member.selection.as_ref()) {
                    Some((key, start, end)) => {
                        use ui::collaboration::PresencePayload;
                        let payload = if start == end {
                            PresencePayload::Caret { pos: start }
                        } else {
                            PresencePayload::Selection { start, end }
                        };
                        presence.apply_remote_presence(user.clone(), key, payload);
                        carets.push(user.clone());
                    }
                    // Their caret went away: forgetting it costs their whole
                    // record, so the roster names them again next frame. Who
                    // they are is not theirs to lose by stopping.
                    None if had_caret.contains(user) => {
                        presence.unregister_peer(user);
                        cleared = true;
                    }
                    None => {}
                }
            }
            true
        });
        self.presence.carets = carets;
        if cleared {
            self.apply_here();
        }
    }
}

/// A member's published caret or selection, as the record it is in and its
/// anchors.
fn selection_anchors(
    selection: Option<&collab_types::presence::ShellSelection>,
) -> Option<(PresenceKey, usize, usize)> {
    let selection = selection?;
    let anchor = tilo_server_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.anchor).ok()?,
    };
    let head = tilo_server_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.head).ok()?,
    };
    tilo_server_protocol::live::read_presence_anchors(&anchor, &head)
}

impl Mount {
    /// The workspace this mount draws.
    fn workspace_id(&self) -> Option<WorkspaceId> {
        crate::peer::workspace_id(&self.workspace).ok()
    }

    /// Show the pointer shape the surface asks for where the pointer is, as
    /// a native window does: the canvas's CSS cursor, set only on a change.
    pub(super) fn show_cursor(&mut self) {
        let cursor = self.frame.pointer_cursor();
        if cursor != self.cursor {
            self.cursor = cursor;
            self.window.set_cursor(cursor);
        }
    }
}

/// This person's pointer at `(x, y)` on the surface (CSS pixels), as the
/// workspace's live layer carries it: `{tile: {tab}, at}` over the tile of the
/// tab it is over, in that tile's logical pixels. `None` over no tab, which
/// publishes no pointer.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn pointer_tile(id: u64, x: f32, y: f32) -> Result<Option<String>, JsValue> {
    // A read: it changes nothing a semantics tree shows.
    with_mount_quietly(id, |m| {
        let point = (f64::from(x), f64::from(y));
        let Some((tab, at_x, at_y)) = tilo_live::presence::anchor(&m.tiles(), point) else {
            return Ok(None);
        };
        serde_json::to_string(&serde_json::json!({
            "tile": {"tab": tab.to_string()},
            "at": [at_x, at_y],
        }))
        .map(Some)
        .map_err(error)
    })
}

/// Name the people of the mount's workspace, `[{id, name}]`, `id` each
/// person's key as their actions record it (what `cx.me()` and
/// `ActionCx::author` answer). Live presence names devices; a page that knows
/// who belongs to the workspace (its entry admitted them) names the people, so
/// an app shows `cx.people().name(author)` as the person's name rather than
/// their key.
///
/// # Errors
///
/// Rejects malformed JSON or a closed mount.
#[wasm_bindgen]
pub fn set_people(id: u64, people: &str) -> Result<(), JsValue> {
    #[derive(serde::Deserialize)]
    struct Person {
        id: String,
        name: String,
    }
    let people: Vec<Person> =
        serde_json::from_str(people).map_err(|error| JsValue::from_str(&error.to_string()))?;
    with_mount_quietly(id, |m| {
        m.frame.update_presence(|presence| {
            for person in people {
                let name = person.name.trim();
                if let Some(member) = MemberId::parsed_person(&person.id)
                    && !name.is_empty()
                {
                    presence.set_display_name(member, name);
                }
            }
            true
        });
        m.apply_here();
        Ok(())
    })
}

/// A member's word arrived in `workspace` of `peer`: a mount that draws it
/// asks for a frame at once, because the next frame is what places the
/// pointer that moved. A shell at rest draws nothing on its own, so without
/// this the pointer would wait for the next unrelated change.
/// One object of another device's window page reached this page: the mounts showing
/// `workspace` of `peer` build window `tab` again, so the frame that arrived is drawn, and ask for
/// the frame that shows it.
pub(crate) fn page_arrived(peer: u32, workspace: WorkspaceId, tab: u64) {
    super::MOUNTS.with(|mounts| {
        // A page that arrives while a mount is being served is drawn by the frame that service asks
        // for: never a second borrow.
        let Ok(mounts) = mounts.try_borrow() else {
            return;
        };
        for mount in mounts.values() {
            if mount.peer == Some(peer) && mount.workspace_id() == Some(workspace) {
                if let Some(desktop) = &mount.desktop {
                    desktop.handle.rebuild(tab);
                }
                mount.window.request_redraw();
            }
        }
    });
}

pub(crate) fn heard(peer: u32, workspace: WorkspaceId) {
    super::MOUNTS.with(|mounts| {
        // A word that arrives while a mount is being served is laid out by
        // the frame that service asks for: never a second borrow.
        let Ok(mounts) = mounts.try_borrow() else {
            return;
        };
        for mount in mounts.values() {
            if mount.peer == Some(peer) && mount.workspace_id() == Some(workspace) {
                mount.window.request_redraw();
            }
        }
    });
}
