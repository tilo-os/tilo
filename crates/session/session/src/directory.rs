//! The public workspaces the sites this computer knows list: what "Join a
//! workspace…" shows of other people's workspaces, where each one can be
//! joined, and what became of the joins asked for.
//!
//! A site names its home workspace and the directory of its public ones in
//! its `entry.json`; the directory then lists every public workspace it
//! serves with what it is, who is in it now and how lively it is
//! (`tilo_relay_protocol::directory`). The community site this computer is
//! configured with is read first, and a site the person types is read too.
//!
//! Joining one asks its door to admit this person
//! ([`tilo_relay_protocol::directory::DoorAnswer`]) and verifies the
//! invitation the door answers with for them: the room that invitation
//! grants, and nothing the directory claims, is the workspace joined
//! ([`join`]).
//!
//! Every read and every join happens on a worker of its own, never on the
//! session's loop ([`Store::ask`], [`Store::join`] and [`Store::answered`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;
use tilo_desktop_surface::DesktopHandle;
use tilo_relay_protocol::directory::{Activity, DirectoryEntry, DirectoryPage, DoorAnswer};
use tilo_relay_protocol::rooms::RoomRole;
use tilo_sheets::directory::{Directory, Listing, ListingAction, Status};

/// How long establishing a connection to a site may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a site's entry or its directory may take to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest `entry.json` read.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The largest directory read: a few hundred listings with their faces.
const MAX_DIRECTORY_BYTES: usize = 1024 * 1024;
/// The vendor's community site.
const VENDOR_CONFIG: &str = "/usr/share/tilo/community.toml";
/// The administrator's, which wins.
const SYSTEM_CONFIG: &str = "/etc/tilo/community.toml";
/// Overrides both paths, for a test or a development session.
pub(crate) const CONFIG_ENV: &str = "TILO_COMMUNITY_CONFIG";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunityToml {
    /// The site whose home workspace a computer is in by default; empty for
    /// none.
    #[serde(default)]
    site: String,
}

/// The community site configured for this computer, if any.
pub(crate) fn community_site() -> Option<url::Url> {
    let paths: Vec<PathBuf> = match std::env::var_os(CONFIG_ENV) {
        Some(path) => vec![PathBuf::from(path)],
        None => vec![PathBuf::from(SYSTEM_CONFIG), PathBuf::from(VENDOR_CONFIG)],
    };
    site_from(paths.iter().map(PathBuf::as_path))
}

/// The site the first readable file of `paths` names. A file that does not
/// parse is reported and skipped.
fn site_from<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Option<url::Url> {
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let config: CommunityToml = match toml::from_str(&text) {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "community config does not parse; skipping it");
                continue;
            }
        };
        if config.site.trim().is_empty() {
            return None;
        }
        return match site_entry(&config.site) {
            Ok(site) => Some(site),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "community site is not a web address");
                None
            }
        };
    }
    None
}

/// An origin a site or host is reached at: HTTPS, or plain HTTP on this
/// machine's loopback only (a host under development).
fn web_origin(text: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(text).map_err(|_| "that is not a web address")?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err("that is not a web address".into()),
    }
    if url.cannot_be_a_base() || url.host_str().is_none() {
        return Err("that is not a web address".into());
    }
    Ok(url)
}

/// The entry a person typed: a site's address, with or without its scheme
/// and path. The route of a link is not part of it.
fn site_entry(text: &str) -> Result<url::Url, String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err("that is not a web address".into());
    }
    let with_scheme = if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    };
    let mut entry = web_origin(&with_scheme)?;
    entry.set_fragment(None);
    entry.set_query(None);
    // `entry.json` sits beside the entry page: a path names a directory.
    if !entry.path().ends_with('/') {
        let path = format!("{}/", entry.path());
        entry.set_path(&path);
    }
    Ok(entry)
}

/// One public workspace a directory lists, with where it is joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    /// What its directory says about it.
    pub(crate) entry: DirectoryEntry,
    /// Its door, resolved; `None` for a site's home workspace that lets
    /// people in by invitation only.
    pub(crate) door: Option<url::Url>,
    /// The site whose own workspace it is: joining it joins that site's
    /// home.
    pub(crate) site: Option<url::Url>,
    /// The host name it is listed by ("tilo.coffee").
    pub(crate) host: String,
}

impl Listed {
    /// The key it is joined and found again by: its room, else the site
    /// whose home it is, else the name it is listed under.
    pub(crate) fn key(&self) -> String {
        if self.entry.room.is_empty() {
            self.site
                .as_ref()
                .map_or_else(|| self.entry.name.clone(), url::Url::to_string)
        } else {
            self.entry.room.clone()
        }
    }

    /// How lively it is at `now_ms`.
    fn activity(&self, now_ms: u64) -> Activity {
        self.entry.activity(now_ms)
    }
}

#[derive(Deserialize)]
struct EntryJson {
    home: Option<HomeJson>,
    /// The site's directory of public workspaces.
    directory: Option<String>,
}

#[derive(Deserialize)]
struct HomeJson {
    #[serde(default)]
    name: String,
    host: HostJson,
    admission: Option<String>,
}

#[derive(Deserialize)]
struct HostJson {
    origin: String,
}

/// A site's home workspace, as its `entry.json` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SiteWorkspace {
    /// The site's entry: where its links point.
    entry: url::Url,
    name: String,
    /// The host's admission door, when it admits people without an
    /// invitation.
    admission: Option<url::Url>,
}

/// What a site's `entry.json` says: its home workspace and its directory.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SiteEntry {
    home: Option<SiteWorkspace>,
    directory: Option<url::Url>,
}

/// The listings of `page`, read from the directory at `directory`. `site`
/// is the site the directory belongs to, when it is a site's: its community
/// listing joins that site's home.
fn listings_of(page: DirectoryPage, directory: &url::Url, site: Option<&url::Url>) -> Vec<Listed> {
    let host = directory.host_str().unwrap_or_default().to_owned();
    page.workspaces
        .into_iter()
        .filter(|entry| !entry.name.trim().is_empty())
        .map(|entry| Listed {
            door: directory
                .join(&entry.door)
                .ok()
                .and_then(|door| web_origin(door.as_str()).ok()),
            site: site.filter(|_| entry.community).cloned(),
            host: host.clone(),
            entry,
        })
        .collect()
}

/// The listing of a site's home workspace that its directory does not list
/// (or that names no directory): what its `entry.json` says.
fn home_listing(site: &url::Url, home: &SiteWorkspace) -> Listed {
    Listed {
        entry: DirectoryEntry {
            room: String::new(),
            name: home.name.clone(),
            description: String::new(),
            default_role: RoomRole::Editor,
            door: home
                .admission
                .as_ref()
                .map(url::Url::to_string)
                .unwrap_or_default(),
            here: Vec::new(),
            here_count: 0,
            members: 0,
            last_active_ms: None,
            community: true,
        },
        door: home.admission.clone(),
        site: Some(site.clone()),
        host: site.host_str().unwrap_or_default().to_owned(),
    }
}

/// The listings in the order the sheet shows them: the community's own
/// first, then the liveliest (most people in it now, then most recently
/// active), then by name. A workspace two directories list is shown once,
/// as the first one listed it.
fn ordered(listings: Vec<Listed>, now_ms: u64) -> Vec<Listed> {
    let mut seen = BTreeSet::new();
    let mut kept: Vec<_> = listings
        .into_iter()
        .filter(|listing| {
            if listing.entry.room.is_empty() {
                listing
                    .site
                    .as_ref()
                    .is_none_or(|site| seen.insert(site.to_string()))
            } else {
                seen.insert(listing.entry.room.clone())
            }
        })
        .collect();
    let liveliness = |listing: &Listed| match listing.activity(now_ms) {
        Activity::HereNow(here) => (u64::from(here), u64::MAX),
        _ => (0, listing.entry.last_active_ms.unwrap_or(0)),
    };
    kept.sort_by(|a, b| {
        b.entry
            .community
            .cmp(&a.entry.community)
            .then_with(|| liveliness(b).cmp(&liveliness(a)))
            .then_with(|| {
                a.entry
                    .name
                    .to_lowercase()
                    .cmp(&b.entry.name.to_lowercase())
            })
    });
    kept
}

/// What a site answered when its directory or its entry was read.
struct Snapshot {
    listings: Vec<Listed>,
    failures: Vec<String>,
}

/// The person a join is made as: the key a door admits, and the name a room
/// learns of them.
pub(crate) struct Person {
    /// The person's root key, which this device is certified as.
    user: [u8; 32],
    /// The name they go by, never their device's.
    name: String,
}

impl Person {
    /// This computer's person: the one its device certificate names.
    ///
    /// # Errors
    /// A computer with no identity of its own joins nowhere as anybody.
    pub(crate) fn this_computer() -> Result<Self, String> {
        let identity = crate::sync::device_identity().ok_or_else(|| {
            "this computer has no identity yet, so it cannot join a workspace".to_owned()
        })?;
        Ok(Self {
            user: identity.device_cert.user_public_key,
            name: own_person_name(ui::local_identity::load().map(|identity| identity.display_name)),
        })
    }
}

/// The name this person goes by in a room: the one they chose, else their
/// login account's. A room, a public one above all, learns nothing of this
/// person's devices.
pub(crate) fn own_person_name(chosen: Option<String>) -> String {
    chosen
        .filter(|name| runtime_identity::is_chosen_display_name(name))
        .and_then(|name| {
            tilo_user_identity::display_name(&name)
                .ok()
                .map(str::to_owned)
        })
        .unwrap_or_else(tilo_identity_host::provisioning::login_account_display_name)
}

/// A workspace this person was let into: the room its door's invitation
/// granted, the name it was listed under, and the invitation itself, which
/// this device enrolls into the room's state with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Joined {
    pub(crate) room: String,
    pub(crate) name: String,
    pub(crate) invitation: String,
}

/// What the worker is asked.
enum Ask {
    /// Read the community site and, when named, this one too.
    Read { site: Option<String> },
    /// Ask `door` to admit `person`, and verify what it answers.
    Join {
        key: String,
        name: String,
        door: url::Url,
        person: Person,
    },
    /// Join what the person pasted: an invitation's text, a site, or a
    /// workspace link.
    Take { text: String, person: Person },
}

/// An invitation that admits this person to a room: what a join is granted
/// with, and what enrolls this device into the room.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Granted {
    room: String,
    invitation: String,
}

/// What the worker answers.
enum Answer {
    /// What the sites list, and what could not be read.
    Read(Snapshot),
    /// Where joining the listing `key`, named `name`, stands: what it was
    /// granted, or why it was not.
    Joined {
        key: String,
        name: String,
        granted: Result<Granted, String>,
    },
    /// Where joining what the person pasted stands, named `name` when it got
    /// far enough to know it.
    Taken {
        key: String,
        name: String,
        granted: Result<Granted, String>,
    },
}

/// A door's refusal, when it says why.
#[derive(Deserialize)]
struct DoorRefusal {
    error: Option<String>,
}

/// The directories as last read, the joins asked for, and the worker that
/// carries both out.
#[derive(Default)]
pub(crate) struct Store {
    status: std::cell::RefCell<Status>,
    listed: std::cell::RefCell<Vec<Listed>>,
    note: std::cell::RefCell<Option<String>>,
    /// One join at a time.
    joining: std::cell::RefCell<Option<String>>,
    /// The join whose enrollment into its room is on its way, by the room's
    /// key, while it is.
    settling: std::cell::RefCell<Option<String>>,
    /// Why joining a listing failed, by its key, until it is joined.
    failures: std::cell::RefCell<BTreeMap<String, String>>,
    /// The workspaces joined since the session last took them, for the desk
    /// to add.
    joined: std::cell::RefCell<Vec<Joined>>,
    asks: std::cell::RefCell<Option<mpsc::Sender<Ask>>>,
    answers: std::cell::RefCell<Option<mpsc::Receiver<Answer>>>,
}

impl Store {
    /// The worker, started the first time anything is asked of it: one
    /// thread reads the sites and asks their doors, in turn.
    fn worker(&self) -> Option<mpsc::Sender<Ask>> {
        let asking = self.asks.borrow().clone();
        if let Some(asks) = asking {
            return Some(asks);
        }
        let (asks, waiting) = mpsc::channel();
        let (answers, read) = mpsc::channel();
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-directory".into())
            .spawn(move || work(&waiting, &answers))
        {
            tracing::warn!(%error, "the workspace directory is not read");
            return None;
        }
        *self.answers.borrow_mut() = Some(read);
        *self.asks.borrow_mut() = Some(asks.clone());
        Some(asks)
    }

    /// Read the directories again: the community site's, and `site`'s when
    /// the person named one.
    pub(crate) fn ask(&self, site: Option<String>) {
        *self.status.borrow_mut() = Status::Loading;
        *self.note.borrow_mut() = None;
        let Some(asks) = self.worker() else {
            return;
        };
        if let Err(error) = asks.send(Ask::Read { site }) {
            tracing::warn!(%error, "the workspace directory worker is gone");
        }
    }

    /// Join `listing`, which the sheet knows as `key`: ask its door to admit
    /// this person, off the frame loop. One join at a time.
    ///
    /// # Errors
    /// Why no door was asked: the listing lets people in by invitation only,
    /// or this computer has no identity to join as.
    pub(crate) fn join(&self, key: &str, listing: &Listed) -> Result<(), String> {
        let door = listing
            .door
            .clone()
            .ok_or_else(|| format!("{} needs an invitation", listing.entry.name.trim()))?;
        let person = Person::this_computer()?;
        let Some(asks) = self.worker() else {
            return Err("the directory has nowhere to ask".to_owned());
        };
        *self.joining.borrow_mut() = Some(key.to_owned());
        self.failures.borrow_mut().remove(key);
        *self.note.borrow_mut() = None;
        if let Err(error) = asks.send(Ask::Join {
            key: key.to_owned(),
            name: listing.entry.name.clone(),
            door,
            person,
        }) {
            *self.joining.borrow_mut() = None;
            return Err(format!("the join was not asked of its door: {error}"));
        }
        Ok(())
    }

    /// The listing `key` names, while it is listed.
    pub(crate) fn listing(&self, key: &str) -> Option<Listed> {
        self.listed
            .borrow()
            .iter()
            .find(|listing| listing.key() == key)
            .cloned()
    }

    /// Join what the person pasted into the sheet — an invitation's text, a
    /// site, or a workspace link — off the frame loop.
    ///
    /// # Errors
    /// Why nothing was asked: nothing was pasted, or this computer has no
    /// identity to join as.
    pub(crate) fn take(&self, text: &str) -> Result<(), String> {
        let text: String = text.split_whitespace().collect();
        if text.is_empty() {
            return Err("Paste a link or a code first.".to_owned());
        }
        let person = Person::this_computer()?;
        let Some(asks) = self.worker() else {
            return Err("the directory has nowhere to ask".to_owned());
        };
        *self.joining.borrow_mut() = Some(text.clone());
        self.failures.borrow_mut().remove(&text);
        *self.note.borrow_mut() = None;
        if let Err(error) = asks.send(Ask::Take { text, person }) {
            *self.joining.borrow_mut() = None;
            return Err(format!("that was not asked of its host: {error}"));
        }
        Ok(())
    }

    /// The join whose enrollment is on its way, by the room it is for, while
    /// it is: the sheet says so until it lands.
    pub(crate) fn expect_enrollment(&self, room: &str) {
        *self.settling.borrow_mut() = Some(room.to_owned());
        *self.note.borrow_mut() = Some(tilo_sheets::word("directory-joining"));
    }

    /// Whether a join is still being settled.
    pub(crate) fn settling(&self) -> bool {
        self.settling.borrow().is_some()
    }

    /// What became of the enrollment into the room of `key`: the join is
    /// settled, either way.
    ///
    /// A failure is said where the person pressed, and the row offers to
    /// join again.
    pub(crate) fn enrolled(&self, key: &str, name: &str, result: &Result<(), String>) {
        if self.settling.borrow().as_deref() != Some(key) {
            return;
        }
        *self.settling.borrow_mut() = None;
        match result {
            Ok(()) => *self.note.borrow_mut() = None,
            Err(reason) => {
                *self.note.borrow_mut() = Some(tilo_sheets::words(
                    "directory-join-failed",
                    &[("name", name), ("error", reason)],
                ));
                self.failures
                    .borrow_mut()
                    .insert(key.to_owned(), reason.clone());
            }
        }
    }

    /// The listing this session is joining, by its key, while it is.
    pub(crate) fn joining(&self) -> Option<String> {
        self.joining.borrow().clone()
    }

    /// Take the workspaces joined since this was last asked.
    pub(crate) fn take_joined(&self) -> Vec<Joined> {
        std::mem::take(&mut *self.joined.borrow_mut())
    }

    /// Take what the worker answered since the last call. Whether anything
    /// arrived.
    pub(crate) fn answered(&self) -> bool {
        let mut answered = false;
        let read = self.answers.borrow();
        let Some(read) = read.as_ref() else {
            return false;
        };
        while let Ok(answer) = read.try_recv() {
            answered = true;
            match answer {
                Answer::Read(Snapshot { listings, failures }) => {
                    if listings.is_empty() && !failures.is_empty() {
                        *self.status.borrow_mut() = Status::Failed(failures.join("; "));
                        continue;
                    }
                    *self.listed.borrow_mut() = listings;
                    *self.status.borrow_mut() = Status::Listed;
                }
                Answer::Joined { key, name, granted } => {
                    *self.joining.borrow_mut() = None;
                    match granted {
                        Ok(granted) => {
                            self.failures.borrow_mut().remove(&key);
                            self.joined.borrow_mut().push(Joined {
                                room: granted.room,
                                name,
                                invitation: granted.invitation,
                            });
                        }
                        Err(reason) => {
                            // The sheet says why, and the row says it was
                            // not joined, to try again.
                            *self.note.borrow_mut() = Some(tilo_sheets::words(
                                "directory-join-failed",
                                &[("name", &name), ("error", &reason)],
                            ));
                            self.failures.borrow_mut().insert(key, reason);
                        }
                    }
                }
                Answer::Taken { key, name, granted } => {
                    *self.joining.borrow_mut() = None;
                    match granted {
                        Ok(granted) => self.joined.borrow_mut().push(Joined {
                            room: granted.room,
                            name: if name.is_empty() {
                                tilo_sheets::word("directory-joined-name")
                            } else {
                                name
                            },
                            invitation: granted.invitation,
                        }),
                        Err(reason) => {
                            *self.note.borrow_mut() = Some(tilo_sheets::words(
                                "directory-code-failed",
                                &[("error", &reason)],
                            ));
                            self.failures.borrow_mut().insert(key, reason);
                        }
                    }
                }
            }
        }
        answered
    }

    /// What became of the last read or join, when it went wrong.
    pub(crate) fn set_note(&self, note: Option<String>) {
        *self.note.borrow_mut() = note;
    }
}

/// The worker: read what is asked, join where it is asked, and answer with
/// what the sites said.
fn work(asks: &mpsc::Receiver<Ask>, answers: &mpsc::Sender<Answer>) {
    while let Ok(ask) = asks.recv() {
        let answer = match ask {
            Ask::Read { site } => Answer::Read(read(site.as_deref())),
            Ask::Join {
                key,
                name,
                door,
                person,
            } => Answer::Joined {
                key,
                name,
                granted: join(&door, &person),
            },
            Ask::Take { text, person } => {
                let taken = take(&text, &person);
                Answer::Taken {
                    key: text,
                    name: taken.name,
                    granted: taken.granted,
                }
            }
        };
        if answers.send(answer).is_err() {
            return;
        }
    }
}

/// What joining the pasted `text` got: the name the workspace goes by, when
/// what was pasted says it, and what it was granted.
struct Taken {
    name: String,
    granted: Result<Granted, String>,
}

/// Join what the person pasted, as [`classify`] reads it: an invitation's
/// text is redeemed where it says; a site or a workspace link is joined
/// through the door of the workspace it names, on the site it names.
///
/// Blocks on the site: run it off the frame loop.
fn take(text: &str, person: &Person) -> Taken {
    let pasted = match classify(text) {
        Ok(pasted) => pasted,
        Err(error) => {
            return Taken {
                name: String::new(),
                granted: Err(error),
            };
        }
    };
    match pasted {
        Pasted::Invitation { text, entry } => Taken {
            name: entry.map_or_else(String::new, |entry| site_name(&entry)),
            granted: verify_invitation(&text, person),
        },
        Pasted::Site { entry, room } => {
            let name = site_name(&entry);
            let granted = read_site_home(&entry).and_then(|door| join(&door, person)).and_then(
                |granted| match room {
                    Some(room) if room.to_string() != granted.room => Err(
                        "that link is for another workspace on this site: ask whoever shared it for an invitation"
                            .to_owned(),
                    ),
                    _ => Ok(granted),
                },
            );
            Taken { name, granted }
        }
    }
}

/// An invitation redeemed for this person: the room it grants, and the text
/// the room's host admits this device by.
fn verify_invitation(text: &str, person: &Person) -> Result<Granted, String> {
    let verified = tilo_workspace_join::decode_and_verify_invitation_text(
        text.trim(),
        Some(person.user),
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .map_err(|error| format!("that invitation is not for you: {error}"))?;
    let room = verified.grant().room.clone();
    tilo_desktop_model::RoomId::parse(&room)
        .ok_or_else(|| "that invitation names an invalid room".to_owned())?;
    Ok(Granted {
        room,
        invitation: text.trim().to_owned(),
    })
}

/// What the person pasted, as the join field reads it.
enum Pasted {
    /// An invitation's text, redeemed at the host it names; with the site the
    /// link it came in is under, when it came in a link.
    Invitation {
        text: String,
        entry: Option<url::Url>,
    },
    /// A site, whose home workspace its door admits people to; with the room
    /// a link named, which must be that workspace.
    Site {
        entry: url::Url,
        room: Option<tilo_desktop_model::RoomId>,
    },
}

/// Read what the person pasted: an invitation's text, a workspace link, or a
/// site.
fn classify(text: &str) -> Result<Pasted, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("that is not a link or a code".to_owned());
    }
    if is_invitation(text) {
        return Ok(Pasted::Invitation {
            text: text.to_owned(),
            entry: None,
        });
    }
    if !text.contains('.') && !text.contains(':') && !text.contains('/') {
        return Err("that is not a link or a code".to_owned());
    }
    if text.contains('#') {
        let entry = link_entry(text);
        if let Some(entry) = &entry
            && let Ok(web_entry) = tilo_reference::link::WebEntry::new(entry)
            && let Ok(link) = tilo_reference::link::Link::parse(
                text,
                &tilo_reference::link::WebEntries::new(vec![web_entry]),
            )
        {
            let entry = site_entry(entry)?;
            if link.secrets.pair.is_some() {
                return Err("that link pairs a device with a host, not a workspace".to_owned());
            }
            let tilo_reference::link::Scope::Room { room, .. } = link.target.scope else {
                return Ok(Pasted::Site { entry, room: None });
            };
            let room = tilo_desktop_model::RoomId::from(room);
            if let Some(invitation) = link.secrets.invite {
                return Ok(Pasted::Invitation {
                    text: invitation.expose().to_owned(),
                    entry: Some(entry),
                });
            }
            return Ok(Pasted::Site {
                entry,
                room: Some(room),
            });
        }
    }
    Ok(Pasted::Site {
        entry: site_entry(text)?,
        room: None,
    })
}

/// Whether `text` is an invitation's text itself.
fn is_invitation(text: &str) -> bool {
    tilo_workspace_join::decode_and_verify_invitation_text(
        text,
        None,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .is_ok()
}

/// The entry of a web link: everything before its route, without the
/// full-tab `app/` segment.
fn link_entry(url: &str) -> Option<String> {
    let before = url.split_once('#').map_or(url, |(before, _)| before);
    let before = before.split_once('?').map_or(before, |(before, _)| before);
    let before = before.strip_suffix("app/").unwrap_or(before);
    before.ends_with('/').then(|| before.to_owned())
}

/// What a site calls itself, for a workspace joined by its link: its own
/// name, else the host's.
fn site_name(entry: &url::Url) -> String {
    let home = tilo_net::http::blocking_client()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .ok()
        .and_then(|client| read_site(&client, entry).ok())
        .and_then(|read| read.home);
    home.map_or_else(
        || entry.host_str().unwrap_or_default().to_owned(),
        |home| home.name,
    )
}

/// The door of the site's home workspace, as its `entry.json` names it.
/// Blocks on the site.
fn read_site_home(entry: &url::Url) -> Result<url::Url, String> {
    let client = tilo_net::http::blocking_client()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| format!("no network to join over: {error}"))?;
    let read = read_site(&client, entry)?;
    let home = read
        .home
        .ok_or_else(|| "that site has no workspace to join".to_owned())?;
    home.admission
        .ok_or_else(|| "that site joins people by invitation only".to_owned())
}

/// Ask `door` to admit this person, and verify the invitation it answers
/// with for them. The room that invitation grants is the workspace joined:
/// what a directory said about it is not what makes it so.
///
/// Blocks on the door: run it off the frame loop.
fn join(door: &url::Url, person: &Person) -> Result<Granted, String> {
    let client = tilo_net::http::blocking_client()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| format!("no network to join over: {error}"))?;
    let url = door
        .join("join")
        .map_err(|_| "that is not a web address".to_owned())?;
    let response = client
        .post(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&serde_json::json!({
            "userPublicKey": hex::encode(person.user),
            "displayName": person.name,
        }))
        .send()
        .map_err(|error| format!("the site could not be reached: {error}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .map_err(|error| format!("the site could not be reached: {error}"))?;
    if bytes.len() > MAX_ANSWER_BYTES {
        return Err("the door answered more than a door says".into());
    }
    if !status.is_success() {
        let refusal = serde_json::from_slice::<DoorRefusal>(&bytes)
            .ok()
            .and_then(|refusal| refusal.error)
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        // A 5xx is a host that is full or failing: worth trying again.
        return Err(if status.is_server_error() {
            format!("the site could not be reached: {refusal}")
        } else {
            format!("the workspace did not let you in: {refusal}")
        });
    }
    let answer: DoorAnswer = serde_json::from_slice(&bytes)
        .map_err(|error| format!("the workspace did not let you in: {error}"))?;
    let verified = tilo_workspace_join::decode_and_verify_invitation_text(
        &answer.invitation,
        Some(person.user),
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .map_err(|error| format!("the workspace did not let you in: {error}"))?;
    let room = verified.grant().room.clone();
    tilo_desktop_model::RoomId::parse(&room)
        .ok_or_else(|| "the door named an invalid room".to_owned())?;
    Ok(Granted {
        room,
        invitation: answer.invitation,
    })
}

/// What the community site and `typed` list, in the order the sheet shows
/// them. Blocks on the sites: run it off the frame loop.
fn read(typed: Option<&str>) -> Snapshot {
    let mut sites = Vec::new();
    if let Some(typed) = typed.map(str::trim).filter(|typed| !typed.is_empty()) {
        match site_entry(typed) {
            Ok(site) => sites.push(site),
            Err(error) => {
                return Snapshot {
                    listings: Vec::new(),
                    failures: vec![error],
                };
            }
        }
    }
    if let Some(community) = community_site() {
        sites.push(community);
    }
    let client = match tilo_net::http::blocking_client()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return Snapshot {
                listings: Vec::new(),
                failures: vec![format!("no network to read the directory over: {error}")],
            };
        }
    };
    let mut listings = Vec::new();
    let mut failures = Vec::new();
    let mut directories = BTreeSet::new();
    for site in &sites {
        match read_site(&client, site) {
            Ok(read) => {
                let mut listed = Vec::new();
                if let Some(directory) = &read.directory
                    && directories.insert(directory.to_string())
                {
                    match fetch_directory(&client, directory) {
                        Ok(page) => listed = listings_of(page, directory, Some(site)),
                        Err(error) => failures.push(error),
                    }
                }
                if let Some(home) = &read.home
                    && !listed.iter().any(|listing| listing.entry.community)
                {
                    listed.insert(0, home_listing(site, home));
                }
                // A site's own workspace is joined at its home, even when
                // its directory lists it without a door of its own.
                if let Some(home) = &read.home {
                    for listing in listed
                        .iter_mut()
                        .filter(|listing| listing.entry.community && listing.door.is_none())
                    {
                        listing.door.clone_from(&home.admission);
                    }
                }
                listings.extend(listed);
            }
            Err(error) => failures.push(error),
        }
    }
    Snapshot { listings, failures }
}

/// Read what the site at `entry` names: its home workspace and its
/// directory of public workspaces.
fn read_site(client: &reqwest::blocking::Client, entry: &url::Url) -> Result<SiteEntry, String> {
    let config: EntryJson = get_json(
        client,
        &entry
            .join("entry.json")
            .map_err(|_| "that is not a web address".to_owned())?,
    )?;
    let directory = config
        .directory
        .as_deref()
        .and_then(|directory| entry.join(directory).ok())
        .and_then(|directory| web_origin(directory.as_str()).ok());
    let home = match config.home {
        Some(home) => Some(home_workspace(entry, &home)?),
        None => None,
    };
    Ok(SiteEntry { home, directory })
}

/// The home workspace a site's `entry.json` names.
fn home_workspace(entry: &url::Url, home: &HomeJson) -> Result<SiteWorkspace, String> {
    let origin = web_origin(&home.host.origin)?;
    let admission = home
        .admission
        .as_deref()
        .map(|door| {
            entry
                .join(door)
                .map_err(|_| "that is not a web address".to_owned())
        })
        .transpose()?;
    let name = if home.name.trim().is_empty() {
        origin.host_str().unwrap_or_default().to_owned()
    } else {
        home.name.trim().to_owned()
    };
    Ok(SiteWorkspace {
        entry: entry.clone(),
        name,
        admission,
    })
}

/// Read `url` as JSON, at most [`MAX_ANSWER_BYTES`] of it.
fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::blocking::Client,
    url: &url::Url,
) -> Result<T, String> {
    let response = client
        .get(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .send()
        .map_err(|error| format!("the site could not be reached: {error}"))?;
    if !response.status().is_success() {
        return Err("that site has no workspace".into());
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("the site could not be reached: {error}"))?;
    if bytes.len() > MAX_ANSWER_BYTES {
        return Err("the site answered more than a site says".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "that site has no workspace".to_owned())
}

/// The public workspaces the directory at `directory` lists. Every
/// directory route is a `POST`, as a relay serves nothing else.
fn fetch_directory(
    client: &reqwest::blocking::Client,
    directory: &url::Url,
) -> Result<DirectoryPage, String> {
    let response = client
        .post(directory.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&serde_json::json!({}))
        .send()
        .map_err(|error| format!("the site could not be reached: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "the directory answered HTTP {}",
            response.status().as_u16()
        ));
    }
    let bytes = response
        .bytes()
        .map_err(|error| format!("the site could not be reached: {error}"))?;
    if bytes.len() > MAX_DIRECTORY_BYTES {
        return Err("the directory answered more than a directory says".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("the directory is unreadable: {error}"))
}

/// The words a role is shown with.
fn role_label(role: RoomRole) -> String {
    tilo_sheets::word(match role {
        RoomRole::Viewer => "directory-role-viewer",
        RoomRole::Commenter => "directory-role-commenter",
        RoomRole::Editor => "directory-role-editor",
        RoomRole::Moderator => "directory-role-moderator",
    })
}

/// How lively a listing is: the row's secondary line.
fn activity_line(listing: &Listed, now_ms: u64) -> String {
    match listing.activity(now_ms) {
        Activity::HereNow(count) => {
            tilo_sheets::words("directory-here-now", &[("count", &count.to_string())])
        }
        Activity::ActiveToday => tilo_sheets::word("directory-active-today"),
        Activity::ActiveThisWeek => tilo_sheets::word("directory-active-week"),
        Activity::Quiet => tilo_sheets::word("directory-quiet"),
    }
}

/// How many belong to a listing, in the row's own column; `None` when it
/// does not say.
fn members_line(listing: &Listed) -> Option<String> {
    match listing.entry.members {
        0 => None,
        members => Some(tilo_sheets::words(
            "directory-members",
            &[("count", &members.to_string())],
        )),
    }
}

/// The role people who join `listing` get, as its button describes it.
fn joins_as(listing: &Listed) -> String {
    tilo_sheets::words(
        "directory-joins-as",
        &[("role", &role_label(listing.entry.default_role))],
    )
}

/// What the sheet shows: the workspaces this computer holds, and what the
/// sites' directories list.
pub(crate) fn view(store: &Store, handle: &DesktopHandle) -> Directory {
    let now_ms = tilo_clock::unix_now_ms();
    // The workspaces this computer holds, by room, and the rows for those a
    // site does not list.
    let (held, desk_rows) = handle.read(|tree| {
        let held: BTreeMap<String, (String, String)> = tree
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.room().to_string(),
                    (workspace.id.0.to_string(), workspace.name.clone()),
                )
            })
            .collect();
        (held, tree.workspaces.clone())
    });
    let joining = store.joining();
    let failures = store.failures.borrow().clone();
    let mut listings = Vec::new();
    let mut shown: BTreeSet<String> = BTreeSet::new();
    for listing in ordered(store.listed.borrow().clone(), now_ms) {
        let listed_key = listing.key();
        let held_by = held.get(&listing.entry.room).cloned();
        let key = match &held_by {
            Some((id, _)) => {
                shown.insert(id.clone());
                id.clone()
            }
            None => listed_key.clone(),
        };
        let failure = failures.get(&listed_key);
        let action = if held_by.is_some() {
            ListingAction::Open
        } else if joining.as_deref() == Some(listed_key.as_str()) {
            ListingAction::Joining
        } else if failure.is_some() {
            ListingAction::TryAgain
        } else {
            ListingAction::Join
        };
        listings.push(Listing {
            key,
            name: listing.entry.name.clone(),
            about: if listing.entry.description.is_empty() {
                listing.host.clone()
            } else {
                listing.entry.description.clone()
            },
            host: listing.host.clone(),
            tag: listing
                .entry
                .community
                .then(|| tilo_sheets::words("directory-community-tag", &[("host", &listing.host)])),
            activity: failure.map_or_else(
                || activity_line(&listing, now_ms),
                |reason| tilo_sheets::words("directory-join-failed-line", &[("error", reason)]),
            ),
            members: failure.map_or_else(|| members_line(&listing), |_| None),
            joins_as: joins_as(&listing),
            here: listing
                .entry
                .here
                .iter()
                .map(|face| (face.name.clone(), face.user_public_key.clone()))
                .collect(),
            here_count: usize::try_from(listing.entry.here_count).unwrap_or(usize::MAX),
            action,
            waiting: joining.is_some()
                && !matches!(action, ListingAction::Open | ListingAction::Joining),
        });
    }
    for workspace in desk_rows {
        let id = workspace.id.0.to_string();
        if shown.contains(&id) {
            continue;
        }
        listings.push(Listing {
            key: id,
            name: workspace.name.clone(),
            action: ListingAction::Open,
            ..Listing::default()
        });
    }
    Directory {
        status: store.status.borrow().clone(),
        listings,
        note: store.note.borrow().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::directory::{Face, door_path};

    fn entry(name: &str) -> DirectoryEntry {
        let room = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
        DirectoryEntry {
            room: room.into(),
            name: name.into(),
            description: format!("All about {name}"),
            default_role: RoomRole::Commenter,
            door: door_path(room),
            here: Vec::new(),
            here_count: 0,
            members: 3,
            last_active_ms: None,
            community: false,
        }
    }

    fn directory() -> url::Url {
        url::Url::parse("https://eu1.tilo.coffee/peer/v1/directory").unwrap()
    }

    #[test]
    fn a_listings_door_is_read_against_its_directory() {
        let page = DirectoryPage {
            workspaces: vec![entry("Coffee")],
        };
        let listed = listings_of(page, &directory(), None);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].host, "eu1.tilo.coffee");
        assert_eq!(
            listed[0].door.as_ref().map(url::Url::as_str),
            Some(
                format!(
                    "https://eu1.tilo.coffee/peer/v1/directory/{}/",
                    listed[0].entry.room
                )
                .as_str()
            )
        );
        assert_eq!(listed[0].site, None, "a host's listing is no site's home");

        // A door that is not a web address is no door.
        let mut odd = entry("Odd");
        odd.door = "ftp://example.org/".into();
        let listed = listings_of(
            DirectoryPage {
                workspaces: vec![odd],
            },
            &directory(),
            None,
        );
        assert_eq!(
            listed.first().and_then(|listing| listing.door.clone()),
            None
        );
    }

    #[test]
    fn the_community_comes_first_then_the_liveliest_then_by_name() {
        let now = 10 * 24 * 60 * 60 * 1000;
        let room = |listing: &mut DirectoryEntry, index: u8| {
            listing.room = format!("6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b{index:02}");
        };
        let mut quiet = entry("Bakery");
        room(&mut quiet, 1);
        quiet.last_active_ms = Some(now - 5 * 24 * 60 * 60 * 1000);
        let mut busy = entry("Garden");
        room(&mut busy, 2);
        busy.here_count = 2;
        busy.here = vec![Face {
            user_public_key: hex::encode([1; 32]),
            name: "Ana".into(),
        }];
        let mut busier = entry("Zoo");
        room(&mut busier, 3);
        busier.here_count = 9;
        let mut recent = entry("Atelier");
        room(&mut recent, 4);
        recent.last_active_ms = Some(now - 60_000);
        let mut community = entry("tilo.coffee");
        room(&mut community, 5);
        community.community = true;
        let page = DirectoryPage {
            workspaces: vec![quiet, busy, busier, recent, community],
        };
        let shown: Vec<String> = ordered(listings_of(page, &directory(), None), now)
            .into_iter()
            .map(|listing| listing.entry.name)
            .collect();
        assert_eq!(shown, ["tilo.coffee", "Zoo", "Garden", "Atelier", "Bakery"]);
    }

    #[test]
    fn a_workspace_two_directories_list_is_shown_once() {
        let mut first = listings_of(
            DirectoryPage {
                workspaces: vec![entry("Coffee")],
            },
            &directory(),
            None,
        );
        first.extend(listings_of(
            DirectoryPage {
                workspaces: vec![entry("Coffee again")],
            },
            &url::Url::parse("https://other.example/peer/v1/directory").unwrap(),
            None,
        ));
        let shown = ordered(first, 0);
        assert_eq!(shown.len(), 1);
        assert_eq!(
            shown.first().map(|listing| listing.entry.name.as_str()),
            Some("Coffee")
        );
    }

    #[test]
    fn a_site_without_a_directory_still_lists_its_home() {
        let site = url::Url::parse("https://tilo.coffee/").unwrap();
        let home = SiteWorkspace {
            entry: site.clone(),
            name: "tilo.coffee".into(),
            admission: Some(url::Url::parse("https://rooms.tilo.coffee/").unwrap()),
        };
        let listed = home_listing(&site, &home);
        assert!(listed.entry.community);
        assert_eq!(listed.host, "tilo.coffee");
        assert_eq!(listed.site, Some(site));
        assert_eq!(listed.door, home.admission);
        assert_eq!(listed.key(), "https://tilo.coffee/");
    }

    #[test]
    fn a_typed_site_is_read_with_or_without_its_scheme() {
        assert_eq!(
            site_entry("example.org").map(|entry| entry.to_string()),
            Ok("https://example.org/".to_owned())
        );
        assert_eq!(
            site_entry("https://example.org/coffee").map(|entry| entry.to_string()),
            Ok("https://example.org/coffee/".to_owned())
        );
        assert_eq!(
            site_entry("http://127.0.0.1:8080").map(|entry| entry.host_str().map(str::to_owned)),
            Ok(Some("127.0.0.1".to_owned())),
            "a host under development on this machine's loopback"
        );
        assert!(site_entry("ftp://example.org").is_err());
        assert!(site_entry("two words").is_err());
    }

    #[test]
    fn who_is_here_and_how_many_belong_are_two_facts() {
        let mut busy = entry("Busy");
        busy.here_count = 3;
        busy.members = 12;
        let listed = Listed {
            entry: busy,
            door: None,
            site: None,
            host: "eu1.tilo.coffee".into(),
        };
        let activity = activity_line(&listed, 0);
        assert!(activity.contains('3'), "{activity}");
        assert!(!activity.contains("12"), "{activity}");
        let members = members_line(&listed).expect("a listing with members");
        assert!(members.contains("12"), "{members}");
    }
}
