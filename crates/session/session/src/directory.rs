//! The public workspaces the sites this computer knows list: what "Join a
//! workspace…" shows of other people's workspaces, and where each one can
//! be joined.
//!
//! A site names its home workspace and the directory of its public ones in
//! its `entry.json`; the directory then lists every public workspace it
//! serves with what it is, who is in it now and how lively it is
//! (`tilo_relay_protocol::directory`). The community site this computer is
//! configured with is read first, and a site the person types is read too.
//! Every read happens on a worker of its own, never on the session's loop
//! ([`Store::ask`] and [`Store::answered`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;
use tilo_desktop_surface::DesktopHandle;
use tilo_relay_protocol::directory::{Activity, DirectoryEntry, DirectoryPage};
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
    let mut seen = std::collections::BTreeSet::new();
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

/// What the worker is asked.
enum Ask {
    /// Read the community site and, when named, this one too.
    Read { site: Option<String> },
}

/// The directories as last read, and the worker that reads them.
#[derive(Default)]
pub(crate) struct Store {
    status: std::cell::RefCell<Status>,
    listed: std::cell::RefCell<Vec<Listed>>,
    note: std::cell::RefCell<Option<String>>,
    asks: std::cell::RefCell<Option<mpsc::Sender<Ask>>>,
    answers: std::cell::RefCell<Option<mpsc::Receiver<Snapshot>>>,
}

impl Store {
    /// Read the directories again: the community site's, and `site`'s when
    /// the person named one.
    pub(crate) fn ask(&self, site: Option<String>) {
        *self.status.borrow_mut() = Status::Loading;
        *self.note.borrow_mut() = None;
        let asking = self.asks.borrow().clone();
        let asks = if let Some(asks) = asking {
            asks
        } else {
            let (asks, asking) = mpsc::channel();
            let (answers, read) = mpsc::channel();
            if let Err(error) = std::thread::Builder::new()
                .name("tilo-directory".into())
                .spawn(move || work(&asking, &answers))
            {
                tracing::warn!(%error, "the workspace directory is not read");
                return;
            }
            *self.answers.borrow_mut() = Some(read);
            *self.asks.borrow_mut() = Some(asks.clone());
            asks
        };
        if let Err(error) = asks.send(Ask::Read { site }) {
            tracing::warn!(%error, "the workspace directory worker is gone");
        }
    }

    /// Take what the worker read since the last call. Whether anything
    /// arrived.
    pub(crate) fn answered(&self) -> bool {
        let mut answered = false;
        let read = self.answers.borrow();
        let Some(read) = read.as_ref() else {
            return false;
        };
        while let Ok(snapshot) = read.try_recv() {
            answered = true;
            let Snapshot { listings, failures } = snapshot;
            if listings.is_empty() && !failures.is_empty() {
                *self.status.borrow_mut() = Status::Failed(failures.join("; "));
                continue;
            }
            *self.listed.borrow_mut() = listings;
            *self.status.borrow_mut() = Status::Listed;
        }
        answered
    }

    /// What became of the last read or join, when it went wrong.
    pub(crate) fn set_note(&self, note: Option<String>) {
        *self.note.borrow_mut() = note;
    }
}

/// The worker: read what is asked, and answer with what the sites said.
fn work(asks: &mpsc::Receiver<Ask>, answers: &mpsc::Sender<Snapshot>) {
    while let Ok(Ask::Read { site }) = asks.recv() {
        if answers.send(read(site.as_deref())).is_err() {
            return;
        }
    }
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
    let mut directories = std::collections::BTreeSet::new();
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

/// How lively a listing is, and how many belong to it, in a line.
fn activity_line(listing: &Listed, now_ms: u64) -> String {
    let activity = match listing.activity(now_ms) {
        Activity::HereNow(count) => {
            tilo_sheets::words("directory-here-now", &[("count", &count.to_string())])
        }
        Activity::ActiveToday => tilo_sheets::word("directory-active-today"),
        Activity::ActiveThisWeek => tilo_sheets::word("directory-active-week"),
        Activity::Quiet => tilo_sheets::word("directory-quiet"),
    };
    match listing.entry.members {
        0 => activity,
        members => format!(
            "{activity} · {}",
            tilo_sheets::words("directory-members", &[("count", &members.to_string())])
        ),
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
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis()),
    )
    .unwrap_or(u64::MAX);
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
    let mut listings = Vec::new();
    let mut shown: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for listing in ordered(store.listed.borrow().clone(), now_ms) {
        let held_by = held.get(&listing.entry.room).cloned();
        let key = match &held_by {
            Some((id, _)) => {
                shown.insert(id.clone());
                id.clone()
            }
            None => listing.key(),
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
            activity: activity_line(&listing, now_ms),
            joins_as: joins_as(&listing),
            here: listing
                .entry
                .here
                .iter()
                .map(|face| (face.name.clone(), face.user_public_key.clone()))
                .collect(),
            here_count: usize::try_from(listing.entry.here_count).unwrap_or(usize::MAX),
            action: if held_by.is_some() {
                ListingAction::Open
            } else {
                ListingAction::Join
            },
            waiting: false,
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
    fn the_activity_line_says_who_is_here_and_how_many_belong() {
        let mut busy = entry("Busy");
        busy.here_count = 3;
        busy.members = 12;
        let line = activity_line(
            &Listed {
                entry: busy,
                door: None,
                site: None,
                host: "eu1.tilo.coffee".into(),
            },
            0,
        );
        assert!(line.contains('3'), "{line}");
        assert!(line.contains("12"), "{line}");
        assert_eq!(
            line.matches(" · ").count(),
            1,
            "a row says one thing: {line}"
        );
    }
}
