//! What each device of the person holds of their list of workspaces.
//!
//! The person's list of workspaces is a resource of their own workspace
//! (`identity.workspaces`, a [`WorkspaceList`]). Every device of theirs enters
//! each workspace the list shows on it, as a member copy at the address the
//! list keeps for it ([`crate::workspaces::Workspaces::hold_as_members`]): the
//! device's daemon does, every device of theirs holding any copy of their own
//! workspace, and the desktop session shows what it holds: the only way a
//! device takes in a workspace of theirs, the one they just joined on it too
//! ([`Workspaces::take_in_joined`]). One that goes off the list, or that its
//! members name them in only as removed, every device lets go of. A server
//! with an entry among their servers ([`super::SERVERS`]) keeps each workspace
//! the list names instead, unless its entry turned keeping off for it
//! ([`Server::keeps_not`]). One its entry no longer keeps it lets go of
//! ([`Workspaces::release`]); its files stay, and keeping it again takes it up
//! from them.
//!
//! On every device of the person, a workspace held as the device the machine
//! was copied from is entered again as this device at the device serving it
//! ([`Workspaces::held_as_another_device`]): the daemon does that, never the
//! desktop session.
//!
//! It learns of a change from the change itself: a write to the list or to
//! the servers' document, or a workspace reaching this device
//! ([`Workspaces::follow_person_list`]). While a workspace the list shows here
//! is not taken in, it tries again, sooner first and then about once a minute,
//! and says which and why ([`Workspaces::not_here`]); with none missing,
//! nothing reads on a timer.
//!
//! What it keeps is each workspace's to say: `copies.read` (`…/servers#keeps`)
//! names the devices keeping it.

use std::sync::Arc;
use std::time::Duration;

use tilo_peer_state::join::TransportHint;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::IDENTITY_LIST;
use tilo_user_identity::WorkspaceList;

use super::{SERVERS, person_workspace_id, read};
use crate::workspaces::person_list::{KeptAt, listing, showing, unlisting};
use crate::workspaces::person_workspace;
use crate::workspaces::{HostedWorkspace, RELEASED, Workspaces};

/// What one pass over the person's list did, by workspace id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// The workspaces this server keeps from now on.
    pub entered: Vec<String>,
    /// The workspaces it let go of.
    pub released: Vec<String>,
    /// The workspaces the list shows here that this device could not take in, and why.
    pub missing: Vec<NotHere>,
}

/// A workspace of the person's list that shows on this device and that it holds no copy of yet,
/// with why: what the person's list of workspaces on this device says of it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotHere {
    /// The workspace's id.
    pub workspace: String,
    /// The name the person's list gives it.
    pub name: String,
    /// Why this device does not hold it yet, as the device a person reads it on says it.
    pub why: String,
}

/// How long the first attempt again waits after a workspace the list shows here was not taken
/// in; each next waits twice as long, up to [`LAST_RETRY`].
const FIRST_RETRY: Duration = Duration::from_secs(2);
/// The longest a workspace not taken in waits for the next attempt.
const LAST_RETRY: Duration = Duration::from_secs(60);
/// How long a join waits for this device's follower of the person's list to hold what it joined
/// ([`Workspaces::take_in_joined`]) before it answers why it is not here yet.
pub const JOINED_WITHIN: Duration = Duration::from_secs(30);
/// What a join answers when what it joined is not held here in time: the person is in it, and
/// this device goes on taking it in.
pub const NOT_HERE_YET: &str = "you're in it, but it isn't on this device yet";

impl Kept {
    /// Whether the pass changed nothing.
    #[must_use]
    pub fn none(&self) -> bool {
        self.entered.is_empty() && self.released.is_empty()
    }
}

/// Enter every workspace of this device's person's list that shows on this
/// device, or, on a server of theirs, that its entry among their servers keeps,
/// letting go of each that entry keeps no longer: one pass, on any device that
/// holds a copy of the person's own workspace, kept or a member's. Every device
/// first enters as itself what it holds as the device it was copied from. What
/// it could not enter it answers in [`Kept::missing`] and says in
/// [`Workspaces::not_here`].
///
/// # Errors
/// The servers' document or the list could not be read, or a workspace could
/// not be let go of or taken up again from its files.
pub async fn keep_listed(workspaces: &Workspaces) -> Result<Kept, String> {
    let own = workspaces.device_cert().clone();
    let person = person_workspace_id(&own.user_public_key);
    // What this device holds as the device it was copied from it enters as itself, at the device
    // serving it: every device of the person's, listed or not, and never its desktop session.
    let mut kept = enter_as_this_device(workspaces).await;
    if workspaces.state(&person).await.is_none() {
        tracing::debug!("this device holds no copy of its person's own workspace: no list to keep");
        workspaces.say_not_here(Vec::new());
        return Ok(kept);
    }
    // A server keeps what its entry keeps; every other device what the list shows on it.
    let entry = read(workspaces)
        .await?
        .active(&own.device_public_key)
        .cloned();
    for workspace in entry.iter().flat_map(|entry| &entry.keeps_not) {
        if *workspace != person && workspaces.state(workspace).await.is_some() {
            workspaces.release(workspace).await?;
            kept.released.push(workspace.clone());
        }
    }
    let mut entering = Vec::new();
    let mut names = std::collections::BTreeMap::new();
    let (listing, went) = listed(workspaces, &own.user_public_key, &own.device_public_key).await?;
    // A workspace the person left goes from every device of theirs, and one kept off this device
    // from this one; one this device keeps itself is no member copy, and stays: it serves it.
    let off_here = listing
        .iter()
        .filter(|(.., shown)| entry.is_none() && !shown);
    for id in went
        .into_iter()
        .chain(off_here.map(|(workspace, ..)| workspace.to_string()))
    {
        if let_go(workspaces, &id).await {
            kept.released.push(id);
        }
    }
    // One the person was taken out of goes from their list as one they left does: the write
    // brings the pass that lets it go everywhere.
    let mut out = Vec::new();
    for (workspace, ..) in &listing {
        if taken_out(workspaces, &workspace.to_string(), &own.user_public_key).await {
            out.push(unlisting(&workspace.to_string()));
        }
    }
    if !out.is_empty() {
        person_workspace::apply_actions_on(workspaces, &person, IDENTITY_LIST, out, None).await?;
    }
    for (workspace, name, hint, shown) in listing {
        let id = workspace.to_string();
        let keeps = entry.as_ref().map_or(shown, |entry| entry.keeps(&id));
        if id == person || !keeps || workspaces.state(&id).await.is_some() {
            continue;
        }
        if keep_again(workspaces, &id).await? {
            kept.entered.push(id);
            continue;
        }
        match hint {
            Some(hint) => {
                names.insert(workspace, name);
                entering.push((workspace, hint));
            }
            // The device that made it says where it is reached once it holds it: that write
            // to the list brings another pass.
            None => kept.missing.push(NotHere {
                workspace: id,
                name,
                why: "none of your devices has said yet where it is kept".to_owned(),
            }),
        }
    }
    let failed = workspaces.hold_as_members(entering.iter().cloned()).await;
    for (workspace, _) in entering {
        let id = workspace.to_string();
        if workspaces.state(&id).await.is_some() {
            kept.entered.push(id);
        } else {
            let why = failed
                .iter()
                .find(|(failed, _)| *failed == workspace)
                .map_or_else(
                    || "its copy did not reach this device".to_owned(),
                    |(_, why)| why.clone(),
                );
            kept.missing.push(NotHere {
                workspace: id,
                name: names.remove(&workspace).unwrap_or_default(),
                why,
            });
        }
    }
    workspaces.say_not_here(kept.missing.clone());
    Ok(kept)
}

/// Enter as this device each workspace held here as the device this machine was copied from
/// ([`Workspaces::held_as_another_device`]), and say which it holds as itself now.
async fn enter_as_this_device(workspaces: &Workspaces) -> Kept {
    let entering = workspaces.held_as_another_device().await;
    let ids: Vec<String> = entering.iter().map(|(id, _)| id.to_string()).collect();
    workspaces.hold_as_members(entering).await;
    let mut kept = Kept::default();
    for id in ids {
        if workspaces.holds_as_this_device(&id).await {
            kept.entered.push(id);
        }
    }
    kept
}

/// Whether `person` was taken out of `workspace`, as the member copy held here says: its members
/// name them, and only as removed.
async fn taken_out(workspaces: &Workspaces, workspace: &str, person: &[u8; 32]) -> bool {
    if !workspaces.is_member(workspace).await.unwrap_or(false) {
        return false;
    }
    let Ok(state) = workspaces.workspace(workspace).await else {
        return false;
    };
    let members = state
        .lock()
        .await
        .peer()
        .governance()
        .member_list(tilo_clock::wall());
    members.is_ok_and(|list| {
        let mut theirs = list
            .members
            .iter()
            .filter(|member| member.root() == Some(*person));
        theirs.clone().next().is_some() && theirs.all(|member| member.removed)
    })
}

/// Let go of the member copy of `workspace` held here, when there is one: whether it went. Its
/// files stay, as every copy let go of keeps them.
async fn let_go(workspaces: &Workspaces, workspace: &str) -> bool {
    workspaces.is_member(workspace).await.unwrap_or(false)
        && workspaces
            .release(workspace)
            .await
            .inspect_err(|error| tracing::warn!(%error, %workspace, "a workspace off the person's list here is not let go of yet"))
            .is_ok()
}

/// A workspace of the person's list: its id, its name, the address the list keeps for it (`None`
/// for one no device of theirs said where it is kept) and whether it shows on this device.
type Listed = (WorkspaceId, String, Option<TransportHint>, bool);

/// Each workspace of `person`'s list, as [`Listed`] says it for `device`, and each the list says
/// went.
async fn listed(
    workspaces: &Workspaces,
    person: &[u8; 32],
    device: &[u8; 32],
) -> Result<(Vec<Listed>, Vec<String>), String> {
    let list = person_workspace::read_on::<WorkspaceList>(
        workspaces,
        &person_workspace_id(person),
        IDENTITY_LIST,
    )
    .await?;
    let went = list
        .left
        .keys()
        .filter(|id| WorkspaceId::parse(id).is_some_and(|id| list.went(id)))
        .cloned()
        .collect();
    let listing = list
        .listed()
        .filter_map(|(id, listed)| {
            let workspace = WorkspaceId::parse(&id.to_string())?;
            let hint = listed.address().and_then(|bytes| {
                tilo_failure::ignored!(
                    "an address the list keeps that does not read names none",
                    postcard::from_bytes::<TransportHint>(&bytes).ok()
                )
            });
            Some((
                workspace,
                listed.name.clone(),
                hint,
                listed.shows_on(device),
            ))
        })
        .collect();
    Ok((listing, went))
}

/// Take up again the copy of `workspace` this device let go of, from the files
/// it kept, when the workspace still names this device among the ones keeping
/// it: what it had then is where it stands, and the other keepers bring what
/// it missed. A member copy is taken up through the device serving it instead,
/// and a workspace with no files here is entered.
async fn keep_again(workspaces: &Workspaces, workspace: &str) -> Result<bool, String> {
    let path = workspaces.inner.dir.join(format!("{workspace}.workspace"));
    let released = path.with_extension(RELEASED);
    if !released.exists() {
        return Ok(false);
    }
    let hosted = HostedWorkspace::open(path, workspaces.device_keys().as_ref())?;
    let own = workspaces.device_cert().device_public_key;
    if hosted.member.is_some()
        || !hosted
            .peer()
            .governance()
            .keeping_devices()
            .iter()
            .any(|hint| hint.host.device_public_key == own)
    {
        return Ok(false);
    }
    std::fs::remove_file(&released).map_err(|error| error.to_string())?;
    workspaces.serve_direct(&hosted).await?;
    let state = workspaces.read_held(workspace, hosted);
    workspaces.keep_held(workspace, &state).await;
    workspaces.serve_kept(std::slice::from_ref(&state)).await?;
    workspaces.keep_reading(&state).await;
    let _ = workspaces.inner.changes.send(workspace.to_owned());
    tracing::info!(%workspace, "a workspace let go of here is kept again");
    Ok(true)
}

impl Workspaces {
    /// The workspaces the person's list shows on this device that it could not take in yet, each
    /// with why, as the last pass over the list found them ([`keep_listed`]).
    #[must_use]
    pub fn not_here(&self) -> Vec<NotHere> {
        self.inner.not_here.borrow().clone()
    }

    /// Moves whenever [`Self::not_here`] says something else.
    #[must_use]
    pub fn not_here_changed(&self) -> tokio::sync::watch::Receiver<Vec<NotHere>> {
        self.inner.not_here.subscribe()
    }

    /// Whether this device takes `workspace` in by its person's list: it holds a copy of their
    /// own workspace, which holds the list, and `workspace` is another. A device with no copy of
    /// it (one confined to a workspace, or one that has not taken it in yet), and their own
    /// workspace itself, which the list never names, it takes in itself.
    pub async fn takes_in_by_list(&self, workspace: &str) -> bool {
        let cert = self.device_cert();
        let own = person_workspace_id(&cert.user_public_key);
        super::super::member::holds_copies(cert).is_ok()
            && workspace != own
            && self.state(&own).await.is_some()
    }

    /// Have `workspace`, which this device's person just joined at the device `hint` names, on
    /// every device of theirs: listed with where it is kept, so each device's follower of the
    /// list takes it in, this one's included. Answered once it is held here, within
    /// [`JOINED_WITHIN`], and listed under its own name then.
    ///
    /// # Errors
    /// The list could not be written, or this device did not hold it in time: why it is not here
    /// yet ([`Self::held_within`]). It goes on trying.
    pub async fn take_in_joined(
        &self,
        workspace: WorkspaceId,
        hint: &TransportHint,
    ) -> Result<WorkspaceId, String> {
        let (id, own) = (workspace.to_string(), self.own_list());
        let list = person_workspace::read_on::<WorkspaceList>(self, &own, IDENTITY_LIST).await?;
        // One listed already keeps its name until the workspace's own is read.
        let name = list.get(workspace).map(|listed| listed.name.clone());
        let kept = KeptAt {
            device: hint.host.device_public_key,
            hint: Some(hint.clone()),
        };
        let listing = listing(&id, name.as_deref().unwrap_or_default(), Some(&kept));
        person_workspace::apply_actions_on(self, &own, IDENTITY_LIST, listing, None).await?;
        self.held_within(&id, JOINED_WITHIN).await?;
        let name = self
            .state(&id)
            .await
            .and_then(|held| held.governance().workspace_name());
        if let Some(name) = name {
            let named = WorkspaceList::LIST_WORKSPACE.with((id, name));
            if let Err(error) =
                person_workspace::apply_actions_on(self, &own, IDENTITY_LIST, vec![named], None)
                    .await
            {
                tracing::warn!(%error, %workspace, "a joined workspace's name was not listed");
            }
        }
        Ok(workspace)
    }

    /// Wait until `workspace` is held here, `within` at most, and no longer than until the
    /// follower of the person's list says why it could not take it in: the person hears that
    /// at once, while the follower goes on trying.
    ///
    /// # Errors
    /// It was not held: why, as the follower says it ([`Self::not_here`]).
    pub async fn held_within(&self, workspace: &str, within: Duration) -> Result<(), String> {
        let mut holds = self.holds_changed();
        let mut said = self.not_here_changed();
        let why = |said: &[NotHere]| {
            said.iter()
                .find(|missing| missing.workspace == workspace)
                .map(|missing| missing.why.clone())
        };
        let answered = tokio::time::timeout(within, async {
            loop {
                if self.state(workspace).await.is_some() {
                    return None;
                }
                let missing = why(&said.borrow_and_update());
                if missing.is_some() {
                    return missing;
                }
                tokio::select! {
                    moved = holds.changed() => if moved.is_err() { std::future::pending::<()>().await },
                    moved = said.changed() => if moved.is_err() { std::future::pending::<()>().await },
                }
            }
        })
        .await;
        match answered {
            Ok(None) => Ok(()),
            Ok(Some(why)) => Err(format!("{NOT_HERE_YET} ({why}); this device keeps trying")),
            Err(_) => Err(format!("{NOT_HERE_YET}; this device keeps trying")),
        }
    }

    /// Show `workspace`, which the person's list names, on this device again: its follower of the
    /// list takes it in.
    ///
    /// # Errors
    /// This device follows no list of the person's, or the list does not name it or could not be
    /// written.
    pub async fn show_here(&self, workspace: &str) -> Result<(), String> {
        if !self.takes_in_by_list(workspace).await {
            return Err(
                "this device doesn't hold your own workspace yet, whose list it keeps \
                        your workspaces by"
                    .to_owned(),
            );
        }
        let device = self.device_cert().device_public_key;
        let shown = showing(workspace, device, true);
        person_workspace::apply_actions_on(self, &self.own_list(), IDENTITY_LIST, vec![shown], None)
            .await
    }

    /// Take `workspace` off the person's list, where this device follows it: they left it, and
    /// every device of theirs lets it go.
    ///
    /// # Errors
    /// The list could not be written.
    pub async fn leave_listed(&self, workspace: &str) -> Result<(), String> {
        if !self.takes_in_by_list(workspace).await {
            return Ok(());
        }
        let gone = unlisting(workspace);
        person_workspace::apply_actions_on(self, &self.own_list(), IDENTITY_LIST, vec![gone], None)
            .await
    }

    /// The person's own workspace, which holds their list.
    fn own_list(&self) -> String {
        person_workspace_id(&self.device_cert().user_public_key)
    }

    fn say_not_here(&self, missing: Vec<NotHere>) {
        self.inner.not_here.send_if_modified(|said| {
            let changed = *said != missing;
            *said = missing;
            changed
        });
    }

    /// Keep the person's list as this device's entry among their servers
    /// says, now and again after each change to the list, to the servers'
    /// document, or to what this device holds; and, while a workspace the list
    /// shows here is not taken in, again after a wait that grows from
    /// [`FIRST_RETRY`] to [`LAST_RETRY`].
    pub(crate) fn follow_person_list(&self) {
        let mut writes = self.document_writes();
        let mut governance = self.governance_changes();
        let mut holds = self.holds_changed();
        let workspaces = Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let person = person_workspace_id(&self.device_cert().user_public_key);
        runtime.spawn(async move {
            let mut wait = FIRST_RETRY;
            loop {
                let unsettled = {
                    let Some(inner) = workspaces.upgrade() else {
                        return;
                    };
                    match Box::pin(keep_listed(&Self { inner })).await {
                        Ok(kept) => {
                            if !kept.none() {
                                tracing::info!(?kept, "this device keeps the person's list");
                            }
                            !kept.missing.is_empty()
                        }
                        // What it could not read it reads again, as it takes in what it missed.
                        Err(error) => {
                            tracing::warn!(%error, "this device's keeping of the person's list was not applied");
                            true
                        }
                    }
                };
                let again = if unsettled {
                    let again = wait;
                    wait = wait.saturating_mul(2).min(LAST_RETRY);
                    Some(again)
                } else {
                    wait = FIRST_RETRY;
                    None
                };
                let retry = after(again);
                tokio::pin!(retry);
                loop {
                    use tokio::sync::broadcast::error::RecvError;
                    let changed = tokio::select! {
                        write = writes.recv() => match write {
                            // A member copy names what it took in by the store's documents,
                            // not by the record: any write of the list's app or the servers'.
                            Ok(write) => {
                                write.workspace == person
                                    && (write.app == IDENTITY_LIST.0 || write.app == SERVERS.0)
                            }
                            Err(RecvError::Lagged(_)) => true,
                            Err(RecvError::Closed) => return,
                        },
                        change = governance.recv() => match change {
                            Ok(workspace) => workspace == person,
                            Err(RecvError::Lagged(_)) => true,
                            Err(RecvError::Closed) => return,
                        },
                        held = holds.changed() => {
                            if held.is_err() {
                                return;
                            }
                            true
                        }
                        () = &mut retry => break,
                    };
                    if changed {
                        wait = FIRST_RETRY;
                        break;
                    }
                }
            }
        });
    }
}

/// Answered `wait` from now, or never for `None`.
async fn after(wait: Option<Duration>) {
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}
