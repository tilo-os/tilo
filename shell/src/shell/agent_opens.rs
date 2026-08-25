//! Opening an application because a participant without a window asked for
//! one.
//!
//! Every other way an app opens in this session begins with somebody looking
//! at the screen: the launcher, the palette, a click on a row. A participant
//! that holds a room, a name and a capability set but no screen has none of
//! those, and until now that left it able to act on whichever apps somebody
//! else happened to have open — a reach decided by accident rather than by
//! what it was permitted to do.
//!
//! So it asks, on the one connection it has ([`CollabRequest::OpenApp`]), and
//! this is where the session decides. Three properties are the whole design:
//!
//! * **The asking and the deciding are different places.** The broker knows
//!   sockets, rooms and names; it does not know workspaces, capabilities or
//!   launchers, and it does not learn them here. It hands the question over
//!   and the desktop answers it on its own thread against its own state.
//! * **The answer is the room's, not the asker's.** It is decided against the
//!   capabilities of the workspace the app would open into, by the same
//!   `decide_capability` every other act in this session passes, and it is no
//!   unless something says yes.
//! * **A refusal is said.** It goes back to the connection that asked, naming
//!   the application and the reason. An agent that is not told cannot tell a
//!   refusal from a slow start, and will report the wrong one.
//!
//! ## Two ways in, one gate
//!
//! A request also arrives here when a participant opened an application in a
//! workspace this session *shares* — a workspace carries its own
//! collaborators, so an application opened in one is the workspace's and not
//! the opening machine's, and the room says so
//! (`NeighbourMsg::AppOpened`). That path is a peer's account of what a
//! participant asked for, never a peer's authority over this desktop: it is
//! put through the identical decision below, against this session's own
//! workspace, and a session whose policy does not cover the participant opens
//! nothing and writes down why. The only thing the origin changes is where a
//! refusal goes — see [`OpenOrigin`].
//!
//! ## What a grant has to say
//!
//! Opening `dev.qos.photos` asks about `ResourceRef::App("dev.qos.photos")`
//! with [`Verb::Write`] — the same question `search::visibility` resolves a
//! launch to for a person at the keyboard, so an agent is held to the gate
//! people are held to rather than to one written for it. The consequence is
//! the useful part:
//!
//! * `Scope::App("dev.qos.photos")` — may open **this** application.
//! * `Scope::Workspace` — may open applications.
//! * `Scope::Action { app, action }` — may perform one operation in an app
//!   that is already running, and may not start one.
//!
//! ## What this check is worth, exactly
//!
//! It is a second gate, not the only one. The participant asking has already
//! asked itself the same question against its own capability set — that is
//! how the agent's whole surface is built — and this exists because a survey
//! filtered by the caller is a claim by the caller.
//!
//! Its weakness is named rather than papered over: the name it decides about
//! is the one the connection announced on its `Hello`, and that field is not
//! authenticated. Anything that can reach the session's collaboration socket
//! can call itself whatever it likes, so this narrows a grant to the
//! participant it was written for without proving the participant is that
//! one. It is worth having anyway — an unnamed connection, or one naming a
//! participant the room granted nothing, is refused outright — and it is not
//! worth mistaking for identity. Signing this handshake is what would make it
//! one.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use collab_types::caps::{ResourceRef, Verb};

use crate::shell::chrome_state::Chrome;
use crate::shell::collab_broker::{BrokerHandle, OpenAppRequest, OpenOrigin};
use crate::shell::launcher;
use crate::shell::state::{Capability, FsMode, GRANT_SESSION};

/// Requests that have arrived and not yet been decided.
///
/// The broker's reader thread cannot answer one: the workspace, its
/// capabilities and the launcher all live on the thread that draws. So the
/// question is parked here and taken up on the next frame, which is also what
/// makes the notification the launch raises appear on the same frame as the
/// decision.
#[derive(Default)]
pub struct PendingOpens {
    queue: Mutex<Vec<OpenAppRequest>>,
}

impl PendingOpens {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Point the session's broker at this queue.
    pub fn install(self: &Arc<Self>, broker: &BrokerHandle) {
        let queue = Arc::clone(self);
        broker.set_app_opener(Box::new(move |req| {
            if let Ok(mut pending) = queue.queue.lock() {
                pending.push(req);
            }
        }));
    }

    fn take(&self) -> Vec<OpenAppRequest> {
        self.queue
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }
}

/// Decide every request that has arrived, and act on the ones that pass.
///
/// Called once per frame. Cheap when the queue is empty, which is every frame
/// but the handful where a participant asked for something.
pub fn pump(chrome: &Chrome, broker: &BrokerHandle, pending: &PendingOpens) {
    for req in pending.take() {
        // Resolve what it is being opened *onto* before deciding anything, so
        // "I do not have that content" is answered as itself rather than as a
        // window that opened empty. An app started without the thing the
        // request was about is the failure this whole path exists to end: the
        // photo editor comes up with no photograph, publishes no operation on
        // one, and the step that wanted it cropped arrives at an empty window
        // — which reads, from outside, exactly like an agent that did nothing.
        let onto = match stage_content(chrome, &req) {
            Ok(onto) => onto,
            Err(why) => {
                tracing::warn!(
                    app = %req.app_id, with = ?req.with,
                    "refused to open an application onto content: {why}",
                );
                refuse(broker, &req, &why);
                continue;
            }
        };
        match decide(chrome, &req) {
            Ok(display_name) => {
                tracing::info!(
                    app = %req.app_id, who = ?req.handle, room = %req.room,
                    onto = ?onto,
                    "opening an application because a participant asked",
                );
                // From here it is an ordinary launch: the same placement, the
                // same "Opening …" notice, the same failure reporting a person
                // clicking the row would get. A window that opened because an
                // agent needed it must not be a different kind of window.
                if let Err(e) =
                    crate::shell::search::providers::applications::launch_installed_app_with(
                        chrome,
                        &req.app_id,
                        &display_name,
                        onto.as_deref(),
                    )
                {
                    // The person watching has already been told by the
                    // launcher's own error notification. The participant that
                    // asked has not, and it is the one that is waiting.
                    refuse(broker, &req, &format!("{e}"));
                }
            }
            Err(why) => {
                tracing::warn!(
                    app = %req.app_id, who = ?req.handle, room = %req.room,
                    "refused to open an application: {why}",
                );
                refuse(broker, &req, &why);
            }
        }
    }
}

/// Say no, to whoever is in a position to hear it.
///
/// A request that arrived on this session's own socket has a participant
/// blocked on the answer, and it gets one — an agent that is not told cannot
/// tell a refusal from a slow start. A request that arrived over the room does
/// not: the participant asked another session and was answered there, and the
/// peer that relayed the fact is not owed a reply. What that one gets is a log
/// line, because a workspace where one machine has the window and another
/// silently does not is precisely the divergence somebody will have to explain.
fn refuse(broker: &BrokerHandle, req: &OpenAppRequest, why: &str) {
    match req.origin {
        OpenOrigin::Connection(peer) => broker.refuse_open(peer, &req.app_id, why),
        OpenOrigin::Room(peer) => tracing::warn!(
            from = peer, app = %req.app_id, room = %req.room, who = ?req.handle,
            "this session will not open an application the rest of the \
             workspace has: {why}",
        ),
    }
}

/// Put the content an open names where the application can read it, and
/// grant it exactly that much.
///
/// `Ok(None)` is an open that named no object — the ordinary case, and
/// unchanged by any of this.
///
/// # Why the session resolves it and not the app
///
/// What crosses the socket is a content hash, because that is the only name
/// for a picture that means the same thing in two applications that know
/// nothing about each other (see [`CollabRequest::OpenApp`]). But an
/// application is started with a *path*: `launch_with_document` hands it
/// `--open <path>`, which is the same contract a person double-clicking a
/// file gets, and the photo editor's `--open` is the same code either way. So
/// the hash is turned into a file here, on the one side of the socket that
/// has both a store to resolve it against and the authority to say what the
/// app may read.
///
/// The bytes go to a directory of this session's own rather than into the
/// user's documents. Content that arrived because an agent asked for a window
/// is not a file the person created, and writing it where their own files
/// live would leave them tidying up after their assistant.
///
/// # The grant is minted here, narrowly, and it is not implied by the ask
///
/// Naming content grants nothing — the same rule `build_launch_command`
/// states for paths. The app being opened is granted **read** on the staging
/// directory and nothing else, for the session; it is not given the home, and
/// an open that was refused above never reaches this at all.
fn stage_content(chrome: &Chrome, req: &OpenAppRequest) -> Result<Option<PathBuf>, String> {
    let Some(hash) = req.with.as_deref().map(str::trim).filter(|h| !h.is_empty()) else {
        return Ok(None);
    };
    // Asked of this machine's store, which is where content an app offered
    // over the socket is kept (`collab_broker::keep_local_blob`). A hash
    // nothing ever offered is not a slow arrival, it is an address for bytes
    // that are not here, and saying so is the whole difference between a
    // refusal and a window that opens empty.
    let Some(bytes) = qdoc::blobstore::store().get(hash) else {
        return Err(format!(
            "I don't have the content that names ({}…) on this machine, so \
             there is nothing to open it onto",
            &hash[..hash.len().min(12)],
        ));
    };
    let dir = staging_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!(
            "I couldn't make somewhere to put the content ({e})"
        ));
    }
    // Named by the content's own hash, with the extension its bytes say it
    // has: two opens of the same picture reuse one file, and an app that
    // decides what to do by extension gets the right answer.
    let meta = qdoc::blobstore::sniff(&bytes);
    let ext = match meta.mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    };
    let path = dir.join(format!("{hash}.{ext}"));
    if !path.exists() {
        if let Err(e) = std::fs::write(&path, &bytes) {
            return Err(format!("I couldn't put the content where it could be opened ({e})"));
        }
    }
    // Read, on the directory, for this session. The app is about to be handed
    // a path inside it and would otherwise find nothing there: the launcher
    // binds what the manifest and the capability file allow and nothing else.
    {
        let mut st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        let Some(ws) = tree.active_workspace_mut() else {
            return Err(
                "there is no workspace open here, so there is nowhere to open it into"
                    .to_string(),
            );
        };
        ws.grant_app(
            &req.app_id,
            &Capability::Filesystem {
                path: dir.clone(),
                mode: FsMode::Read,
                scope: GRANT_SESSION,
            },
        );
    }
    chrome.borrow_mut().bump();
    Ok(Some(path))
}

/// Where content an open named is put for the application to read.
///
/// Under the runtime directory rather than the user's home: it belongs to
/// this session, goes away with it, and is never mistaken for something the
/// person saved.
fn staging_dir() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("qos-opened")
}

/// May this request open this application here, and what is the application
/// called?
///
/// The name comes back on success because the launch says it out loud — the
/// "Opening …" notice a person reads — and it is the app's own, out of the
/// manifest, never the id the caller sent.
fn decide(chrome: &Chrome, req: &OpenAppRequest) -> Result<String, String> {
    // An unnamed connection is refused before any lookup. A capability names
    // a subject; a connection that named nobody cannot be the subject of one,
    // and treating "no name" as a name would ask the workspace about the
    // empty string.
    let Some(handle) = req.handle.as_deref().filter(|h| !h.trim().is_empty()) else {
        return Err(
            "I can't tell who is asking — this connection announced no name, \
             so there is no grant that could cover it"
                .to_string(),
        );
    };
    // Then what was asked for, so "no such application" is answered as itself
    // rather than as a permission failure. The two are different facts and a
    // caller that cannot tell them apart retries the wrong one.
    let Some(app) = launcher::gui_apps()
        .into_iter()
        .find(|a| a.package_id == req.app_id)
    else {
        return Err(format!(
            "no application called {} is installed here",
            req.app_id
        ));
    };
    let st = chrome.borrow();
    let tree = st.tree();
    let Some(ws) = tree.active_workspace() else {
        return Err(
            "there is no workspace open here, so there is nowhere to open it \
             into and no capabilities to decide by"
                .to_string(),
        );
    };
    // The same question a person's launch passes, about the same resource:
    // `search::visibility::required_capability` resolves a launch of this
    // package to `App(package)` + `Write`, and this asks it of the agent
    // rather than of the person.
    let actor = ws.actor_for_agent(handle);
    let resource = ResourceRef::App(req.app_id.clone());
    if !ws.allows(&actor, &resource, Verb::Write) {
        // Said at `warn`, with the subject spelled exactly as a grant would
        // have to spell it, because the commonest cause of this refusal is
        // not a decision anybody made — it is a grant written for a name that
        // differs from the announced one by its case. `agent:claude` does not
        // cover a participant announcing itself as `Claude`: the name is
        // matched literally (see the module header), and the two are
        // different principals.
        //
        // Loudly, because from the outside this refusal is indistinguishable
        // from an app that was slow to start. The participant is told, but
        // the person who wrote the grant is looking at a log, and "nothing
        // happened" is the one thing this must never look like.
        tracing::warn!(
            app = %req.app_id, announced = %handle,
            needs = %format!("agent:{handle}"),
            "no grant covers this participant: a capability naming \
             `agent:{handle}` (matched exactly, including its case) with write \
             on app:{} is what would let it open {}",
            req.app_id, app.display_name,
        );
        return Err(format!(
            "{handle} has not been given leave to open {} in this room — a \
             grant naming `agent:{handle}` exactly, with write on {}, is what \
             would cover it",
            app.display_name, req.app_id,
        ));
    }
    Ok(app.display_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle};
    use collab_types::caps::{CapOverride, Scope, Subject};
    use parking_lot::RwLock;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    /// The three grants a room can write about opening an application, and
    /// what each one actually permits.
    ///
    /// Asserted against a real `Workspace` and the capability model's own
    /// `decide`, not against a table written here — the claim being made is
    /// about the model, so a re-implementation of it would prove nothing.
    #[test]
    fn the_three_grants_say_three_different_things() {
        let photos = ResourceRef::App("dev.qos.photos".to_string());
        let files = ResourceRef::App("dev.qos.file-explorer".to_string());

        let bare = Workspace::new(WorkspaceId::new(), "opening");
        let actor = bare.actor_for_agent("Claude");
        assert!(
            !bare.allows(&actor, &photos, Verb::Write),
            "an agent holding nothing must not be able to open anything: it \
             carries no role, so there is no base authority underneath"
        );

        // "May open *this* application."
        let mut one = Workspace::new(WorkspaceId::new(), "opening");
        one.grant_capability(CapOverride::grant(
            Subject::agent("Claude"),
            Scope::App("dev.qos.photos".to_string()),
            Verb::Write,
        ));
        let actor = one.actor_for_agent("Claude");
        assert!(one.allows(&actor, &photos, Verb::Write));
        assert!(
            !one.allows(&actor, &files, Verb::Write),
            "a grant naming one application must not open the catalogue"
        );

        // "May open applications."
        let mut any = Workspace::new(WorkspaceId::new(), "opening");
        any.grant_capability(CapOverride::grant(
            Subject::agent("Claude"),
            Scope::Workspace,
            Verb::Write,
        ));
        let actor = any.actor_for_agent("Claude");
        assert!(any.allows(&actor, &photos, Verb::Write));
        assert!(any.allows(&actor, &files, Verb::Write));

        // "May do one thing inside an application that is already running."
        let mut act = Workspace::new(WorkspaceId::new(), "opening");
        act.grant_capability(CapOverride::grant(
            Subject::agent("Claude"),
            Scope::Action {
                app: "dev.qos.photos".to_string(),
                action: "crop".to_string(),
            },
            Verb::Write,
        ));
        let actor = act.actor_for_agent("Claude");
        assert!(
            !act.allows(&actor, &photos, Verb::Write),
            "leave to perform one operation in an app is not leave to start \
             the app — an agent handed a photograph to crop must not be able \
             to spawn a process with it"
        );
    }

    /// A connection that named nobody is refused before anything is looked
    /// up, and the sentence says which of the two things went wrong.
    #[test]
    fn an_unnamed_connection_is_refused_as_unnamed() {
        let chrome = chrome_rc();
        let req = OpenAppRequest {
            app_id: "dev.qos.photos".to_string(),
            with: None,
            room: "room".to_string(),
            who: "peer-1".to_string(),
            handle: None,
            origin: OpenOrigin::Connection(1),
        };
        let why = decide(&chrome, &req).expect_err("an unnamed asker must be refused");
        assert!(
            why.contains("who is asking"),
            "the refusal must name the thing that is missing, not the \
             permission it could not find: {why}"
        );
    }
}
