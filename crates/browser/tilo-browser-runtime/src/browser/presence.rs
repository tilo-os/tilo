//! Who is here, as the page's own frames read them: the workspace's presence
//! board, merged from every member's word ([`crate::peer`]), written into the
//! desk's members and into the frame's presence, so editors draw each
//! member's caret under their name and the desktop's roster lists them. The
//! pointers themselves are the session's to draw ([`tilo_session::people`]),
//! as on every host.

use tilo_desktop::here::LiveMember;
use ui::permissions::UserId;
use ui::presence::PresenceKey;
use wasm_bindgen::prelude::*;

use crate::peer_sync::BoardView;

use super::{Mount, error, with_mount_quietly};

/// What this mount keeps of who is here between frames: how far it read the board, the names and
/// carets it drew, what people say beside their pointer, and the frames it still owes.
#[derive(Default)]
pub(crate) struct Presence {
    /// How many times the board's contents had changed when it was last read.
    pub(crate) revision: u64,
    /// Who was here, by publisher.
    pub(crate) seen: Vec<String>,
    /// Who a caret is drawn for, by publisher: a member who stops selecting has theirs taken away.
    pub(crate) carets: Vec<String>,
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
        if !std::mem::take(&mut self.here_stale) {
            return;
        }
        let Some(desktop) = &self.desktop else {
            return;
        };
        let tree = desktop.handle.read(Clone::clone);
        let people = crate::shell::here_people(&desktop.here, &desktop.shared, &tree);
        let mut here = desktop.here.borrow_mut();
        self.frame.update_presence(|presence| {
            here.show_waves(presence);
            here.register_windows(&people, presence);
            true
        });
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
    /// for the desks and editors that draw them. Runs at the start of every frame and after each
    /// drawn one.
    pub(super) fn place_presence(&mut self) {
        let Some(peer) = self.peer else {
            return;
        };
        let workspace = self.workspace.clone();
        let Some(view) = crate::peer_sync::view(peer, &workspace) else {
            return;
        };
        if view.revision != self.presence.revision {
            self.presence.revision = view.revision;
            self.here_from(&view);
            self.names_from(&view);
        }
        // What the workspace's agents are doing: their cursor travels to the
        // control each action is bound to, over the window that shows it.
        let narrated = crate::peer_sync::take_narrated(peer, &workspace);
        if !narrated.is_empty() {
            self.frame.update_presence(|presence| {
                let mut shown = false;
                for interaction in narrated {
                    shown |= presence.apply_interaction(interaction).unwrap_or(false);
                }
                shown
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
        let Some(followed) = desktop.here.borrow().following().map(str::to_owned) else {
            self.followed_view = ui::headless::FollowedView::default();
            return;
        };
        if self.frame.follow_view(&followed, &mut self.followed_view) {
            self.pending = true;
            self.window.request_redraw();
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
            desktop.announce_arrivals();
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
        let present: Vec<String> = view.members.iter().map(|m| m.publisher.clone()).collect();
        let gone: Vec<String> = self
            .presence
            .seen
            .iter()
            .filter(|publisher| !present.contains(publisher))
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
            for publisher in &gone {
                presence.unregister_peer(publisher);
            }
            for member in &view.members {
                let user = UserId(member.publisher.clone());
                presence.set_participant_kind(&user, member.agent);
                if !member.label.is_empty() {
                    presence.set_display_name(user, member.label.clone());
                }
                match selection_anchors(member.selection.as_ref()) {
                    Some((key, start, end)) => {
                        use ui::collaboration::PresencePayload;
                        let payload = if start == end {
                            PresencePayload::Caret { pos: start }
                        } else {
                            PresencePayload::Selection { start, end }
                        };
                        presence.apply_remote_presence(member.publisher.clone(), key, payload);
                        carets.push(member.publisher.clone());
                    }
                    // Their caret went away: forgetting it costs their whole
                    // record, so the roster names them again next frame. Who
                    // they are is not theirs to lose by stopping.
                    None if had_caret.contains(&member.publisher) => {
                        presence.unregister_peer(&member.publisher);
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
    let anchor = tilo_relay_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.anchor).ok()?,
    };
    let head = tilo_relay_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.head).ok()?,
    };
    tilo_relay_protocol::live::read_presence_anchors(&anchor, &head)
}

impl Mount {
    /// The workspace this mount draws, as the live plane numbers it.
    fn workspace_number(&self) -> Option<u64> {
        crate::peer::workspace_number(&self.workspace).ok()
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
/// who belongs to the workspace (its door admitted them) names the people, so
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
                if !person.id.is_empty() && !name.is_empty() {
                    presence.set_display_name(UserId(person.id), name);
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
/// pointer that moved. A desk at rest draws nothing on its own, so without
/// this the pointer would wait for the next unrelated change.
/// One object of another device's window page reached this page: the mounts showing
/// `workspace` of `peer` build window `tab` again, so the frame that arrived is drawn, and ask for
/// the frame that shows it.
pub(crate) fn page_arrived(peer: u32, workspace: u64, tab: u64) {
    super::MOUNTS.with(|mounts| {
        // A page that arrives while a mount is being served is drawn by the frame that service asks
        // for: never a second borrow.
        let Ok(mounts) = mounts.try_borrow() else {
            return;
        };
        for mount in mounts.values() {
            if mount.peer == Some(peer) && mount.workspace_number() == Some(workspace) {
                if let Some(desktop) = &mount.desktop {
                    desktop.handle.rebuild(tab);
                }
                mount.window.request_redraw();
            }
        }
    });
}

pub(crate) fn heard(peer: u32, workspace: u64) {
    super::MOUNTS.with(|mounts| {
        // A word that arrives while a mount is being served is laid out by
        // the frame that service asks for: never a second borrow.
        let Ok(mounts) = mounts.try_borrow() else {
            return;
        };
        for mount in mounts.values() {
            if mount.peer == Some(peer) && mount.workspace_number() == Some(workspace) {
                mount.window.request_redraw();
            }
        }
    });
}
