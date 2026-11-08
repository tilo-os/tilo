//! The page's presence in a workspace: the board that merges everyone's words
//! and this page's own word on it, for `web/peer.mjs`.
//!
//! A started page says what its person and shell show ([`say`]), publishes it
//! as the board's schedule allows ([`due`]), hears the members' words as they
//! arrive ([`hear`]), and reads back what the board holds ([`view`],
//! [`members`]). The board itself lives in [`tilo_live::presence`]; nothing
//! here draws: the mount places what [`view`] returns.

use std::cell::RefCell;

use collab_types::presence::{
    Payload, Presence, ShellChat, ShellPresence, ShellReaction, ShellSelection, ShellWindow,
};
use serde::{Deserialize, Serialize};
use tilo_browser_peer::PeerHandle;
use tilo_live::presence::{Board, Member, Own, Point, Tile, Wave};
use tilo_user_identity::DeviceCert;
use wasm_bindgen::prelude::*;

use crate::peer::{JsRecord, js_error, workspace_number};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsSelection {
    pub(crate) anchor: String,
    pub(crate) head: String,
}

/// `{kind: 'person' | 'agent', label, color: '#rrggbb', activity}`.
#[derive(Clone, Serialize, Deserialize)]
struct JsParticipant {
    kind: String,
    label: String,
    color: String,
    #[serde(default)]
    activity: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsWave {
    pub(crate) seq: u32,
    pub(crate) target: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsChat {
    pub(crate) text: String,
    pub(crate) sent: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsReaction {
    pub(crate) seq: u32,
    pub(crate) emoji: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsLayout {
    pub(crate) size: [u16; 2],
    pub(crate) windows: Vec<JsWindow>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct JsWindow {
    pub(crate) app: String,
    pub(crate) record: String,
    pub(crate) rect: [u16; 4],
    pub(crate) focused: bool,
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
    selection: Option<JsSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    participant: Option<JsParticipant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    place: Option<String>,
    /// Cursor chat: `{text, sent}`, beside the pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat: Option<JsChat>,
    /// The latest reaction: `{seq, emoji}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reaction: Option<JsReaction>,
    /// The latest wave: `{seq, target}`, `target` the waved-at member's
    /// publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wave: Option<JsWave>,
    /// Where the windows are: `{size, windows: [{app, record, rect,
    /// focused}]}`, `record` its 8-byte digest, hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<JsLayout>,
    /// Whose view this member follows: their publisher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    following: Option<String>,
}

/// One window of a sender's layout, as its wire names it: an app id and the
/// short digest of its record.
fn shell_window(window: &JsWindow) -> Result<ShellWindow, JsError> {
    let record = <[u8; 8]>::try_from(hex::decode(&window.record).map_err(js_error)?)
        .map_err(|_| js_error("a layout window's record is a 16-hex digest"))?;
    Ok(ShellWindow {
        app: window.app.clone(),
        record,
        rect: window.rect,
        focused: window.focused,
    })
}

fn layout_to_js(shell: &ShellPresence) -> JsLayout {
    JsLayout {
        size: [
            u16::try_from(shell.size[0]).unwrap_or(u16::MAX),
            u16::try_from(shell.size[1]).unwrap_or(u16::MAX),
        ],
        windows: shell
            .windows
            .iter()
            .map(|window| JsWindow {
                app: window.app.clone(),
                record: hex::encode(window.record),
                rect: window.rect,
                focused: window.focused,
            })
            .collect(),
    }
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
                .map(shell_window)
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
        selection: value.selection.as_ref().map(|selection| ShellSelection {
            anchor: selection.anchor.clone(),
            head: selection.head.clone(),
        }),
        chat: value.chat.as_ref().map(|chat| ShellChat {
            text: chat.text.clone(),
            sent: chat.sent,
        }),
        reaction: value.reaction.as_ref().map(|reaction| ShellReaction {
            seq: reaction.seq,
            emoji: reaction.emoji.clone(),
        }),
    })))
}

/// One member of a workspace, as the page reads who is here: the board's
/// merged word, flattened for JavaScript and for the mount that draws their
/// pointer, name and caret.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Present {
    /// Their device's publisher, hex.
    pub(crate) publisher: String,
    /// When this stay here began: it moves each time the device comes back
    /// after leaving.
    pub(crate) epoch: u64,
    /// Their presence says they are an agent.
    pub(crate) agent: bool,
    /// The name everyone here calls them.
    pub(crate) label: String,
    /// What they are doing, as their shell says it.
    pub(crate) activity: Option<String>,
    /// The colour their shell wears, `#rrggbb`.
    pub(crate) colour: String,
    /// The person behind the device (user root, hex), once certified.
    pub(crate) user: Option<String>,
    /// What their device calls itself, from its certificate.
    pub(crate) device: String,
    /// The window they work in and the record it shows.
    pub(crate) focus: Option<JsRecord>,
    /// Where within the workspace they are, as their shell says.
    pub(crate) place: Option<String>,
    /// Whose view they follow, by that member's publisher.
    pub(crate) following: Option<String>,
    /// Their latest wave at someone here.
    pub(crate) wave: Option<JsWave>,
    /// Where their pointer is over a tile, and how long ago they said it.
    pub(crate) pointer: Option<PointerNow>,
    /// The caret or selection they published, as live anchors (hex).
    pub(crate) selection: Option<JsSelection>,
    /// What they are saying beside their pointer.
    pub(crate) chat: Option<JsChat>,
    /// Their latest reaction.
    pub(crate) reaction: Option<JsReaction>,
    /// Where their windows are on their screen.
    pub(crate) layout: Option<JsLayout>,
    /// How available they said they were, in the wire's own word.
    pub(crate) availability: String,
    /// The voice channel they are in, as their presence carries it.
    pub(crate) call: Option<String>,
}

/// One member's pointer, over the tile of a tab, and when it was last heard.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PointerNow {
    /// The tab, as a decimal string (it can pass 2^53).
    pub(crate) tab: String,
    /// The point in that tile's logical pixels.
    pub(crate) at: [f64; 2],
    /// How long ago its publisher said it.
    pub(crate) age_ms: f64,
}

impl Present {
    fn of(member: &Member, now_ms: f64) -> Self {
        let shell = member.shell.as_deref();
        Self {
            publisher: hex::encode(member.publisher),
            epoch: member.epoch,
            agent: member.agent(),
            label: member.label().to_owned(),
            activity: member.activity().map(str::to_owned),
            colour: shell.map(|shell| shell.colour.clone()).unwrap_or_default(),
            user: member.user.clone(),
            device: member.device.clone(),
            focus: member.focus().map(|(app, record)| JsRecord {
                app: app.to_owned(),
                record: hex::encode(record),
            }),
            place: member.place().map(str::to_owned),
            following: member.following.clone(),
            wave: member.wave.as_ref().map(|wave| JsWave {
                seq: wave.seq,
                target: wave.target.clone(),
            }),
            pointer: member.pointer(now_ms).map(|(tab, x, y)| PointerNow {
                tab: tab.to_string(),
                at: [x, y],
                age_ms: member.pointer_heard_at().map_or(0.0, |at| now_ms - at),
            }),
            selection: shell
                .and_then(|shell| shell.selection.as_ref())
                .map(|selection| JsSelection {
                    anchor: selection.anchor.clone(),
                    head: selection.head.clone(),
                }),
            chat: shell
                .and_then(|shell| shell.chat.as_ref())
                .map(|chat| JsChat {
                    text: chat.text.clone(),
                    sent: chat.sent,
                }),
            reaction: shell
                .and_then(|shell| shell.reaction.as_ref())
                .map(|reaction| JsReaction {
                    seq: reaction.seq,
                    emoji: reaction.emoji.clone(),
                }),
            layout: shell.map(layout_to_js),
            availability: member.availability.id().to_owned(),
            call: member.call.as_ref().map(|call| call.call.encode()),
        }
    }
}

/// What the page's peer knows of one workspace's presence: the board that
/// merges everyone's word, and this page's own as it last said it.
struct Held {
    /// The workspace as the plane numbers it.
    number: u64,
    board: Board,
    own: Own,
}

/// A started page's presence: who it is, the handle it says on, and a board
/// per workspace it takes part in.
struct Page {
    device: [u8; 32],
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
    /// This page's device in the workspace, hex.
    pub(crate) publisher: String,
    /// This page's own word.
    pub(crate) own: Own,
    /// Everyone here, as the board merged their words.
    pub(crate) members: Vec<Present>,
    /// Every member whose pointer is over a tile the surface shows, where
    /// that tile stands: `(publisher, tab, x, y, heard_at)`.
    pub(crate) placed: Vec<([u8; 32], u64, f64, f64, f64)>,
    /// Why this page has no live plane at all, in the words shown in the
    /// members' place.
    pub(crate) note: Option<String>,
}

/// Register `id`'s peer: the identity and handle its presence says on.
pub(crate) fn register(
    id: u32,
    device: [u8; 32],
    handle: &PeerHandle,
    cert: &DeviceCert,
    label: &str,
) {
    PAGES.with_borrow_mut(|pages| {
        pages.insert(
            id,
            Page {
                device,
                handle: handle.clone(),
                user: hex::encode(cert.user_public_key),
                device_label: cert.device_label.clone(),
                label: label.to_owned(),
                workspaces: std::collections::HashMap::new(),
            },
        );
    });
}

/// Forget `id`'s peer and what it said.
pub(crate) fn release(id: u32) {
    PAGES.with_borrow_mut(|pages| {
        pages.remove(&id);
    });
}

/// The person's name here changed: say it again, so everyone reads it.
pub(crate) fn set_label(id: u32, name: &str) {
    let next = PAGES.with_borrow_mut(|pages| {
        let page = pages.get_mut(&id)?;
        name.clone_into(&mut page.label);
        let mut said = Vec::new();
        for (workspace, held) in &mut page.workspaces {
            let mut own = held.own.clone();
            own.label.clone_from(&page.label);
            held.board.say(&own);
            held.own = own;
            said.push(workspace.clone());
        }
        Some(said)
    });
    for workspace in next.unwrap_or_default() {
        let due = PAGES.with_borrow_mut(|pages| {
            let page = pages.get_mut(&id)?;
            let mut held = page.workspaces.remove(&workspace)?;
            let next = page.publish(&mut held);
            page.workspaces.insert(workspace, held);
            Some(next)
        });
        if let Some(Err(error)) = due {
            web_sys::console::warn_1(&JsValue::from_str(&format!("{error:?}")));
        }
    }
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
                    board: Board::new(workspace, self.device),
                    own: self.own(),
                },
            );
        }
        self.workspaces
            .get_mut(workspace)
            .ok_or_else(|| js_error("not a workspace"))
    }

    /// Say `own` here and publish what the board owes now. Returns when the
    /// board next has something to say, on the clock the page hands back.
    fn say(&mut self, workspace: &str, own: Own) -> Result<f64, JsError> {
        let held = self.held(workspace)?;
        held.board.say(&own);
        held.own = own;
        let mut held = self
            .workspaces
            .remove(workspace)
            .ok_or_else(|| js_error("not a workspace"))?;
        let next = self.publish(&mut held);
        self.workspaces.insert(workspace.to_owned(), held);
        next
    }

    /// Publish the burst due now, and answer when the next one is.
    fn publish(&self, held: &mut Held) -> Result<f64, JsError> {
        let now = now_ms();
        if let Some(burst) = held.board.due(now) {
            let frames: Vec<Presence> = burst.into_iter().map(|(_, frame)| frame).collect();
            self.handle
                .publish_presence(held.number, frames)
                .map_err(js_error)?;
        }
        Ok(held.board.next_due(now).unwrap_or(0.0))
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
            wave: value.wave.as_ref().map(|wave| Wave {
                seq: wave.seq,
                target: wave.target.clone(),
            }),
            pointer,
            shell: shell_from_js(value)?,
            ..base
        }
    };
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        page.say(workspace, own)
    })
}

/// Publish what `peer`'s board for `workspace` owes now, and answer when it
/// next has something to say (`0` for nothing).
pub(crate) fn due(peer: u32, workspace: &str) -> Result<f64, JsError> {
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        let mut held = page.workspaces.remove(workspace);
        let Some(mut taken) = held.take() else {
            return Ok(0.0);
        };
        let next = page.publish(&mut taken);
        page.workspaces.insert(workspace.to_owned(), taken);
        next
    })
}

/// Take one presence frame another member published in `workspace_id`: the
/// members' words merge into this page's board for it, and the mounts that
/// draw it ask for the frame that places what changed. `None` for how this
/// page has no board there.
pub(crate) fn hear(peer: u32, workspace_id: u64, from: [u8; 32], body: &[u8]) {
    let Some(frame) = tilo_live::presence::decode(body) else {
        return;
    };
    let changed = PAGES.with_borrow_mut(|pages| {
        let Some(page) = pages.get_mut(&peer) else {
            return false;
        };
        let mut changed = false;
        for held in page
            .workspaces
            .values_mut()
            .filter(|held| held.number == workspace_id)
        {
            changed |= held.board.hear(from, &frame, now_ms());
        }
        changed
    });
    if changed {
        crate::browser::presence::heard(peer, workspace_id);
    }
}

/// What `peer`'s board for `workspace` holds, with the members' pointers
/// placed over `tiles`: what the mount draws and lists. `None` while this
/// page has no board there.
pub(crate) fn view(peer: u32, workspace: &str, tiles: &[Tile], now_ms: f64) -> Option<BoardView> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        let held = page.workspaces.get(workspace)?;
        Some(BoardView {
            revision: held.board.revision(),
            publisher: hex::encode(page.device),
            own: held.own.clone(),
            members: held
                .board
                .here()
                .map(|member| Present::of(member, now_ms))
                .collect(),
            placed: {
                let placed = held.board.placed(tiles, now_ms);
                held.board
                    .here()
                    .filter_map(|member| {
                        let heard = member.pointer_heard_at()?;
                        let at = placed.iter().find(|at| at.publisher == member.publisher)?;
                        Some((member.publisher, at.tab, at.x, at.y, heard))
                    })
                    .collect()
            },
            note: page.handle.live_note(),
        })
    })
}

/// Everyone `peer`'s board for `workspace` has here, or `None` while this
/// page takes no part there.
pub(crate) fn members(peer: u32, workspace: &str) -> Option<Vec<Present>> {
    PAGES.with_borrow(|pages| {
        let held = pages.get(&peer)?.workspaces.get(workspace)?;
        let now = now_ms();
        Some(
            held.board
                .here()
                .map(|member| Present::of(member, now))
                .collect(),
        )
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
