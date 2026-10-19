//! Workspaces this computer holds through a web site: the community
//! workspace a Tilo computer is in by default, one a person joins by its
//! site, or one a link opened here names.
//!
//! A site names its home workspace in its `entry.json`
//! (docs/architecture/web-routing.md, §8): the workspace's host by its HTTPS
//! origin, and the host's admission door when it lets people in without an
//! invitation. The computer joins the way a browser does: the door
//! pre-admits the person, and this device redeems the door's invitation
//! with its own certificate from the person's root, through the host's
//! workspace join exchange over HTTPS. The room is then held like every
//! other governed room, by the same native owner; only its session is
//! carried over the host's HTTPS routes, since the host publishes no iroh
//! address. Which rooms those are is kept here, so the session reaches them
//! the same way after a restart.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

use crate::shell::state::{RoomId, WorkspaceId};

/// How long establishing a connection to a site or host may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a site's entry or its door may take to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest `entry.json` or door answer read.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
/// The largest directory read: a few hundred listings with their faces.
const MAX_DIRECTORY_BYTES: usize = 1024 * 1024;

fn hosted_rooms_path(state_dir: &Path) -> PathBuf {
    state_dir.join("hosted-rooms.json")
}

/// A room this computer reaches over its host's HTTPS routes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct HostedRoom {
    /// The host's origin.
    pub origin: String,
    /// The site whose home workspace the room is, when it was joined as one.
    pub site: Option<String>,
    /// The host's device key (hex) when the room was joined: a host that
    /// answers with another was set up again.
    pub host: Option<String>,
    /// The app the site's workspace opens in (`so.tilo.chat`), as its
    /// entry names it: the person arrives in it once the join installs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct HostedRooms {
    rooms: BTreeMap<String, HostedRoom>,
    /// Sites whose workspace the person left: never joined again unasked.
    left: BTreeSet<String>,
}

/// Where joining a site's workspace stands, while this session joins it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinProgress {
    /// Its host admitted this device; the workspace installs once its
    /// enrollment completes.
    Joining,
    /// It was not joined, and why, in the person's words.
    Failed(String),
}

/// A site's workspace this session is joining or failed to join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteJoinState {
    /// Its room.
    pub room: RoomId,
    /// The site it is the workspace of, as its listing names it.
    pub site: Option<String>,
    /// The workspace slot it installs into, while there is one.
    pub workspace: Option<WorkspaceId>,
    /// Where it stands.
    pub progress: JoinProgress,
}

/// A site's workspace whose join just installed: the person arrives in it,
/// in the app its site names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrival {
    /// Its room.
    pub room: RoomId,
    /// The app it opens in, once.
    pub app: Option<String>,
}

/// The rooms held through a site, kept beside the rest of the peer state.
#[derive(Default)]
pub(super) struct State {
    path: Option<PathBuf>,
    rooms: Mutex<HostedRooms>,
    /// The community site this computer follows, once it does: its
    /// workspace is named as the community's.
    community: std::sync::OnceLock<String>,
    /// The site workspaces this session is joining, or failed to.
    joins: Mutex<BTreeMap<RoomId, SiteJoinState>>,
    /// Joins installed since chrome last looked.
    arrivals: Mutex<Vec<Arrival>>,
}

impl State {
    /// What `state_dir` keeps; nothing yet, or nothing readable, is none.
    pub(super) fn open(state_dir: &Path) -> Self {
        let path = hosted_rooms_path(state_dir);
        let rooms = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: Some(path),
            rooms: Mutex::new(rooms),
            ..Self::default()
        }
    }

    /// Say where joining `room` stands now.
    pub(super) fn set_join(&self, state: SiteJoinState) {
        self.joins.lock_recover().insert(state.room, state);
    }

    /// Joining `room` is over: it installed, or the person left it.
    pub(super) fn end_join(&self, room: RoomId) -> Option<SiteJoinState> {
        self.joins.lock_recover().remove(&room)
    }

    /// Where joining `room` stands, while it does.
    pub(super) fn join_of(&self, room: RoomId) -> Option<SiteJoinState> {
        self.joins.lock_recover().get(&room).cloned()
    }

    /// Every join this session runs or failed.
    pub(super) fn joins(&self) -> Vec<SiteJoinState> {
        self.joins.lock_recover().values().cloned().collect()
    }

    /// `room`'s join installed: the person arrives in it.
    pub(super) fn arrive(&self, room: RoomId) {
        self.end_join(room);
        let app = self.hosted(room).and_then(|hosted| hosted.app);
        self.arrivals.lock_recover().push(Arrival { room, app });
    }

    /// The joins installed since this was last asked.
    pub(super) fn take_arrivals(&self) -> Vec<Arrival> {
        std::mem::take(&mut *self.arrivals.lock_recover())
    }

    /// The origin of `room`'s host, when this computer reaches it over HTTPS.
    pub(super) fn origin(&self, room: RoomId) -> Option<String> {
        self.hosted(room).map(|hosted| hosted.origin)
    }

    /// How this computer reaches `room`, when over HTTPS.
    pub(super) fn hosted(&self, room: RoomId) -> Option<HostedRoom> {
        self.rooms
            .lock_recover()
            .rooms
            .get(&room.to_string())
            .cloned()
    }

    /// Every room reached over its host's HTTPS routes.
    /// Remember `site` as the community this computer follows.
    pub(super) fn follow_community(&self, site: &url::Url) {
        let _ = self.community.set(site.as_str().to_owned());
    }

    /// Whether `site` is the community this computer follows.
    pub(super) fn is_community(&self, site: &str) -> bool {
        self.community
            .get()
            .is_some_and(|community| community == site)
    }

    pub(super) fn rooms(&self) -> Vec<RoomId> {
        self.rooms
            .lock_recover()
            .rooms
            .keys()
            .filter_map(|room| RoomId::parse(room))
            .collect()
    }

    /// The room held as `site`'s home workspace.
    pub(super) fn room_of_site(&self, site: &url::Url) -> Option<RoomId> {
        self.rooms
            .lock_recover()
            .rooms
            .iter()
            .find(|(_, hosted)| hosted.site.as_deref() == Some(site.as_str()))
            .and_then(|(room, _)| RoomId::parse(room))
    }

    /// Reach `room`'s host as `hosted` says from now on.
    pub(super) fn remember(&self, room: RoomId, hosted: HostedRoom) -> Result<(), String> {
        let mut rooms = self.rooms.lock_recover();
        rooms.rooms.insert(room.to_string(), hosted);
        self.persist(&rooms)
    }

    /// Stop reaching `room` through a site.
    pub(super) fn forget(&self, room: RoomId) -> Result<(), String> {
        let mut rooms = self.rooms.lock_recover();
        if rooms.rooms.remove(&room.to_string()).is_none() {
            return Ok(());
        }
        self.persist(&rooms)
    }

    /// Whether the person left `site`'s workspace.
    pub(super) fn left(&self, site: &url::Url) -> bool {
        self.rooms.lock_recover().left.contains(site.as_str())
    }

    /// Remember that the person left `site`'s workspace, or that they came
    /// back to it.
    pub(super) fn set_left(&self, site: &url::Url, left: bool) -> Result<(), String> {
        let mut rooms = self.rooms.lock_recover();
        let changed = if left {
            rooms.left.insert(site.as_str().to_owned())
        } else {
            rooms.left.remove(site.as_str())
        };
        if !changed {
            return Ok(());
        }
        self.persist(&rooms)
    }

    fn persist(&self, rooms: &HostedRooms) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(rooms).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
            .map_err(|error| error.to_string())
    }
}

/// An origin a host or site is reached at: HTTPS, or plain HTTP on this
/// machine's loopback only (a host under development).
pub(crate) fn web_origin(text: &str) -> Result<url::Url, SiteError> {
    let url = url::Url::parse(text).map_err(|_| SiteError::Invalid)?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err(SiteError::Invalid),
    }
    if url.cannot_be_a_base() || url.host_str().is_none() {
        return Err(SiteError::Invalid);
    }
    Ok(url)
}

fn client() -> Result<tilo_net::Client, SiteError> {
    tilo_net::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| SiteError::Unreachable(error.to_string()))
}

/// The routes of the host at one origin, for the relay session client and
/// the workspace join exchange.
pub(crate) struct HostHttp {
    client: tilo_net::Client,
    origin: url::Url,
}

impl HostHttp {
    /// The host at `origin`.
    pub(crate) fn new(origin: &str) -> Result<Arc<Self>, String> {
        let origin = web_origin(origin).map_err(|error| error.to_string())?;
        Ok(Arc::new(Self {
            client: client().map_err(|error| error.to_string())?,
            origin,
        }))
    }
}

impl tilo_relay_client::HttpClient for HostHttp {
    fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> tilo_relay_client::HttpFuture<'_> {
        let url = self.origin.join(path);
        let session = session.map(str::to_owned);
        Box::pin(async move {
            let url = url.map_err(|error| error.to_string())?;
            let mut request = self
                .client
                .post(url.as_str())
                .header(tilo_net::header::CONTENT_TYPE, "application/octet-stream")
                .body(body);
            if let Some(session) = session {
                request = request.header(tilo_relay_protocol::web::SESSION_HEADER, session);
            }
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response.bytes().await.map_err(|error| error.to_string())?;
            Ok((status, body.to_vec()))
        })
    }
}

/// Why a site's workspace cannot be joined.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SiteError {
    /// Not a site or workspace address.
    #[error("that is not a web address")]
    Invalid,
    /// The site or its host did not answer.
    #[error("the site could not be reached: {0}")]
    Unreachable(String),
    /// The site names no workspace of its own.
    #[error("that site has no workspace")]
    NoWorkspace,
    /// Its workspace lets people in by invitation only.
    #[error("{0} needs an invitation")]
    InvitationOnly(String),
    /// The door turned the person away, and why.
    #[error("the workspace did not let you in: {0}")]
    Refused(String),
    /// A link that makes a new device one of a person's, pasted on a
    /// computer that is someone's already.
    #[error(
        "That is a pairing link. This computer has its own identity; open the link on the device you are adding."
    )]
    PairingLink,
}

/// A site's home workspace, as its `entry.json` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SiteWorkspace {
    /// The site's entry: where its links point.
    pub entry: url::Url,
    /// The workspace's name.
    pub name: String,
    /// The HTTPS origin of its host.
    pub origin: String,
    /// The host's admission door, when it admits people without an
    /// invitation.
    pub admission: Option<url::Url>,
    /// The site's directory of public workspaces, when it names one.
    pub directory: Option<url::Url>,
    /// The app the workspace opens in, when the site names one.
    pub app: Option<String>,
}

#[derive(Deserialize)]
struct EntryJson {
    home: Option<HomeJson>,
    /// The site's directory of public workspaces.
    directory: Option<String>,
}

/// What a site's `entry.json` says: its home workspace and its directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SiteEntry {
    /// The home workspace, when the site names one.
    pub home: Option<SiteWorkspace>,
    /// The directory of public workspaces, when the site names one.
    pub directory: Option<url::Url>,
}

#[derive(Deserialize)]
struct HomeJson {
    #[serde(default)]
    name: String,
    host: HostJson,
    admission: Option<String>,
    /// The app the workspace opens in (`so.tilo.chat`).
    app: Option<String>,
}

#[derive(Deserialize)]
struct HostJson {
    origin: String,
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &tilo_net::Client,
    url: &url::Url,
) -> Result<T, SiteError> {
    let response = client
        .get(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .send()
        .await
        .map_err(|error| SiteError::Unreachable(error.to_string()))?;
    if !response.status().is_success() {
        return Err(SiteError::NoWorkspace);
    }
    let bytes = response
        .bytes_limited(MAX_ANSWER_BYTES)
        .await
        .map_err(|error| SiteError::Unreachable(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|_| SiteError::NoWorkspace)
}

/// The entry a person typed: a site's address, with or without its scheme
/// and path. The route of a link is not part of it.
pub(crate) fn site_entry(text: &str) -> Result<url::Url, SiteError> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err(SiteError::Invalid);
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

/// Read the home workspace the site at `entry` names.
pub(crate) async fn resolve_site(entry: &url::Url) -> Result<SiteWorkspace, SiteError> {
    read_site(entry).await?.home.ok_or(SiteError::NoWorkspace)
}

/// Read what the site at `entry` names: its home workspace and its
/// directory of public workspaces.
pub(crate) async fn read_site(entry: &url::Url) -> Result<SiteEntry, SiteError> {
    let client = client()?;
    let config: EntryJson = get_json(
        &client,
        &entry.join("entry.json").map_err(|_| SiteError::Invalid)?,
    )
    .await?;
    let directory = config
        .directory
        .as_deref()
        .and_then(|directory| entry.join(directory).ok())
        .and_then(|directory| web_origin(directory.as_str()).ok());
    let home = match config.home {
        Some(home) => Some(home_workspace(entry, home, directory.clone())?),
        None => None,
    };
    Ok(SiteEntry { home, directory })
}

/// The home workspace `home` of the site at `entry`.
fn home_workspace(
    entry: &url::Url,
    home: HomeJson,
    directory: Option<url::Url>,
) -> Result<SiteWorkspace, SiteError> {
    let origin = web_origin(&home.host.origin)?;
    let admission = home
        .admission
        .as_deref()
        .map(|door| entry.join(door).map_err(|_| SiteError::Invalid))
        .transpose()?;
    let name = if home.name.trim().is_empty() {
        origin.host_str().unwrap_or_default().to_owned()
    } else {
        home.name.trim().to_owned()
    };
    Ok(SiteWorkspace {
        entry: entry.clone(),
        name,
        origin: origin.origin().ascii_serialization(),
        admission,
        directory,
        app: home.app.as_deref().and_then(app_id),
    })
}

/// `text` as an app id a site may name (`so.tilo.chat`): dotted lowercase
/// segments of letters, digits and `-`. Anything else names no app.
fn app_id(text: &str) -> Option<String> {
    let text = text.trim();
    let segment = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    };
    (text.len() <= 128 && text.contains('.') && text.split('.').all(segment))
        .then(|| text.to_owned())
}

/// The public workspaces the directory at `directory` lists. Every
/// directory route is a `POST`, as a relay serves nothing else.
pub(crate) async fn fetch_directory(
    directory: &url::Url,
) -> Result<tilo_relay_protocol::directory::DirectoryPage, SiteError> {
    let response = client()?
        .post(directory.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|error| SiteError::Unreachable(error.to_string()))?;
    if !response.status().is_success() {
        return Err(SiteError::Unreachable(format!(
            "the directory answered HTTP {}",
            response.status().as_u16()
        )));
    }
    let bytes = response
        .bytes_limited(MAX_DIRECTORY_BYTES)
        .await
        .map_err(|error| SiteError::Unreachable(error.to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| SiteError::Unreachable(format!("the directory is unreadable: {error}")))
}

/// Report the public workspace behind `door` to the people who run it.
pub(crate) async fn report_at_door(
    door: &url::Url,
    reporter: [u8; 32],
    reason: &str,
) -> Result<(), SiteError> {
    let url = door.join("report").map_err(|_| SiteError::Invalid)?;
    let response = client()?
        .post(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&tilo_relay_protocol::directory::DoorReport {
            reporter_user_public_key: hex::encode(reporter),
            reason: reason.to_owned(),
        })
        .send()
        .await
        .map_err(|error| SiteError::Unreachable(error.to_string()))?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(SiteError::Refused(format!(
        "HTTP {}",
        response.status().as_u16()
    )))
}

/// What a door answers a person it admits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DoorAnswer {
    /// The invitation bound to the person, as text.
    pub invitation: String,
    /// The one-time token that admits them at once.
    pub admission_token: String,
    /// The origin of the host to redeem it at: the one the door named, else
    /// the door's own.
    pub origin: String,
}

#[derive(Deserialize)]
struct DoorRefusal {
    error: Option<String>,
}

/// Ask `workspace`'s door to admit the person `user` under `name`.
pub(crate) async fn admit(
    workspace: &SiteWorkspace,
    user: [u8; 32],
    name: &str,
) -> Result<DoorAnswer, SiteError> {
    let door = workspace
        .admission
        .as_ref()
        .ok_or_else(|| SiteError::InvitationOnly(workspace.name.clone()))?;
    let url = door.join("join").map_err(|_| SiteError::Invalid)?;
    let response = client()?
        .post(url.as_str())
        .timeout(ANSWER_TIMEOUT)
        .json(&serde_json::json!({
            "userPublicKey": hex::encode(user),
            "displayName": name,
        }))
        .send()
        .await
        .map_err(|error| SiteError::Unreachable(error.to_string()))?;
    let status = response.status();
    let bytes = response
        .bytes_limited(MAX_ANSWER_BYTES)
        .await
        .map_err(|error| SiteError::Unreachable(error.to_string()))?;
    if status.is_success() {
        let answer: tilo_relay_protocol::directory::DoorAnswer = serde_json::from_slice(&bytes)
            .map_err(|error| SiteError::Refused(error.to_string()))?;
        // A relay's door answers on the host it admits to.
        let origin = answer
            .origin
            .unwrap_or_else(|| door.origin().ascii_serialization());
        web_origin(&origin)?;
        return Ok(DoorAnswer {
            invitation: answer.invitation,
            admission_token: answer.admission_token,
            origin,
        });
    }
    let reason = serde_json::from_slice::<DoorRefusal>(&bytes)
        .ok()
        .and_then(|refusal| refusal.error)
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
    // A 503 is a full room or a failing host: worth trying again later.
    if status.is_server_error() {
        return Err(SiteError::Unreachable(reason));
    }
    Err(SiteError::Refused(reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_site_names_its_entry_directory() {
        assert_eq!(
            site_entry("tilo.coffee").unwrap().as_str(),
            "https://tilo.coffee/"
        );
        assert_eq!(
            site_entry(" https://example.org/open?x=1#/w/abc ")
                .unwrap()
                .as_str(),
            "https://example.org/open/"
        );
        assert_eq!(
            site_entry("http://localhost:4321").unwrap().as_str(),
            "http://localhost:4321/"
        );
        for refused in ["", "http://example.org", "ftp://example.org", "two words"] {
            assert_eq!(site_entry(refused), Err(SiteError::Invalid), "{refused}");
        }
    }

    #[test]
    fn a_sites_home_names_the_app_it_opens_in_and_nothing_else_is_an_app() {
        let entry = site_entry("tilo.coffee").unwrap();
        let home = |app: Option<&str>| HomeJson {
            name: "Lounge".into(),
            host: HostJson {
                origin: "https://eu1.tilo.coffee".into(),
            },
            admission: Some("https://eu1.tilo.coffee/".into()),
            app: app.map(str::to_owned),
        };
        let read = |app| home_workspace(&entry, home(app), None).unwrap().app;
        assert_eq!(read(Some("so.tilo.chat")).as_deref(), Some("so.tilo.chat"));
        assert_eq!(read(None), None);
        for refused in [
            "",
            "chat",
            "../so.tilo.chat",
            "so.tilo.Chat",
            "so..chat",
            "so.tilo/chat",
        ] {
            assert_eq!(read(Some(refused)), None, "{refused}");
        }
    }

    #[test]
    fn hosted_rooms_are_kept_across_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let room = RoomId::new();
        let state = State::open(dir.path());
        assert_eq!(state.origin(room), None);
        let site = site_entry("example.org").unwrap();
        state
            .remember(
                room,
                HostedRoom {
                    origin: "https://eu1.example".into(),
                    site: Some(site.to_string()),
                    host: None,
                    app: Some("so.tilo.chat".into()),
                },
            )
            .unwrap();
        let reopened = State::open(dir.path());
        assert_eq!(
            reopened.origin(room).as_deref(),
            Some("https://eu1.example")
        );
        assert_eq!(reopened.room_of_site(&site), Some(room));
        assert_eq!(
            reopened
                .hosted(room)
                .and_then(|hosted| hosted.app)
                .as_deref(),
            Some("so.tilo.chat"),
            "the app the site's workspace opens in is kept"
        );
        state.set_left(&site, true).unwrap();
        assert!(State::open(dir.path()).left(&site));
        state.forget(room).unwrap();
        assert_eq!(State::open(dir.path()).origin(room), None);
        state.set_left(&site, false).unwrap();
        assert!(!State::open(dir.path()).left(&site));
    }
}
