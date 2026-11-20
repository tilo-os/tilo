//! The page's peer network: the session's own sync, over this page's endpoint.
//!
//! A started page runs the session's client as a desktop does — the board of who is here in each
//! workspace it shows, its own word on each presence track — and supplies the transport: the
//! certified peer's live session. This module names the page's own values and hands the board's
//! members to JavaScript as the `Shown` record every surface reads.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use collab_types::presence::Shown;
use iroh_base::{EndpointAddr, EndpointId};
use tilo_session::sync::{Client, Device, OwnPresence, Topic, Transport};
use tilo_session_actions::SessionActions;
use tilo_session_client::Link;
use tilo_user_identity::DeviceCert;
use wasm_bindgen::prelude::*;

use crate::peer::{js_error, workspace_id};
use crate::presence_value::{JsPresence, pointer_of, shell_from_js};
use tilo_server_protocol::WorkspaceId;

/// A started page's live session: the session's client, the transport ends it says through, and the
/// workspaces its mounts show.
struct Page {
    /// The device of the host this page's session is with.
    host: [u8; 32],
    /// The person the host acts for, its root key as hex: a host serves workspaces for people
    /// and is nobody in them itself.
    local_person: String,
    handle: Link,
    /// The person this page belongs to, their root key as hex.
    user: String,
    /// This page's own device key.
    device: [u8; 32],
    /// The session's sync, saying this page's word on each workspace's presence track.
    client: Rc<Client>,
    /// The workspaces the page shows, by topic.
    workspaces: HashMap<WorkspaceId, String>,
    /// The wave this page said last, so the same one said again is the same wave.
    waved: Option<u32>,
    /// Semantic work its agents narrated since the mounts last took it, by workspace, admitted and
    /// bound to the member that published it.
    narrated: tilo_session_client::narration::Held,
    /// The action APIs this page asks the host's actions through, one per workspace: one session path,
    /// shared by the page's Rust and its JavaScript.
    apis: RefCell<HashMap<WorkspaceId, SessionActions>>,
    /// Opens an API: the session's remote actions (their wire and the pump answering them) load
    /// with the peer's code, which registers the page, and not with the start module, which only
    /// asks for an API once a peer is in.
    open_api: fn(&Link, WorkspaceId, [u8; 32]) -> SessionActions,
    /// Which session this page is under (`STARTED`'s number as it registered): a page whose
    /// session was replaced is the same page under a later one.
    generation: u64,
}

/// An action API to `workspace`'s host actions over `handle`, with the host `host`.
fn open_api(handle: &Link, workspace: WorkspaceId, host: [u8; 32]) -> SessionActions {
    SessionActions::new(handle, workspace, host)
}

thread_local! {
    /// The started pages, by [`crate::peer::WasmPeer::id`].
    static PAGES: RefCell<HashMap<u32, Page>> = RefCell::new(HashMap::new());
    /// How many sessions the pages have opened in all: each registration takes the next
    /// number, so what held a page's earlier session beside its id (an event stream on its
    /// link) knows the replacement for what it is.
    static STARTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether a page is registered as `id` now.
pub(crate) fn registered(id: u32) -> bool {
    PAGES.with_borrow(|pages| pages.contains_key(&id))
}

/// Which session the page `id` is under: a number that changes each time a session opens as it.
pub(crate) fn generation(id: u32) -> Option<u64> {
    PAGES.with_borrow(|pages| pages.get(&id).map(|page| page.generation))
}

/// Why an action was not asked: the page holds no session with the workspace's server now.
pub(crate) fn no_server() -> tilo_actions::remote::ActionCallError {
    tilo_actions::remote::ActionCallError::Unavailable {
        reason: "The workspace's server can't be reached from here right now".into(),
    }
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
    /// Why this page has no live session at all, in the words shown in the
    /// members' place.
    pub(crate) note: Option<String>,
}

/// Register `id`'s peer: the identity and handle its presence says on, and the host (`host`, its
/// certificate) its session is with.
pub(crate) fn register(id: u32, handle: &Link, cert: &DeviceCert, host: &DeviceCert, label: &str) {
    // The page's replication loads with this code, not with the start module
    // (crate::replicated): from here on the shell, the shell and every mount
    // ask the workspace's documents through it.
    crate::replication::install();
    let Ok(publisher) = EndpointId::from_bytes(&cert.device_public_key) else {
        return;
    };
    let (_heard, arrived) = std::sync::mpsc::channel();
    let (_landed, enrolled) = std::sync::mpsc::channel();
    #[cfg_attr(
        all(target_arch = "wasm32", target_os = "unknown"),
        expect(
            clippy::arc_with_non_send_sync,
            reason = "a page runs on one thread, and its session's handle holds the page's engine"
        )
    )]
    let client = Rc::new(Client::over(
        Transport {
            outgoing: std::sync::Arc::new(handle.clone()),
            arrived,
            enrolled,
            saying: Some(tilo_session_client::saying(handle)),
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
    let generation = STARTED.with(|started| {
        let next = started.get().wrapping_add(1).max(1);
        started.set(next);
        next
    });
    PAGES.with_borrow_mut(|pages| {
        pages.insert(
            id,
            Page {
                host: host.device_public_key,
                local_person: hex::encode(host.user_public_key),
                handle: handle.clone(),
                user: hex::encode(cert.user_public_key),
                device: cert.device_public_key,
                client,
                workspaces: HashMap::new(),
                waved: None,
                narrated: tilo_session_client::narration::Held::default(),
                apis: RefCell::default(),
                open_api,
                generation,
            },
        );
    });
}

/// Every started page of the person whose root is `user` (hex): what changes for that person as a
/// whole, such as a revocation their root signs, reaches all of their sessions at once.
pub(crate) fn handles_of(user: &str) -> Vec<Link> {
    PAGES.with_borrow(|pages| {
        pages
            .values()
            .filter(|page| page.user == user)
            .map(|page| page.handle.clone())
            .collect()
    })
}

/// The device key of the started page `id`.
pub(crate) fn device_of(id: u32) -> Option<[u8; 32]> {
    PAGES.with_borrow(|pages| pages.get(&id).map(|page| page.device))
}

/// The person `id`'s workspace host acts for, its root key as hex: the host serves workspaces
/// for people and is nobody in them itself, so a list of a workspace's people never offers it.
pub(crate) fn local_person(id: u32) -> Option<String> {
    PAGES.with_borrow(|pages| pages.get(&id).map(|page| page.local_person.clone()))
}

/// The host handle of the started page `id`: what this page asks its session for over the peer it
/// is with.
pub(crate) fn handle_of(id: u32) -> Option<Link> {
    PAGES.with_borrow(|pages| pages.get(&id).map(|page| page.handle.clone()))
}

/// The action API to `workspace`'s host actions for `id`'s page, or `None` while its session has no
/// host: one session path per workspace, whichever of the page's callers asks.
pub(crate) fn actions_of(id: u32, workspace: WorkspaceId) -> Option<SessionActions> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&id)?;
        let mut apis = page.apis.borrow_mut();
        Some(
            apis.entry(workspace)
                .or_insert_with(|| (page.open_api)(&page.handle, workspace, page.host))
                .clone(),
        )
    })
}

/// The action API to `workspace`'s server through whichever session the page `id` holds as it is
/// asked: what a window that outlives one session asks, and follows across the session that
/// replaces it ([`tilo_session_actions::CurrentSession`]).
pub(crate) fn current_session(
    id: u32,
    workspace: WorkspaceId,
) -> tilo_session_actions::CurrentSession {
    tilo_session_actions::CurrentSession::new(
        move || {
            let generation = generation(id)?;
            let api = actions_of(id, workspace)?;
            Some((generation, std::sync::Arc::new(api) as _))
        },
        no_server,
    )
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
    });
    // The bar's own chip reads the name this page goes by: a name given in the profile card is
    // drawn at once, not at whatever change wakes the page next.
    crate::browser::wake_desktops();
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
        // The host this page's session was admitted by hosts what it shows: its word for the runs
        // it executes is heard, nobody else's.
        for (_, workspace) in &live {
            self.client.set_workspace_host(workspace, Some(self.host));
        }
        self.client.follow();
    }
}

/// This page says `value` of itself in `workspace`: the client keeps what changed and says it under
/// one schedule. Answers when it next has something to say, in the page's own clock (`Date.now()`
/// ms), or `0` when it has nothing.
pub(crate) fn say(peer: u32, workspace: &str, value: &JsPresence) -> Result<f64, JsError> {
    let number = workspace_id(workspace)?;
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
        page.client.set_where(
            pointer_of(value).map_err(js_error)?,
            shell_from_js(value).map_err(js_error)?,
        );
        let mut own = page.client.own();
        own.following.clone_from(&value.following);
        if let Some(member) = &value.member {
            own.profile_ms = own.profile_ms.max(member.profile_ms);
        }
        page.client.set_own(own);
        page.client.follow();
        Ok(page.client.next_due_in(workspace).unwrap_or(0.0))
    })
}

/// Publish what `peer` owes now (keepalives, repeats), and answer when it next has something to say
/// (`0` for nothing). A member the board forgot on the way, gone quiet, is taken down by the
/// mounts that draw the workspace.
pub(crate) fn due(peer: u32, workspace: &str) -> Result<f64, JsError> {
    workspace_id(workspace)?;
    let (next, forgot) = PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        let Some(workspace_id) = page
            .workspaces
            .iter()
            .find_map(|(id, shown)| (shown == workspace).then_some(*id))
        else {
            return Ok((0.0, None));
        };
        // What this workspace's board owes, whether or not it is the one in front: a page shows
        // each of its workspaces at once, and each keeps its own schedule.
        let before = page.client.here_revision(workspace);
        page.client.due_in(workspace);
        let forgot = (page.client.here_revision(workspace) != before).then_some(workspace_id);
        Ok::<_, JsError>((page.client.next_due_in(workspace).unwrap_or(0.0), forgot))
    })?;
    if let Some(workspace_id) = forgot {
        crate::browser::presence::heard(peer, workspace_id);
    }
    Ok(next)
}

/// Take one presence frame another member published in `workspace_id`: their word merges into the
/// page's board for it, and the mounts that draw it ask for the frame that places what changed. A
/// page with no board there takes none.
pub(crate) fn hear(peer: u32, workspace_id: WorkspaceId, from: [u8; 32], body: &[u8]) {
    let changed = PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        let workspace = page.workspaces.get(&workspace_id)?;
        let before = page.client.here_revision(workspace);
        let Ok(from) = EndpointId::from_bytes(&from) else {
            return None;
        };
        page.client.heard_body(workspace, from, body);
        Some((
            page.client.here_revision(workspace) != before,
            workspace.clone(),
        ))
    });
    let Some((changed, workspace)) = changed else {
        return;
    };
    if changed {
        crate::browser::presence::heard(peer, workspace_id);
    }
    // A page in a hidden tab has its timers held back by the browser, and a member silent for long
    // is taken down: what it owes leaves as it hears the others too, so a tab kept in the
    // background stays here while its connection does.
    let _ = due(peer, &workspace);
}

/// Take semantic work `from` narrated on a record's live stream in `workspace_id` (`event`):
/// admitted only from a member here that this page's board knows as an agent
/// ([`tilo_session_client::narration::admit`]), and held for the mounts that draw the workspace,
/// whose next frame shows it. Work from anyone else, or that does not hold, is dropped.
pub(crate) fn narrated(
    peer: u32,
    workspace_id: WorkspaceId,
    from: [u8; 32],
    event: &tilo_server_protocol::live::SemanticFrame,
) {
    let admitted = PAGES.with_borrow_mut(|pages| {
        let Some(page) = pages.get_mut(&peer) else {
            return false;
        };
        let Some(workspace) = page.workspaces.get(&workspace_id).cloned() else {
            return false;
        };
        let here = page.client.members(&workspace);
        page.narrated.hear(&workspace, &here, from, event)
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
            .map(|page| page.narrated.take(workspace))
            .unwrap_or_default()
    })
}

/// This page's own word, as its session says it, before the page takes part
/// in any workspace: the name the person joined under is the page's from the
/// moment its peer starts, so nothing shows them as a guest while the
/// workspace's board catches up ([`view`]).
pub(crate) fn own(peer: u32) -> Option<Shown> {
    PAGES.with_borrow(|pages| pages.get(&peer).map(|page| page.client.own_here()))
}

/// What `peer`'s board for `workspace` holds: what the mount draws and lists. `None` while this page
/// takes no part there.
pub(crate) fn view(peer: u32, workspace: &str) -> Option<BoardView> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        page.workspaces
            .values()
            .find(|shown| shown.as_str() == workspace)?;
        let (members, named) = with_persons(
            page.client.members(workspace),
            &page.handle.workspace_devices(workspace),
        );
        Some(BoardView {
            // What is known here changed: the board, or how many of its members the workspace's
            // governance names the person of.
            revision: page
                .client
                .here_revision(workspace)
                .wrapping_add(named << 32),
            own: page.client.own_here(),
            members,
            note: page.handle.live_note(),
        })
    })
}

/// `members` each as the person whose device they are: the workspace's governance names whose
/// each member device is (`devices`, device to person), so a member the live session never
/// certified to this page is still shown, called and counted as their person. With how many of
/// them the governance named.
fn with_persons(
    mut members: Vec<Shown>,
    devices: &std::collections::BTreeMap<String, String>,
) -> (Vec<Shown>, u64) {
    let mut named: u64 = 0;
    for member in &mut members {
        if let Some(person) = devices.get(&member.publisher) {
            member.user.get_or_insert_with(|| person.clone());
            named = named.saturating_add(1);
        }
    }
    (members, named)
}

/// Everyone `peer`'s board for `workspace` has here, or `None` while this page takes no part there.
pub(crate) fn members(peer: u32, workspace: &str) -> Option<Vec<Shown>> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        page.workspaces
            .values()
            .find(|shown| shown.as_str() == workspace)?;
        Some(
            with_persons(
                page.client.members(workspace),
                &page.handle.workspace_devices(workspace),
            )
            .0,
        )
    })
}

/// `peer` leaves `workspace`: the members take it down now.
pub(crate) fn leave(peer: u32, workspace: &str) -> Result<(), JsError> {
    let number = workspace_id(workspace)?;
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        // The client says this device left on the workspace's track, before the board that would
        // number its later words is gone.
        page.workspaces.remove(&number);
        let front = page.workspaces.keys().next().copied();
        page.follow(front);
        Ok(())
    })
}
