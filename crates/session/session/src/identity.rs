//! Who sits at this device and which identity each workspace presents, kept
//! current by workers: the session's loop never waits on the disk to learn
//! either.
//!
//! The session reads each once at startup, then a worker reads it again
//! whenever a directory it lives in changes ([`tilo_files::native::Watched`]):
//!
//! - the person's public identity and photo ([`ui::local_identity`]), which
//!   the workspace sheet's people rows and the apps naming an author ask
//!   for; and
//! - the identity registry
//!   ([`tilo_identity_host::registry::IdentityRegistryStore::load_reconciled_prompt`]),
//!   which every publish and frame asks which identity a workspace
//!   presents. A reconcile lists and parses every identity record, so it runs
//!   on the worker and the loop is handed the registry it produced.
//!
//! An offer about the person themselves is put to them here
//! ([`offer`]): asked from their own avatar, and performed when they answer
//! ([`answered`]).

use std::path::PathBuf;
use std::sync::Arc;

use tilo_desktop_surface::{DesktopHandle, DesktopNotifications, HostAction};
use tilo_files::native::Watched;
use ui::local_identity::{LocalIdentity, LocalIdentitySource, RuntimeIdentity};

/// Who sits at this device, as the loop asks for it.
#[derive(Clone)]
struct Present {
    /// Their public identity.
    identity: Option<LocalIdentity>,
    /// The public avatar bound to their root key, as [`RuntimeIdentity`] reads it: the file it
    /// lives in is read with the identity, on the worker, and an avatar a frame paints only clones
    /// pixels.
    photo: Option<([u8; 32], runtime_identity::ProfilePhoto)>,
}

/// [`Present`], kept current by a worker.
struct WatchedLocalIdentity(Watched<Present>);

impl LocalIdentitySource for WatchedLocalIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        self.0.get().identity.clone()
    }

    fn public_photo(&self) -> Option<([u8; 32], runtime_identity::ProfilePhoto)> {
        self.0.get().photo.clone()
    }
}

/// The directories the person's identity record can live in: the identity directory, and the
/// directory of the record the launcher bound.
fn identity_directories() -> Vec<PathBuf> {
    runtime_identity::identity_dir_from_env()
        .ok()
        .into_iter()
        .chain(
            std::env::var_os(runtime_identity::IDENTITY_ENV_VAR)
                .map(PathBuf::from)
                .and_then(|record| record.parent().map(std::path::Path::to_path_buf)),
        )
        .collect()
}

/// Who sits at this device, as the one id the session acts as, and the ids they went by before.
pub(crate) struct Seat {
    /// The person, by the id the workspaces' governance names them by: their root key in hex, which
    /// this device's certificate carries — never the device's own key. A computer that has no
    /// identity yet goes by the login account at it, whom its desk is theirs as until the person
    /// sets one up.
    pub(crate) person: String,
    /// The ids they went by before this device had an identity — the login account at it, and what
    /// the caller named — under which a desk kept from then is still theirs.
    pub(crate) earlier: Vec<String>,
    /// The salt this device's ids are minted under, from its own key: what the workspace's
    /// governance derives to tell which device a window of the workspace runs on. `None` on a
    /// computer that has no identity yet.
    pub(crate) salt: Option<u16>,
}

/// Who sits here, read from this device's own identity as the session starts,
/// which setup settles before the desk opens.
pub(crate) fn seat(options: &crate::Options) -> Seat {
    let identity = crate::sync::device_identity();
    let person = match &identity {
        Some(identity) => hex::encode(identity.device_cert.user_public_key),
        None if !options.user.is_empty() => options.user.clone(),
        None => options.person.clone(),
    };
    let earlier = [options.user.clone(), options.person.clone()]
        .into_iter()
        .filter(|id| !id.is_empty() && *id != person)
        .collect();
    let salt = identity.map(|identity| {
        tilo_desktop_model::ids::device_salt_of_key(&identity.device_cert.device_public_key)
    });
    Seat {
        person,
        earlier,
        salt,
    }
}

/// The person a session started with `options` sits as: what [`seat`] reads, for what is made for
/// them before the session starts.
#[must_use]
pub fn seated_person(options: &crate::Options) -> String {
    seat(options).person
}

/// Read who sits here at startup, and keep it current from then on. A value that cannot be watched
/// is read where it is asked for, as before.
/// Called before the session builds what reads identity, so nothing is served the directory's state
/// from before the workers' first read: the first read is this one, on this thread, with the
/// watches already armed.
pub fn install() {
    match tilo_identity_host::registry::IdentityRegistryStore::from_env() {
        Ok(store) => {
            if let Err(error) = store.load_reconciled_prompt() {
                tracing::warn!(%error, "cannot read the identity registry at startup");
            }
        }
        Err(error) => tracing::warn!(%error, "the identity registry is not at hand"),
    }
    match Watched::start("local-identity", identity_directories(), || Present {
        identity: RuntimeIdentity.load(),
        photo: RuntimeIdentity.public_photo(),
    }) {
        Ok(watched) => {
            ui::local_identity::install_source(Some(Arc::new(WatchedLocalIdentity(watched))));
        }
        Err(error) => tracing::warn!(%error, "cannot watch the person's identity"),
    }
}

/// Who this device acts for in `workspace` : the identity its registry presents there, else the
/// person sitting at it.
pub(crate) fn person_in(workspace: &str, person: &str) -> String {
    tilo_identity_host::registry::selected_principal_id_prompt(workspace)
        .ok()
        .flatten()
        .unwrap_or_else(|| person.to_owned())
}

/// Name this person's identity `name` : their active identity record takes it, so their own desktop
/// and the people they work with see it from now on. Says what stopped it when it could not.
pub(crate) fn rename_to(name: &str) -> Result<(), String> {
    let dir = runtime_identity::identity_dir_from_env().map_err(|error| error.to_string())?;
    let record = runtime_identity::resolve_active_identity(&dir)
        .ok_or_else(|| "no active identity".to_owned())?;
    let id = record
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| "identity record without an id".to_owned())?
        .to_owned();
    let colour = runtime_identity::load_metadata_from_path(&record)
        .map(|metadata| metadata.avatar_color)
        .map_err(|error| error.to_string())?;
    runtime_identity::update_identity_profile(&dir, &id, name, colour)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Put a host offer to the person: one about who they are ([`HostAction::is_personal`], their name
/// and key here or another of their devices) is asked from their own avatar in the top bar, where
/// it waits until they answer it or put it off; every other offer is a banner over the windows.
/// This is the session's one door for such an offer, as a page's is `desktop_offer_host_action` in
/// a browser. The session has no question of its own about the person to ask through it yet: what
/// it knows of them is settled by setup before the desk opens ([`crate::setup`]), so an ask about
/// them arrives from what the session hosts rather than from the session itself.
pub fn offer(
    handle: &DesktopHandle,
    notices: &DesktopNotifications,
    action: HostAction,
    title: impl Into<String>,
    body: impl Into<String>,
) {
    if action.is_personal() {
        handle.ask_personally(action, title.into(), body.into());
    } else {
        notices.post(action.notice(title, body));
    }
}

/// The person answered an ask about themselves (the host action they chose,
/// such as keeping their name and key on this device or linking another of
/// their devices): their identity work for it.
///
/// Changing the person's identity record is setup's ([`crate::setup`]),
/// which asks its own questions before the desk opens; so there is no work
/// behind an answer here yet, and the ask has already left their avatar.
/// The choice is named rather than dropped, so what the person answered is
/// never lost without trace.
pub(crate) fn answered(choice: &str) {
    tracing::info!(
        %choice,
        "an ask about the person was answered: their identity is kept by setup"
    );
}
