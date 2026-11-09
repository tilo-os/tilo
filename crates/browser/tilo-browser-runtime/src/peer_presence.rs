//! The page's presence in a workspace, for `web/peer.mjs`: the board that
//! merges everyone's words and this page's own word on it, driven on
//! [`tilo_live::presence::Board`] exactly as a native session drives it.
//!
//! A started page says what its person and shell show ([`say`]), publishes it
//! as the board's schedule allows ([`due`]), hears the members' words as they
//! arrive ([`hear`]), and reads back what the board holds ([`view`],
//! [`members`]). Nothing here draws, and nothing here keeps a second copy of
//! the driving: this module only names a page's own values and hands the
//! board's members to JavaScript as the [`Shown`] record every surface reads.

use std::cell::RefCell;

use collab_types::presence::{
    Payload, Presence, ShellChat, ShellPresence, ShellReaction, ShellSelection, ShellWindow, Shown,
    ShownLayout, ShownWave,
};
use serde::{Deserialize, Serialize};
use tilo_browser_peer::PeerHandle;
use tilo_live::presence::{Board, Own, Point, shown};
use tilo_user_identity::DeviceCert;
use wasm_bindgen::prelude::*;

use crate::peer::{JsRecord, js_error, workspace_number};

/// `{kind: 'person' | 'agent', label, color: '#rrggbb', activity}`.
#[derive(Clone, Serialize, Deserialize)]
struct JsParticipant {
    kind: String,
    label: String,
    color: String,
    #[serde(default)]
    activity: Option<String>,
}

/// Where this page's pointer is: over the tile of a tab of the workspace, in
/// that tile's logical pixels — the way every device's pointer travels. The
/// tab's id is a decimal string: it can pass 2^53.
#[derive(Clone, Serialize, Deserialize)]
struct JsPointer {
    tile: JsTile,
    at: [f32; 2],
}

#[derive(Clone, Serialize, Deserialize)]
struct JsTile {
    tab: String,
}

/// What this page says of itself in one workspace: its face and screen, the
/// window the person works in and their caret there, where its pointer is,
/// what it is saying beside it, its wave and who it follows. The page sends
/// it whole with each change; the board keeps what changed and says it under
/// its own schedule.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsPresence {
    /// This view's sequence, as the shell wire carries it: receivers order
    /// views by it, and one at or below the last they took is a repeat.
    #[serde(default)]
    seq: u32,
    #[serde(default)]
    sent_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    focus: Option<JsRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pointer: Option<JsPointer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selection: Option<ShellSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    participant: Option<JsParticipant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    place: Option<String>,
    /// Cursor chat, beside the pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat: Option<ShellChat>,
    /// The latest reaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reaction: Option<ShellReaction>,
    /// The latest wave, `target` the waved-at member's publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wave: Option<ShownWave>,
    /// Where the windows are: `{size, windows: [{app, record, rect, focused}]}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<ShownLayout>,
    /// Whose view this member follows: their publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    following: Option<String>,
}

/// This page's shell as its value names it: who it is, its screen, the
/// window the person works in and their caret there. `None` while the value
/// names nobody, which says nothing of a shell.
fn shell_from_js(value: &JsPresence) -> Result<Option<Box<ShellPresence>>, JsError> {
    let Some(participant) = &value.participant else {
        return Ok(None);
    };
    if !matches!(participant.kind.as_str(), "person" | "agent") {
        return Err(js_error("participant kind"));
    }
    let colour = participant.color.clone();
    let hex = colour
        .strip_prefix('#')
        .ok_or_else(|| js_error("participant colour must be #rrggbb"))?;
    if hex.len() != 6 || hex::decode(hex).is_err() {
        return Err(js_error("participant colour must be #rrggbb"));
    }
    let (size, mut windows) = match &value.layout {
        Some(layout) => (
            [u32::from(layout.size[0]), u32::from(layout.size[1])],
            layout
                .windows
                .iter()
                .map(|window| {
                    let record =
                        <[u8; 8]>::try_from(hex::decode(&window.record).map_err(js_error)?)
                            .map_err(|_| js_error("a window's record is a 16-hex digest"))?;
                    Ok(ShellWindow {
                        app: window.app.clone(),
                        record,
                        rect: window.rect,
                        focused: window.focused,
                    })
                })
                .collect::<Result<Vec<_>, JsError>>()?,
        ),
        None => ([0, 0], Vec::new()),
    };
    // The window the person works in is the one their caret is in, whatever
    // the layout said: the value names it on its own.
    if let Some(focus) = &value.focus {
        let mut found = false;
        for window in &mut windows {
            window.focused = window.app == focus.app && hex::encode(window.record) == focus.record;
            found |= window.focused;
        }
        if !found {
            windows.push(ShellWindow {
                app: focus.app.clone(),
                record: <[u8; 8]>::try_from(hex::decode(&focus.record).map_err(js_error)?)
                    .map_err(|_| js_error("a focused window's record is a 16-hex digest"))?,
                rect: [0, 0, 0, 0],
                focused: true,
            });
        }
    }
    Ok(Some(Box::new(ShellPresence {
        seq: value.seq,
        sent_at_ms: value.sent_at_ms,
        label: participant.label.clone(),
        agent: participant.kind == "agent",
        colour,
        activity: participant.activity.clone(),
        place: value.place.clone(),
        size,
        windows,
        selection: value.selection.clone(),
        chat: value.chat.clone(),
        reaction: value.reaction.clone(),
    })))
}

/// What the page's peer knows of one workspace's presence: where the
/// workspace stands on the plane, and the board it drives there.
struct Held {
    /// The workspace as the plane numbers it.
    number: u64,
    board: Board,
    /// Semantic work its agents narrated since the mounts last took it,
    /// admitted and bound to the member that published it.
    interactions: Vec<tilo_actions::SemanticInteraction>,
}

/// The most narrated work a workspace holds for its mounts between two
/// frames: a burst past it is older than what the frame shows anyway.
const MAX_HELD_INTERACTIONS: usize = 32;

/// A started page's presence: who it is, the handle it says on, and a board
/// per workspace it takes part in.
struct Page {
    device: [u8; 32],
    /// The device of the host this page's session is with.
    host: [u8; 32],
    handle: PeerHandle,
    /// The person this page belongs to, their root key as hex.
    user: String,
    /// What this page's device calls itself, from its certificate.
    device_label: String,
    /// The person's name here, as everyone reads it.
    label: String,
    workspaces: std::collections::HashMap<String, Held>,
}

thread_local! {
    /// The started pages' presence, by [`WasmPeer::id`].
    static PAGES: RefCell<std::collections::HashMap<u32, Page>> = RefCell::new(std::collections::HashMap::new());
}

/// Publish the burst `held` owes now, and answer when the next one is due.
fn publish(handle: &PeerHandle, held: &mut Held) -> Result<f64, JsError> {
    let now = now_ms();
    if let Some(frames) = held.board.due(now) {
        let frames: Vec<Presence> = frames.into_iter().map(|(_, frame)| frame).collect();
        handle
            .publish_presence(held.number, frames)
            .map_err(js_error)?;
    }
    Ok(held.board.next_due(now).unwrap_or(0.0))
}

/// Unix milliseconds, on the clock presence is heard on.
#[expect(
    clippy::cast_precision_loss,
    reason = "a millisecond clock far below 2^53"
)]
fn now_ms() -> f64 {
    ui::time::unix_now_ms() as f64
}

/// A started page's presence: what its workspaces' boards hold.
pub(crate) struct BoardView {
    /// How many times what is known here changed.
    pub(crate) revision: u64,
    /// This page's device in the workspace.
    pub(crate) device: [u8; 32],
    /// This page's own word.
    pub(crate) own: Own,
    /// Everyone here, as the board merged their words: where each of them
    /// points, on the tiles the surface passed.
    pub(crate) members: Vec<Shown>,
    /// Why this page has no live plane at all, in the words shown in the
    /// members' place.
    pub(crate) note: Option<String>,
}

/// Register `id`'s peer: the identity and handle its presence says on, and
/// the host (`host`, its certificate) its session is with.
pub(crate) fn register(
    id: u32,
    handle: &PeerHandle,
    cert: &DeviceCert,
    host: &DeviceCert,
    label: &str,
) {
    PAGES.with_borrow_mut(|pages| {
        pages.insert(
            id,
            Page {
                device: cert.device_public_key,
                host: host.device_public_key,
                handle: handle.clone(),
                user: hex::encode(cert.user_public_key),
                device_label: cert.device_label.clone(),
                label: label.to_owned(),
                workspaces: std::collections::HashMap::new(),
            },
        );
    });
}

/// Every started page of the person whose root is `user` (hex): what changes
/// for that person as a whole, such as a revocation their root signs, reaches
/// all of their sessions at once.
pub(crate) fn handles_of(user: &str) -> Vec<PeerHandle> {
    PAGES.with_borrow(|pages| {
        pages
            .values()
            .filter(|page| page.user == user)
            .map(|page| page.handle.clone())
            .collect()
    })
}

/// `id`'s session, and the device of the host it is with: where the
/// person's agent door is asked.
pub(crate) fn host_of(id: u32) -> Option<(PeerHandle, [u8; 32])> {
    PAGES.with_borrow(|pages| pages.get(&id).map(|page| (page.handle.clone(), page.host)))
}

/// Forget `id`'s peer and what it said.
pub(crate) fn release(id: u32) {
    PAGES.with_borrow_mut(|pages| {
        pages.remove(&id);
    });
}

/// The person's name here changed: say it again, so everyone reads it.
pub(crate) fn set_label(id: u32, name: &str) {
    PAGES.with_borrow_mut(|pages| {
        let Some(page) = pages.get_mut(&id) else {
            return;
        };
        name.clone_into(&mut page.label);
        let label = page.label.clone();
        let handle = page.handle.clone();
        for held in page.workspaces.values_mut() {
            let mut own = held.board.own().clone();
            own.label.clone_from(&label);
            held.board.say(&own);
            if let Err(error) = publish(&handle, held) {
                web_sys::console::warn_1(&JsValue::from_str(&format!("{error:?}")));
            }
        }
    });
}

impl Page {
    /// This page's own word, before anything it says.
    fn own(&self) -> Own {
        Own {
            label: self.label.clone(),
            device: self.device_label.clone(),
            user: Some(self.user.clone()),
            ..Own::default()
        }
    }

    fn held(&mut self, workspace: &str) -> Result<&mut Held, JsError> {
        if !self.workspaces.contains_key(workspace) {
            let number = workspace_number(workspace)?;
            self.workspaces.insert(
                workspace.to_owned(),
                Held {
                    number,
                    board: {
                        let mut board = Board::new(workspace, self.device);
                        board.seed(self.own());
                        board
                    },
                    interactions: Vec::new(),
                },
            );
        }
        self.workspaces
            .get_mut(workspace)
            .ok_or_else(|| js_error("not a workspace"))
    }

    /// Say `own` here and publish what the board owes now. Returns when the
    /// board next has something to say, on the clock the page hands back.
    fn say(&mut self, workspace: &str, own: &Own) -> Result<f64, JsError> {
        let handle = self.handle.clone();
        let held = self.held(workspace)?;
        held.board.say(own);
        publish(&handle, held)
    }

    /// Leave the workspace: the members take this page down now, and its
    /// board here is forgotten.
    fn leave(&mut self, workspace: &str) -> Result<(), JsError> {
        let workspace_id = workspace_number(workspace)?;
        self.handle
            .publish_presence(
                workspace_id,
                vec![Presence::desktop(workspace, Payload::Left)],
            )
            .map_err(js_error)?;
        self.workspaces.remove(workspace);
        Ok(())
    }
}

/// This page says `value` of itself in `workspace`: the board keeps what
/// changed and says it under one schedule. Answers when the board next has
/// something to say, in the page's own clock (`Date.now()` ms), or `0` when
/// it has nothing.
pub(crate) fn say(peer: u32, workspace: &str, value: &JsPresence) -> Result<f64, JsError> {
    let own = {
        let base = PAGES.with_borrow(|pages| pages.get(&peer).map(Page::own));
        let base = base.ok_or_else(|| js_error("not a started peer"))?;
        let pointer = match &value.pointer {
            Some(pointer) => Some(Point {
                tab: pointer
                    .tile
                    .tab
                    .parse()
                    .map_err(|_| js_error("a pointer's tab is a decimal u64"))?,
                x: pointer.at[0],
                y: pointer.at[1],
            }),
            None => None,
        };
        Own {
            following: value.following.clone(),
            wave: value.wave.clone(),
            pointer,
            shell: shell_from_js(value)?,
            ..base
        }
    };
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        page.say(workspace, &own)
    })
}

/// Publish what `peer`'s board for `workspace` owes now, and answer when it
/// next has something to say (`0` for nothing).
pub(crate) fn due(peer: u32, workspace: &str) -> Result<f64, JsError> {
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        let handle = page.handle.clone();
        let Some(held) = page.workspaces.get_mut(workspace) else {
            return Ok(0.0);
        };
        publish(&handle, held)
    })
}

/// Take one presence frame another member published in `workspace_id`: the
/// members' words merge into this page's board for it, and the mounts that
/// draw it ask for the frame that places what changed. A page with no board
/// there takes none.
pub(crate) fn hear(peer: u32, workspace_id: u64, from: [u8; 32], body: &[u8]) {
    let changed = PAGES.with_borrow_mut(|pages| {
        let Some(page) = pages.get_mut(&peer) else {
            return false;
        };
        let now = now_ms();
        page.workspaces
            .values_mut()
            .filter(|held| held.number == workspace_id)
            .any(|held| held.board.heard(from, body, now))
    });
    if changed {
        crate::browser::presence::heard(peer, workspace_id);
    }
}

/// Take semantic work `from` narrated on a record's live stream in
/// `workspace_id` (`event`): admitted only from a member here that this
/// page's board knows as an agent
/// ([`tilo_browser_peer::narration::admit`]), and held for the mounts that
/// draw the workspace, whose next frame shows it. Work from anyone else, or
/// that does not hold, is dropped.
pub(crate) fn narrated(
    peer: u32,
    workspace_id: u64,
    from: [u8; 32],
    event: &tilo_relay_protocol::live::SemanticFrame,
) {
    let admitted = PAGES.with_borrow_mut(|pages| {
        let Some(page) = pages.get_mut(&peer) else {
            return false;
        };
        page.workspaces
            .values_mut()
            .filter(|held| held.number == workspace_id)
            .any(|held| {
                let Some(interaction) =
                    tilo_browser_peer::narration::admit(&held.board, from, event)
                else {
                    return false;
                };
                if held.interactions.len() >= MAX_HELD_INTERACTIONS {
                    held.interactions.remove(0);
                }
                held.interactions.push(interaction);
                true
            })
    });
    if admitted {
        crate::browser::presence::heard(peer, workspace_id);
    }
}

/// The semantic work `peer`'s agents narrated in `workspace` since the last
/// call, oldest first. Empty, and allocating nothing, while there is none.
pub(crate) fn take_narrated(peer: u32, workspace: &str) -> Vec<tilo_actions::SemanticInteraction> {
    PAGES.with_borrow_mut(|pages| {
        pages
            .get_mut(&peer)
            .and_then(|page| page.workspaces.get_mut(workspace))
            .map(|held| std::mem::take(&mut held.interactions))
            .unwrap_or_default()
    })
}

/// What `peer`'s board for `workspace` holds: what the mount draws and lists.
/// `None` while this page has no board there.
pub(crate) fn view(peer: u32, workspace: &str, now_ms: f64) -> Option<BoardView> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        let held = page.workspaces.get(workspace)?;
        let board = &held.board;
        Some(BoardView {
            revision: board.revision(),
            device: page.device,
            own: board.own().clone(),
            members: board.here().map(|member| shown(member, now_ms)).collect(),
            note: page.handle.live_note(),
        })
    })
}

/// Everyone `peer`'s board for `workspace` has here, or `None` while this
/// page takes no part there.
pub(crate) fn members(peer: u32, workspace: &str) -> Option<Vec<Shown>> {
    PAGES.with_borrow(|pages| {
        let held = pages.get(&peer)?.workspaces.get(workspace)?;
        let now = now_ms();
        Some(held.board.here().map(|member| shown(member, now)).collect())
    })
}

/// `peer` leaves `workspace`: the members take it down now.
pub(crate) fn leave(peer: u32, workspace: &str) -> Result<(), JsError> {
    PAGES.with_borrow_mut(|pages| {
        pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?
            .leave(workspace)
    })
}
