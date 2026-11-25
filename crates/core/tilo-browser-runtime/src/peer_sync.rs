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
use tilo_session_actions::SessionActions;
use tilo_session_client::Link;
use tilo_session_client::{Client, Device, InCall, OwnPresence, Topic, Transport};
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
    handle: Link,
    /// The person this page belongs to, their root key as hex.
    user: String,
    /// The session's sync, saying this page's word on each workspace's presence track.
    client: Rc<Client>,
    /// The workspaces the page shows, by topic.
    workspaces: HashMap<WorkspaceId, String>,
    /// The workspace whose call this page is in, which its presence says.
    calling: Option<String>,
    /// The wave this page said last, so the same one said again is the same wave.
    waved: Option<u32>,
    /// The action APIs this page asks the host's actions through, one per workspace: one session path,
    /// shared by the page's Rust and its JavaScript.
    apis: RefCell<HashMap<WorkspaceId, SessionActions>>,
    /// Opens an API: the session's remote actions (their wire and the pump answering them) load
    /// with the peer's code, which registers the page, and not with the start module, which only
    /// asks for an API once a peer is in.
    open_api: fn(&Link, WorkspaceId, [u8; 32]) -> SessionActions,
}

/// An action API to `workspace`'s host actions over `handle`, with the host `host`.
fn open_api(handle: &Link, workspace: WorkspaceId, host: [u8; 32]) -> SessionActions {
    SessionActions::new(handle, workspace, host)
}

thread_local! {
    /// The started pages, by [`crate::peer::WasmPeer::id`].
    static PAGES: RefCell<HashMap<u32, Page>> = RefCell::new(HashMap::new());
}

/// Whether a page is registered as `id` now.
pub(crate) fn registered(id: u32) -> bool {
    PAGES.with_borrow(|pages| pages.contains_key(&id))
}

/// Register `id`'s peer: the identity and handle its presence says on, and the host (`host`, its
/// certificate) its session is with.
pub(crate) fn register(id: u32, handle: &Link, cert: &DeviceCert, host: &DeviceCert, label: &str) {
    let Ok(publisher) = EndpointId::from_bytes(&cert.device_public_key) else {
        return;
    };
    let (_heard, arrived) = std::sync::mpsc::channel();
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
            saying: Some(tilo_session_client::Saying::Frames(
                tilo_session_client::saying(handle),
            )),
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
                workspaces: HashMap::new(),
                calling: None,
                waved: None,
                apis: RefCell::default(),
                open_api,
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
/// (`0` for nothing).
pub(crate) fn due(peer: u32, workspace: &str) -> Result<f64, JsError> {
    workspace_id(workspace)?;
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        if !page.workspaces.values().any(|shown| shown == workspace) {
            return Ok(0.0);
        }
        // What this workspace's board owes, whether or not it is the one in front: a page shows
        // each of its workspaces at once, and each keeps its own schedule.
        page.client.due_in(workspace);
        Ok(page.client.next_due_in(workspace).unwrap_or(0.0))
    })
}

/// Take one presence frame another member published in `workspace_id`: their word merges into the
/// page's board for it. A page with no board there takes none.
pub(crate) fn hear(peer: u32, workspace_id: WorkspaceId, from: [u8; 32], body: &[u8]) {
    let workspace = PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        let workspace = page.workspaces.get(&workspace_id)?;
        let from = EndpointId::from_bytes(&from).ok()?;
        page.client.heard_body(workspace, from, body);
        Some(workspace.clone())
    });
    let Some(workspace) = workspace else {
        return;
    };
    // A page in a hidden tab has its timers held back by the browser, and a member silent for long
    // is taken down: what it owes leaves as it hears the others too, so a tab kept in the
    // background stays here while its connection does.
    let _ = due(peer, &workspace);
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

/// `peer` is in `call` in `workspace`, or in no call there (`None`): its presence there says so,
/// and everyone in the workspace lists it in the call's channel. Joining a call takes part in the
/// workspace and puts it in front, where this page's call is said.
pub(crate) fn call(peer: u32, workspace: &str, call: Option<InCall>) -> Result<(), JsError> {
    let number = workspace_id(workspace)?;
    PAGES.with_borrow_mut(|pages| {
        let page = pages
            .get_mut(&peer)
            .ok_or_else(|| js_error("not a started peer"))?;
        match &call {
            Some(_) => {
                page.workspaces.insert(number, workspace.to_owned());
                page.calling = Some(workspace.to_owned());
                page.follow(Some(number));
            }
            None if page.calling.as_deref() == Some(workspace) => page.calling = None,
            None => return Ok(()),
        }
        page.client.in_call(call);
        page.client.follow();
        Ok(())
    })
}

/// This page's call in `workspace`, if it is in one there.
pub(crate) fn own_call(peer: u32, workspace: &str) -> Option<InCall> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        if page.calling.as_deref() != Some(workspace) {
            return None;
        }
        page.client.own().call
    })
}

/// The voice channels of `workspace` anyone is in, as its presence says (this page's own call
/// among them), or `None` while this page takes no part there. Each device is its person as the
/// workspace's governance names it, when its presence named nobody yet.
pub(crate) fn channels(peer: u32, workspace: &str) -> Option<Vec<tilo_live::calls::Channel>> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        page.workspaces
            .values()
            .find(|shown| shown.as_str() == workspace)?;
        let heard: Vec<_> = page
            .client
            .calls()
            .into_iter()
            .filter(|call| call.workspace == workspace)
            .collect();
        let persons = page
            .handle
            .workspace_devices(workspace)
            .into_iter()
            .filter_map(|(device, person)| {
                let mut key = [0; 32];
                hex::decode_to_slice(&device, &mut key).ok()?;
                Some((key, person))
            })
            .collect();
        let own = page.client.own();
        let call = own
            .call
            .as_ref()
            .filter(|_| page.calling.as_deref() == Some(workspace));
        Some(tilo_live::calls::channels(
            &heard,
            call.map(|call| tilo_live::calls::Own {
                device: *page.client.publisher().as_bytes(),
                person: own.user.as_deref(),
                call,
            }),
            &persons,
            &page.handle.call_rules(workspace),
        ))
    })
}

/// What the call in `channel` of `workspace` lets this page's person do now, and what the call
/// allows; nothing for a workspace the page is not in.
pub(crate) fn allows(
    peer: u32,
    workspace: &str,
    channel: &str,
) -> Option<(tilo_live::calls::CallRule, tilo_live::calls::Directions)> {
    PAGES.with_borrow(|pages| {
        let page = pages.get(&peer)?;
        page.workspaces
            .values()
            .find(|shown| shown.as_str() == workspace)?;
        let rule = page
            .handle
            .call_rules(workspace)
            .remove(channel)
            .unwrap_or_default();
        let may = rule.may(page.client.own().user.as_deref());
        Some((rule, may))
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
