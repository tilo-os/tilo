//! Who sits at this device and which identity each workspace presents, kept
//! current by workers: the session's loop never waits on the disk to learn
//! either.
//!
//! The session reads each once at startup, then a worker reads it again
//! whenever a directory it lives in changes ([`tilo_files::native::Watched`]):
//!
//! - the person's public identity and photo ([`tilo_ui_base::local_identity`]), which
//!   the workspace sheet's people rows and the apps naming an author ask
//!   for; and
//! - the identity registry
//!   ([`tilo_identity::store::IdentityRegistryStore::load_reconciled_prompt`]),
//!   which every publish and frame asks which identity a workspace
//!   presents. A reconcile lists and parses every identity record, so it runs
//!   on the worker and the loop is handed the registry it produced.
//!
//! An offer about the person themselves is put to them here
//! ([`offer`]): asked from their own avatar, and performed when they answer
//! ([`answered`]).

use std::path::PathBuf;
use std::sync::Arc;

use tilo_files::native::Watched;
use tilo_session_core::desktop::state::{DesktopHandle, HostAction};
use tilo_ui_base::{
    local_identity::LocalIdentity, local_identity::LocalIdentitySource, local_identity::PhotoSource,
};

use crate::platform::notice::Notices;

/// Who sits at this device, as the loop asks for it.
#[derive(Clone)]
struct Present {
    /// Their public identity.
    identity: Option<LocalIdentity>,
    /// The public avatar bound to their root key: it is read from their identity record with the
    /// identity, on the worker, and an avatar a frame paints only clones pixels.
    photo: Option<([u8; 32], tilo_identity_client::ProfilePhoto)>,
}

/// [`Present`], kept current by a worker.
struct WatchedLocalIdentity(Watched<Present>);

/// A number that rises each time the person's identity record is read again, on the worker or by a
/// writer in this process. What the record holds that a reading caches — their name and their
/// picture — keys on it, so a write is seen at the next pass rather than at the next identity.
static RECORD_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`RECORD_GENERATION`], for a reading that caches what the identity record holds.
pub(crate) fn record_generation() -> u64 {
    RECORD_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

impl LocalIdentitySource for WatchedLocalIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        self.0.get().identity.clone()
    }

    fn refresh(&self) {
        self.0.refresh();
    }
}

impl PhotoSource for WatchedLocalIdentity {
    fn public_photo(&self) -> Option<([u8; 32], tilo_identity_client::ProfilePhoto)> {
        self.0.get().photo.clone()
    }
}

/// The directory the person's identity record lives in.
fn identity_directories() -> Vec<PathBuf> {
    tilo_identity::store::account_identity_dir_from_env()
        .ok()
        .into_iter()
        .collect()
}

/// The person's public identity and photo, read from their active record: this session hosts the
/// identity service, so it reads the directory rather than asking that service.
fn read_present() -> Present {
    let Some(directory) = tilo_identity::store::account_identity_dir_from_env().ok() else {
        return Present {
            identity: None,
            photo: None,
        };
    };
    let Some(record) = tilo_identity::store::resolve_active_identity(&directory) else {
        return Present {
            identity: None,
            photo: None,
        };
    };
    let Ok(mut metadata) = tilo_identity_client::load_metadata_from_path(&record) else {
        return Present {
            identity: None,
            photo: None,
        };
    };
    // The dev control provisions a marked primary record with a fixture label. It is still an
    // unnamed person in the shell until they give that identity a name.
    if tilo_identity::provisioning::is_throwaway_test_display_name(&record, &metadata.display_name)
    {
        metadata.display_name.clear();
    }
    Present {
        identity: Some(LocalIdentity {
            group_public_key: metadata.group_public_key,
            display_name: metadata.display_name,
            avatar_color: metadata.avatar_color,
            device_public_key: crate::sync::device_identity()
                .map(|identity| identity.device_cert.device_public_key),
        }),
        photo: metadata
            .profile_photo
            .map(|photo| (metadata.group_public_key, photo)),
    }
}

/// Who this device runs as, as the one id the session acts as, and the ids they went by before.
pub(crate) struct Own {
    /// The person, by the id the workspaces' governance names them by: their root key in hex, which
    /// this device's certificate carries — never the device's own key. A computer that has no
    /// identity yet goes by the login account at it, whom its session is theirs as until the person
    /// sets one up.
    pub(crate) person: String,
    /// The ids they went by before this device had an identity — the login account at it, and what
    /// the caller named — under which a session kept from then is still theirs.
    pub(crate) earlier: Vec<String>,
    /// The salt this device's ids are minted under, from its own key: what the workspace's
    /// governance derives to tell which device a window of the workspace runs on. `None` on a
    /// computer that has no identity yet.
    pub(crate) salt: Option<u16>,
    /// This device on the person's other devices ([`DeviceId`]), from its own key as the fabric
    /// names it: the id a notice addresses it by, so an alarm set for the phone rings there and
    /// not on the laptop. `None` on a computer that has no identity yet, and then every notice of
    /// the person's is shown here.
    pub(crate) device: Option<String>,
}

/// Who runs here, read from this device's own identity as the session starts,
/// which setup settles before the session opens.
pub(crate) fn own(options: &crate::Options) -> Own {
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
    let salt = identity.as_ref().map(|identity| {
        tilo_desktop_model::ids::device_salt_of_key(&identity.device_cert.device_public_key)
    });
    let device = identity.as_ref().and_then(|identity| {
        crate::platform::fabric::DeviceId::new(hex::encode(identity.device_cert.device_public_key))
            .ok()
            .map(|device| device.as_str().to_owned())
    });
    Own {
        person,
        earlier,
        salt,
        device,
    }
}

/// The person a session started with `options` runs as: what [`own`] reads, for what is made for
/// them before the session starts.
#[must_use]
pub fn own_person(options: &crate::Options) -> String {
    own(options).person
}

/// The ids a session started with `options` runs as: the person of [`own`]'s, and this device's
/// id on their other devices when the computer has an identity.
#[must_use]
pub fn own_ids(options: &crate::Options) -> (String, Option<String>) {
    let own = own(options);
    (own.person, own.device)
}

/// Read who sits here at startup, and keep it current from then on. A value that cannot be watched
/// is read where it is asked for, as before.
/// Called before the session builds what reads identity, so nothing is served the directory's state
/// from before the workers' first read: the first read is this one, on this thread, with the
/// watches already armed.
pub fn install() {
    match tilo_identity::store::IdentityRegistryStore::from_env() {
        Ok(store) => {
            if let Err(error) = store.load_reconciled_prompt() {
                tracing::warn!(%error, "cannot read the identity registry at startup");
            }
        }
        Err(error) => tracing::warn!(%error, "the identity registry is not at hand"),
    }
    match Watched::start("local-identity", identity_directories(), read_present) {
        Ok(watched) => {
            // Each read that lands moves the generation a cache of the record keys on.
            watched.on_change(|| {
                RECORD_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            });
            tilo_ui_base::local_identity::install_source(Some(Arc::new(WatchedLocalIdentity(
                watched,
            ))));
        }
        Err(error) => tracing::warn!(%error, "cannot watch the person's identity"),
    }
}

/// Who this device acts for in `workspace` : the identity its registry presents there, else the
/// person sitting at it.
pub(crate) fn person_in(workspace: &str, person: &str) -> String {
    tilo_identity::store::selected_actor_id_prompt(workspace)
        .ok()
        .flatten()
        .unwrap_or_else(|| person.to_owned())
}

/// Name this person's identity `name` : their active identity record takes it, so their own desktop
/// and the people they work with see it from now on. Says what stopped it when it could not.
pub(crate) fn rename_to(name: &str) -> Result<(), String> {
    let dir =
        tilo_identity::store::account_identity_dir_from_env().map_err(|error| error.to_string())?;
    tilo_identity::store::rename_active_identity(&dir, name).map_err(|error| error.to_string())?;
    // This process wrote the identity: it reads it now, rather than going on with the name it
    // replaced until the watch over the directory says so.
    tilo_ui_base::local_identity::refresh();
    Ok(())
}

/// The person's own picture as a `data:` URL a view draws, or empty when they chose none: their
/// identity record's photo, encoded once for each change of the identity.
pub(crate) fn own_photo_url() -> String {
    use image::ImageEncoder as _;
    static KEPT: std::sync::Mutex<(u64, String)> = std::sync::Mutex::new((u64::MAX, String::new()));
    let generation = record_generation();
    {
        let kept = KEPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if kept.0 == generation {
            return kept.1.clone();
        }
    }
    let url = tilo_ui_base::local_identity::own_public_photo()
        .and_then(|photo| {
            let mut bytes = Vec::new();
            image::codecs::png::PngEncoder::new(&mut bytes)
                .write_image(
                    photo.rgba(),
                    u32::from(photo.width()),
                    u32::from(photo.height()),
                    image::ExtendedColorType::Rgba8,
                )
                .ok()?;
            collab_types::photo_url(&bytes)
        })
        .unwrap_or_default();
    let mut kept = KEPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *kept = (generation, url.clone());
    url
}

/// Make the picture `bytes` encode this person's own, or take theirs away when none: their
/// active identity record keeps it, and every face that shows them reads it from there. Says
/// what stopped it when it could not.
pub(crate) fn set_photo(bytes: Option<&[u8]>) -> Result<(), String> {
    let dir =
        tilo_identity::store::account_identity_dir_from_env().map_err(|error| error.to_string())?;
    let active = tilo_identity::store::resolve_active_identity(&dir)
        .ok_or_else(|| "this device keeps no identity to give a picture".to_owned())?;
    let id = active
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| "the identity record has no id".to_owned())?
        .to_owned();
    let photo = bytes.map(bounded_picture).transpose()?;
    tilo_identity::store::update_identity_photo(&dir, &id, photo)
        .map_err(|error| error.to_string())?;
    // This process wrote the identity: it reads it now, rather than going on with the picture it
    // replaced until the watch over the directory says so.
    tilo_ui_base::local_identity::refresh();
    Ok(())
}

/// The picture `bytes` hold, at the size an identity record keeps: decoded upright and shrunk to
/// at most [`tilo_identity_client::MAX_PHOTO_DIMENSION`] pixels a side.
fn bounded_picture(bytes: &[u8]) -> Result<tilo_identity_client::ProfilePhoto, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| format!("the picture could not be read: {error}"))?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let limit = u32::from(tilo_identity_client::MAX_PHOTO_DIMENSION);
    let image = if width > limit || height > limit {
        let longest = width.max(height);
        let scaled = |side: u32| {
            u64::from(side)
                .saturating_mul(u64::from(limit))
                .checked_div(u64::from(longest))
                .and_then(|scaled| u32::try_from(scaled).ok())
                .unwrap_or(limit)
                .max(1)
        };
        image::imageops::resize(
            &image,
            scaled(width),
            scaled(height),
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image
    };
    let (width, height) = image.dimensions();
    let width = u16::try_from(width).map_err(|_| "the picture is too wide".to_owned())?;
    let height = u16::try_from(height).map_err(|_| "the picture is too tall".to_owned())?;
    tilo_identity_client::ProfilePhoto::new(width, height, image.into_raw()).map_err(str::to_owned)
}

/// Put a host offer to the person: one about who they are ([`HostAction::is_personal`], their name
/// and key here or another of their devices) is asked from their own avatar in the top bar, where
/// it waits until they answer it or put it off; every other offer is a banner over the windows.
/// This is the session's one API for such an offer, as a page's is `desktop_offer_host_action` in
/// a browser. The session has no question of its own about the person to ask through it yet: what
/// it knows of them is settled by setup before the session opens ([`crate::setup`]), so an ask about
/// them arrives from what the session hosts rather than from the session itself.
pub fn offer(
    handle: &DesktopHandle,
    notices: &dyn Notices,
    action: HostAction,
    title: impl Into<String>,
    body: impl Into<String>,
) {
    if action.is_personal() {
        handle.ask_personally(action, title.into(), body.into());
    } else {
        notices.show(&action.ask(title, body));
    }
}

/// The person answered an ask about themselves (the host action they chose,
/// such as keeping their name and key on this device or linking another of
/// their devices): their identity work for it.
///
/// Changing the person's identity record is setup's ([`crate::setup`]),
/// which asks its own questions before the session opens; so there is no work
/// behind an answer here yet, and the ask has already left their avatar.
/// The choice is named rather than dropped, so what the person answered is
/// never lost without trace.
pub(crate) fn answered(choice: &str) {
    tracing::info!(
        %choice,
        "an ask about the person was answered: their identity is kept by setup"
    );
}
