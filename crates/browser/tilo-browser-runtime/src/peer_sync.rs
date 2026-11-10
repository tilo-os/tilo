//! The page's collaboration plane: the session's own sync, over this page's endpoint.
//!
//! A started page runs the session's client as a desktop does — the board of who is here in each
//! workspace it shows, its own word on each presence track — and supplies the transport: the
//! certified peer's live plane. This module names the page's own values and hands the board's
//! members to JavaScript as the `Shown` record every surface reads.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use collab_types::presence::{
    Payload, Presence, ShellChat, ShellPresence, ShellReaction, ShellSelection, ShellWindow, Shown,
    ShownLayout, ShownWave,
};
use iroh::{EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};
use tilo_session::sync::{Client, Cmd, Device, OwnPresence, SayPresence, Topic, Transport};
use tilo_session_host::PeerHandle;
use tilo_session_host::session_actions::SessionActions;
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
/// it whole with each change; the client keeps what changed and says it under
/// the session's schedule.
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

/// Where this page's pointer is, on the tile it names. `None` while it names none.
fn pointer_of(value: &JsPresence) -> Result<Option<tilo_live::presence::Point>, JsError> {
    value
        .pointer
        .as_ref()
        .map(|pointer| {
            Ok(tilo_live::presence::Point {
                tab: pointer
                    .tile
                    .tab
                    .parse()
                    .map_err(|_| js_error("a pointer's tab is a decimal u64"))?,
                x: pointer.at[0],
                y: pointer.at[1],
            })
        })
        .transpose()
}

/// This page's presence tracks: the live plane its certified peer holds for each workspace, the one
/// every device of the workspace says presence on.
struct PagePlane {
    handle: PeerHandle,
}

impl std::fmt::Debug for PagePlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("page plane")
    }
}

impl SayPresence for PagePlane {
    fn say(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        sequence: u32,
        frame: &Presence,
    ) -> Result<(), String> {
        // The plane numbers the objects it carries itself; `sequence` is the board's count.
        let _ = sequence;
        let topic =
            workspace_number(&workspace.to_string()).map_err(|error| format!("{error:?}"))?;
        self.handle.publish_presence(topic, vec![frame.clone()])
    }
}

/// The most narrated work a workspace holds for its mounts between two frames: a burst past it is
/// older than what the frame shows anyway.
const MAX_HELD_INTERACTIONS: usize = 32;

/// A started page's plane: the session's client, the transport ends it says through, and the
/// workspaces its mounts show.
struct Page {
    /// The device of the host this page's session is with.
    host: [u8; 32],
    handle: PeerHandle,
    /// The person this page belongs to, their root key as hex.
    user: String,
    /// The session's sync, saying this page's word on each workspace's presence track.
    client: Rc<Client>,
    /// What the session says to the plane that this page has no carrier for yet: the live plane a
    /// workspace's presence rides is opened by the mount that shows it, so a join or a leave has
    /// nothing here to do. Draining it keeps a session that says them from queueing.
    said: RefCell<tokio::sync::mpsc::UnboundedReceiver<Cmd>>,
    /// The workspaces the page shows, by topic.
    workspaces: HashMap<Topic, String>,
    /// The wave this page said last, so the same one said again is the same wave.
    waved: Option<u32>,
    /// Semantic work its agents narrated since the mounts last took it, by workspace, admitted and
    /// bound to the member that published it.
    interactions: HashMap<String, Vec<tilo_actions::SemanticInteraction>>,
    /// The doors this page asks the host's actions through, one per workspace: one session path,
    /// shared by the page's Rust and its JavaScript.
    doors: RefCell<HashMap<u64, tilo_session_host::session_actions::SessionActions>>,
}

thread_local! {
    /// The started pages, by [`crate::peer::WasmPeer::id`].
    static PAGES: RefCell<HashMap<u32, Page>> = RefCell::new(HashMap::new());
}

/// A started page's board for one workspace, as a mount draws it.
pub(crate) struct BoardView {
    /// How many times what is known here changed.
    pub(crate) revision: u64,
    /// This page's own word.
    pub(crate) own: Shown,
    /// Everyone here, as the board merged their words: where each of them
    /// points, on the tiles the surface passed.
    pub(crate) members: Vec<Shown>,
    /// Why this page has no live plane at all, in the words shown in the
    /// members' place.
    pub(crate) note: Option<String>,
}

/// Register `id`'s peer: the identity and handle its presence says on, and the host (`host`, its
/// certificate) its session is with.
pub(crate) fn register(
    id: u32,
    handle: &PeerHandle,
    cert: &DeviceCert,
    host: &DeviceCert,
    label: &str,
) {
    let Ok(publisher) = EndpointId::from_bytes(&cert.device_public_key) else {
        return;
    };
    let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
    let (_heard, arrived) = std::sync::mpsc::channel();
    let (_landed, enrolled) = std::sync::mpsc::channel();
    let client = Rc::new(Client::over(
        Transport {
            outgoing: Rc::new(outgoing),
            arrived,
            enrolled,
            saying: Some(Rc::new(PagePlane {
                handle: handle.clone(),
            })),
        },
        Device {
            publisher,
            endpoint: EndpointAddr::new(publisher),
            person: Some(cert.user_public_key),
            name: cert.device_label.clone(),
        },
        OwnPresence {
            label: label.to_owned(),
            user: Some(hex::encode(cert.user_public_key)),
            ..OwnPresence::default()
        },
    ));
    PAGES.with_borrow_mut(|pages| {
        pages.insert(
            id,
            Page {
                host: host.device_public_key,
                handle: handle.clone(),
                user: hex::encode(cert.user_public_key),
                client,
                said: RefCell::new(said),
                workspaces: HashMap::new(),
                waved: None,
                interactions: HashMap::new(),
                doors: RefCell::default(),
            },
        );
    });
}

/// Every started page of the person whose root is `user` (hex): what changes for that person as a
/// whole, such as a revocation their root signs, reaches all of their sessions at once.
pub(crate) fn handles_of(user: &str) -> Vec<PeerHandle> {
    PAGES.with_borrow(|pages| {
        pages
            .values()
            .filter(|page| page.user == user)
            .map(|page| page.handle.clone())
            .collect()
    })
}

/// The door to `workspace`'s host actions for `id`'s page, or `None` while its session has no
/// host: one session path per workspace, whichever of the page's callers asks.
pub(crate) fn actions_of(id: u32, workspace: u64) -> Option<SessionActions> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&id)?;
        let mut doors = page.doors.borrow_mut();
        Some(
            doors
                .entry(workspace)
                .or_insert_with(|| SessionActions::new(&page.handle, workspace, page.host))
                .clone(),
        )
    })
}

/// `id`'s session's sync, which says this page's word and reads who is here: the session's own
/// roster reads it too.
pub(crate) fn client(id: u32) -> Option<Rc<Client>> {
    PAGES.with_borrow(|pages| pages.get(&id).map(|page| Rc::clone(&page.client)))
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
        let mut own = page.client.own();
        name.clone_into(&mut own.label);
        page.client.set_own(own);
        page.take_said();
    });
}

impl Page {
    /// Take part in the workspaces this page shows, with `front` in front, and take what arrived.
    fn follow(&self, front: Option<Topic>) {
        let live: Vec<(Topic, String)> = self
            .workspaces
            .iter()
            .map(|(topic, workspace)| (*topic, workspace.clone()))
            .collect();
        self.client.follow_workspaces(&live, front);
        self.take_said();
        self.client.follow();
    }

    /// What the session said to the plane, which this page has no carrier for yet.
    fn take_said(&self) {
        while self.said.borrow_mut().try_recv().is_ok() {}
    }
}

/// This page says `value` of itself in `workspace`: the client keeps what changed and says it under
/// one schedule. Answers when it next has something to say, in the page's own clock (`Date.now()`
/// ms), or `0` when it has nothing.
pub(crate) fn say(peer: u32, workspace: &str, value: &JsPresence) -> Result<f64, JsError> {
    let number = workspace_number(workspace)?;
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        page.workspaces.insert(number, workspace.to_owned());
        page.follow(Some(number));
        if let Some(wave) = &value.wave
            && page.waved != Some(wave.seq)
        {
            page.waved = Some(wave.seq);
            page.client.wave(&wave.target);
        }
        // Where this page is said after it takes part: a workspace this page just joined hears its
        // whole word.
        page.client
            .set_where(pointer_of(value)?, shell_from_js(value)?);
        let mut own = page.client.own();
        own.following.clone_from(&value.following);
        page.client.set_own(own);
        page.client.follow();
        page.take_said();
        Ok(page.client.next_due_in(workspace).unwrap_or(0.0))
    })
}

/// Publish what `peer` owes now (keepalives, repeats), and answer when it next has something to say
/// (`0` for nothing).
pub(crate) fn due(peer: u32, workspace: &str) -> Result<f64, JsError> {
    workspace_number(workspace)?;
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        if !page.workspaces.values().any(|shown| shown == workspace) {
            return Ok(0.0);
        }
        page.take_said();
        // What this workspace's board owes, whether or not it is the one in front: a page shows
        // each of its workspaces at once, and each keeps its own schedule.
        page.client.due_in(workspace);
        Ok(page.client.next_due_in(workspace).unwrap_or(0.0))
    })
}

/// Take one presence frame another member published in `workspace_id`: their word merges into the
/// page's board for it, and the mounts that draw it ask for the frame that places what changed. A
/// page with no board there takes none.
pub(crate) fn hear(peer: u32, workspace_id: u64, from: [u8; 32], body: &[u8]) {
    let changed = PAGES.with_borrow(|pages| {
        let Some(page) = pages.get(&peer) else {
            return false;
        };
        let Some(workspace) = page.workspaces.get(&workspace_id) else {
            return false;
        };
        let before = page.client.here_revision(workspace);
        let Ok(from) = EndpointId::from_bytes(&from) else {
            return false;
        };
        page.client.heard_body(workspace, from, body);
        page.client.here_revision(workspace) != before
    });
    if changed {
        crate::browser::presence::heard(peer, workspace_id);
    }
}

/// Take semantic work `from` narrated on a record's live stream in `workspace_id` (`event`):
/// admitted only from a member here that this page's board knows as an agent
/// ([`tilo_session_host::narration::admit`]), and held for the mounts that draw the workspace,
/// whose next frame shows it. Work from anyone else, or that does not hold, is dropped.
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
        let Some(workspace) = page.workspaces.get(&workspace_id).cloned() else {
            return false;
        };
        let here = page.client.members(&workspace);
        let Some(interaction) = tilo_session_host::narration::admit(&here, from, event) else {
            return false;
        };
        let held = page.interactions.entry(workspace).or_default();
        if held.len() >= MAX_HELD_INTERACTIONS {
            held.remove(0);
        }
        held.push(interaction);
        true
    });
    if admitted {
        crate::browser::presence::heard(peer, workspace_id);
    }
}

/// The semantic work `peer`'s agents narrated in `workspace` since the last call, oldest first.
/// Empty, and allocating nothing, while there is none.
pub(crate) fn take_narrated(peer: u32, workspace: &str) -> Vec<tilo_actions::SemanticInteraction> {
    PAGES.with_borrow_mut(|pages| {
        pages
            .get_mut(&peer)
            .and_then(|page| page.interactions.remove(workspace))
            .unwrap_or_default()
    })
}

/// What `peer`'s board for `workspace` holds: what the mount draws and lists. `None` while this page
/// takes no part there.
pub(crate) fn view(peer: u32, workspace: &str) -> Option<BoardView> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        page.workspaces
            .values()
            .find(|shown| shown.as_str() == workspace)?;
        Some(BoardView {
            revision: page.client.here_revision(workspace),
            own: page.client.own_here(),
            members: page.client.members(workspace),
            note: page.handle.live_note(),
        })
    })
}

/// Everyone `peer`'s board for `workspace` has here, or `None` while this page takes no part there.
pub(crate) fn members(peer: u32, workspace: &str) -> Option<Vec<Shown>> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        page.workspaces
            .values()
            .find(|shown| shown.as_str() == workspace)?;
        Some(page.client.members(workspace))
    })
}

/// `peer` leaves `workspace`: the members take it down now.
pub(crate) fn leave(peer: u32, workspace: &str) -> Result<(), JsError> {
    let number = workspace_number(workspace)?;
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        // Said before the board that numbers this device's frames is gone.
        page.handle
            .publish_presence(number, vec![Presence::desktop(workspace, Payload::Left)])
            .map_err(js_error)?;
        page.workspaces.remove(&number);
        let front = page.workspaces.keys().next().copied();
        page.follow(front);
        Ok(())
    })
}
