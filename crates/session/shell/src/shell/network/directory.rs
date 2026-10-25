//! The public workspaces a computer offers to join, and making one of the
//! person's own public.
//!
//! "Join a workspace…" lists what the sites this computer knows serve in
//! their directories (`tilo_relay_protocol::directory`): the community site
//! the image names first (its own workspace at the top), then the hosts of
//! the workspaces this computer holds through the web, and a site the person
//! types. Each listing says what the workspace is, who is in it now and how
//! lively it is; joining one goes through its door, which admits the person
//! with the role its owner set, and the workspace is held like any other
//! from then on. Nothing here keeps a list of its own: every listing is read
//! from the host that serves the workspace, when the sheet asks.
//!
//! Publishing is the room's own governance on the host it lives on: the
//! room's `publish`, `unpublish` and `publication` actions, asked as this
//! device of the person who administers it. A workspace only this computer
//! holds has no host to list it yet.

use tilo_relay_protocol::directory::{Activity, DirectoryEntry, DirectoryPage, Listing};
use tilo_relay_protocol::rooms::RoomRole;

use super::hosted_sites::{self, SiteError, SiteWorkspace};
use super::site_join::SiteJoin;
use super::{RoomId, ShellNetwork};

/// The directory of a relay host, below its origin.
fn relay_directory(origin: &str) -> Option<url::Url> {
    let origin = hosted_sites::web_origin(origin).ok()?;
    origin
        .join(&format!(
            "{}{}",
            tilo_relay_protocol::web::API_PREFIX,
            tilo_relay_protocol::directory::DIRECTORY_PATH
        ))
        .ok()
}

/// One public workspace the sheet lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryListing {
    /// What its directory says about it.
    pub entry: DirectoryEntry,
    /// Its room, when the directory names one this computer can read.
    pub room: Option<RoomId>,
    /// Its door, resolved; `None` for a site's home workspace that lets
    /// people in by invitation only.
    pub door: Option<url::Url>,
    /// The site whose own workspace it is, for the site's community
    /// listing: joining it joins that site's home.
    pub site: Option<url::Url>,
    /// The host name it is listed by ("tilo.coffee").
    pub host: String,
    /// This computer holds it already.
    pub joined: bool,
}

impl DirectoryListing {
    /// How lively it is at `now_ms`.
    #[must_use]
    pub fn activity(&self, now_ms: u64) -> Activity {
        self.entry.activity(now_ms)
    }

    /// Whether it answers what the person typed into the sheet's search:
    /// its name, description or host, ignoring case. Nothing typed matches
    /// everything.
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        [
            self.entry.name.as_str(),
            self.entry.description.as_str(),
            self.host.as_str(),
        ]
        .iter()
        .any(|field| field.to_lowercase().contains(&query))
    }
}

/// The listings of `page`, read from the directory at `directory`. `site`
/// is the site the directory belongs to, when it is a site's: its community
/// listing joins that site's home.
pub(crate) fn listings_of(
    page: DirectoryPage,
    directory: &url::Url,
    site: Option<&url::Url>,
) -> Vec<DirectoryListing> {
    let host = directory.host_str().unwrap_or_default().to_owned();
    page.workspaces
        .into_iter()
        .filter(|entry| !entry.name.trim().is_empty())
        .map(|entry| DirectoryListing {
            room: RoomId::parse(&entry.room),
            door: directory
                .join(&entry.door)
                .ok()
                .and_then(|door| hosted_sites::web_origin(door.as_str()).ok()),
            site: site.filter(|_| entry.community).cloned(),
            host: host.clone(),
            joined: false,
            entry,
        })
        .collect()
}

/// The listing of a site's home workspace that its directory does not list
/// (or that names no directory): what its `entry.json` says.
pub(crate) fn home_listing(home: &SiteWorkspace) -> DirectoryListing {
    DirectoryListing {
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
        room: None,
        door: home.admission.clone(),
        site: Some(home.entry.clone()),
        host: home.entry.host_str().unwrap_or_default().to_owned(),
        joined: false,
    }
}

/// The listings in the order the sheet shows them: the community's own
/// first, then the liveliest (most people in it now, then most recently
/// active), then by name. A workspace two directories list is shown once,
/// as the first one listed it.
pub(crate) fn ordered(listings: Vec<DirectoryListing>, now_ms: u64) -> Vec<DirectoryListing> {
    let mut seen = std::collections::BTreeSet::new();
    let mut kept: Vec<_> = listings
        .into_iter()
        .filter(|listing| match (listing.room, &listing.site) {
            (Some(room), _) => seen.insert(room.to_string()),
            (None, Some(site)) => seen.insert(site.to_string()),
            (None, None) => true,
        })
        .collect();
    let liveliness = |listing: &DirectoryListing| match listing.activity(now_ms) {
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

/// Whether a public workspace is listed, and with what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Publication {
    /// The room it is about now: another one than asked when making it
    /// public moved the workspace onto the person's host.
    pub room: RoomId,
    /// Listed in the directory of the host it lives on.
    pub public: bool,
    /// What it is.
    pub description: String,
    /// The role everyone who joins from the directory gets.
    pub default_role: RoomRole,
    /// Making it public moved it from this computer onto the person's host.
    pub moved: bool,
}

impl Publication {
    /// A workspace nobody lists, in `room`.
    #[must_use]
    pub fn private(room: RoomId) -> Self {
        Self {
            room,
            public: false,
            description: String::new(),
            default_role: RoomRole::Commenter,
            moved: false,
        }
    }
}

const fn role_word(role: RoomRole) -> &'static str {
    match role {
        RoomRole::Viewer => "viewer",
        RoomRole::Commenter => "commenter",
        RoomRole::Editor => "editor",
        RoomRole::Moderator => "moderator",
    }
}

/// What the room's `publish`, `unpublish` or `publication` answered.
fn publication_of(value: &serde_json::Value, room: RoomId) -> Result<Publication, String> {
    let public = value
        .get("public")
        .and_then(serde_json::Value::as_bool)
        .ok_or("the host's answer says nothing about the workspace being public")?;
    let default_role = match value
        .get("default_role")
        .and_then(serde_json::Value::as_str)
    {
        Some("viewer") => RoomRole::Viewer,
        Some("editor") => RoomRole::Editor,
        _ => RoomRole::Commenter,
    };
    Ok(Publication {
        room,
        public,
        moved: false,
        description: value
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        default_role,
    })
}

/// What a person sees about a workspace someone else runs.
const NOT_YOURS: &str = "Only the person who runs this workspace can make it public.";

impl ShellNetwork {
    /// The public workspaces the sites this computer knows list, in the
    /// order the sheet shows them; with `typed`, a site the person typed is
    /// asked too. A site out of reach is left out quietly, as long as any
    /// answered. Blocks on the sites: call it off the frame loop.
    pub(crate) fn directory(&self, typed: Option<&str>) -> Result<Vec<DirectoryListing>, String> {
        let runtime = self.runtime_handle()?;
        let mut sites = Vec::new();
        if let Some(typed) = typed.map(str::trim).filter(|typed| !typed.is_empty()) {
            sites.push(hosted_sites::site_entry(typed).map_err(|error| error.to_string())?);
        }
        if let Some(community) = super::configured_community_site() {
            sites.push(community);
        }
        let mut listings = Vec::new();
        let mut failures = Vec::new();
        let mut directories = std::collections::BTreeSet::new();
        for site in &sites {
            match runtime.block_on(hosted_sites::read_site(site)) {
                Ok(read) => {
                    let mut listed = Vec::new();
                    if let Some(directory) = &read.directory {
                        directories.insert(directory.to_string());
                        match runtime.block_on(hosted_sites::fetch_directory(directory)) {
                            Ok(page) => listed = listings_of(page, directory, Some(site)),
                            Err(error) => failures.push(error.to_string()),
                        }
                    }
                    if let Some(home) = &read.home
                        && !listed.iter().any(|listing| listing.entry.community)
                    {
                        listed.insert(0, home_listing(home));
                    }
                    listings.extend(listed);
                }
                Err(error) => failures.push(error.to_string()),
            }
        }
        // The hosts of the workspaces held through the web list theirs too.
        let origins: std::collections::BTreeSet<String> = self
            .hosted_sites
            .rooms()
            .into_iter()
            .filter_map(|room| self.hosted_sites.origin(room))
            .collect();
        for directory in origins.iter().filter_map(|origin| relay_directory(origin)) {
            if !directories.insert(directory.to_string()) {
                continue;
            }
            if let Ok(page) = runtime.block_on(hosted_sites::fetch_directory(&directory)) {
                listings.extend(listings_of(page, &directory, None));
            }
        }
        if listings.is_empty() && !failures.is_empty() {
            return Err(failures.join("; "));
        }
        for listing in &mut listings {
            listing.joined = match (listing.room, &listing.site) {
                (Some(room), _) => self.holds_room(room),
                (None, Some(site)) => self.holds_site(site),
                (None, None) => false,
            };
        }
        Ok(ordered(listings, tilo_clock::unix_now_ms()))
    }

    /// Join the public workspace `listing` lists, as this computer's
    /// person, with the role its owner set: through its door, then as any
    /// workspace held through the web. The site's own workspace is joined
    /// as its home, even after the person left it. Blocks on the host: call
    /// it off the frame loop.
    ///
    /// # Errors
    ///
    /// Why it was not joined, in the person's words.
    pub(crate) fn join_listed(&self, listing: &DirectoryListing) -> Result<SiteJoin, String> {
        if let Some(site) = listing.site.as_ref().filter(|_| listing.entry.community) {
            return self.rejoin_site(site).map_err(|error| error.to_string());
        }
        let door = listing
            .door
            .clone()
            .ok_or_else(|| SiteError::InvitationOnly(listing.entry.name.clone()).to_string())?;
        let workspace = SiteWorkspace {
            // The door is the listing's address: the workspace is
            // remembered by it, and leaving it is remembered by it too.
            entry: door.clone(),
            name: listing.entry.name.clone(),
            origin: door.origin().ascii_serialization(),
            admission: Some(door),
            directory: None,
            app: None,
        };
        self.join_site_workspace(&workspace, true)
            .map_err(|error| error.to_string())
    }

    /// Report the public workspace `room`, held through its door, to the
    /// people who run it.
    pub(crate) fn report_workspace(&self, room: RoomId, reason: &str) -> Result<(), String> {
        let door = self
            .site_of_room(room)
            .filter(|site| site.path().contains("/directory/"))
            .ok_or("only a workspace joined from a directory can be reported to its host")?;
        let (user, _) = self.site_person().map_err(|error| error.to_string())?;
        let runtime = self.runtime_handle()?;
        runtime
            .block_on(hosted_sites::report_at_door(&door, user, reason))
            .map_err(|error| error.to_string())
    }

    /// Whether `room` is public, and with what, as the host it lives on
    /// answers.
    pub(crate) fn publication(&self, room: RoomId) -> Result<Publication, String> {
        let answer = match self.administered_room(room) {
            Some(held) => self
                .govern(&held, "publication", serde_json::Map::new())
                .and_then(|answer| publication_of(&answer, held.room())),
            None => Err(NOT_YOURS.into()),
        };
        self.note_publication(room, &answer);
        answer
    }

    /// Make `room` public with `listing`, or private again with `None`, on
    /// the host it lives on. Blocks on the host: call it off the frame
    /// loop.
    pub(crate) fn publish(
        &self,
        room: RoomId,
        listing: Option<Listing>,
    ) -> Result<Publication, String> {
        let listing = listing
            .map(|listing| listing.checked().map_err(|error| error.to_string()))
            .transpose()?;
        let held = self.administered_room(room).ok_or(NOT_YOURS)?;
        let answer = match listing {
            Some(listing) => {
                let mut arguments = serde_json::Map::new();
                arguments.insert(
                    "default_role".into(),
                    role_word(listing.default_role).into(),
                );
                arguments.insert("description".into(), listing.description.into());
                self.govern(&held, "publish", arguments)?
            }
            None => self.govern(&held, "unpublish", serde_json::Map::new())?,
        };
        let answer = publication_of(&answer, held.room());
        if answer.is_ok() {
            self.note_publication(room, &answer);
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::directory::Face;

    fn entry(name: &str, room: RoomId) -> DirectoryEntry {
        DirectoryEntry {
            room: room.to_string(),
            name: name.into(),
            description: format!("All about {name}"),
            default_role: RoomRole::Commenter,
            door: tilo_relay_protocol::directory::door_path(&room.to_string()),
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
        let room = RoomId::new();
        let listed = listings_of(
            DirectoryPage {
                workspaces: vec![entry("Coffee", room)],
            },
            &directory(),
            None,
        );
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].room, Some(room));
        assert_eq!(listed[0].host, "eu1.tilo.coffee");
        assert_eq!(
            listed[0].door.as_ref().map(url::Url::as_str),
            Some(format!("https://eu1.tilo.coffee/peer/v1/directory/{room}/").as_str())
        );
        assert_eq!(listed[0].site, None, "a host's listing is no site's home");

        // A site's room service lists its own workspace with its door
        // beside the directory, and the site it belongs to.
        let site = url::Url::parse("https://tilo.coffee/").unwrap();
        let mut own = entry("tilo.coffee", RoomId::new());
        own.door = "./".into();
        own.community = true;
        let listed = listings_of(
            DirectoryPage {
                workspaces: vec![own],
            },
            &url::Url::parse("https://rooms.tilo.coffee/directory").unwrap(),
            Some(&site),
        );
        assert_eq!(
            listed[0].door.as_ref().map(url::Url::as_str),
            Some("https://rooms.tilo.coffee/")
        );
        assert_eq!(listed[0].site.as_ref(), Some(&site));

        // A door that is not a web address is no door.
        let mut odd = entry("Odd", RoomId::new());
        odd.door = "ftp://example.org/".into();
        let listed = listings_of(
            DirectoryPage {
                workspaces: vec![odd],
            },
            &directory(),
            None,
        );
        assert_eq!(listed[0].door, None);
    }

    #[test]
    fn the_community_comes_first_then_the_liveliest_then_by_name() {
        let now = 10 * 24 * 60 * 60 * 1000;
        let mut quiet = entry("Bakery", RoomId::new());
        quiet.last_active_ms = Some(now - 5 * 24 * 60 * 60 * 1000);
        let mut busy = entry("Garden", RoomId::new());
        busy.here_count = 2;
        busy.here = vec![Face {
            user_public_key: hex::encode([1; 32]),
            name: "Ana".into(),
        }];
        let mut busier = entry("Zoo", RoomId::new());
        busier.here_count = 9;
        let recent = {
            let mut recent = entry("Atelier", RoomId::new());
            recent.last_active_ms = Some(now - 60_000);
            recent
        };
        let silent = entry("archive", RoomId::new());
        let mut community = entry("tilo.coffee", RoomId::new());
        community.community = true;
        let page = DirectoryPage {
            workspaces: vec![quiet, busy, silent, community, busier, recent],
        };
        let shown: Vec<String> = ordered(listings_of(page, &directory(), None), now)
            .into_iter()
            .map(|listing| listing.entry.name)
            .collect();
        assert_eq!(
            shown,
            [
                "tilo.coffee",
                "Zoo",
                "Garden",
                "Atelier",
                "Bakery",
                "archive"
            ]
        );
    }

    #[test]
    fn a_workspace_two_directories_list_is_shown_once() {
        let room = RoomId::new();
        let mut first = listings_of(
            DirectoryPage {
                workspaces: vec![entry("Coffee", room)],
            },
            &directory(),
            None,
        );
        first.extend(listings_of(
            DirectoryPage {
                workspaces: vec![entry("Coffee again", room)],
            },
            &url::Url::parse("https://other.example/peer/v1/directory").unwrap(),
            None,
        ));
        let shown = ordered(first, 0);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].entry.name, "Coffee");
    }

    #[test]
    fn search_reads_name_description_and_host() {
        let listed = listings_of(
            DirectoryPage {
                workspaces: vec![entry("Rust Club", RoomId::new())],
            },
            &directory(),
            None,
        )
        .remove(0);
        assert!(listed.matches(""));
        assert!(listed.matches("  rust "));
        assert!(listed.matches("ALL ABOUT"));
        assert!(listed.matches("tilo.coffee"), "the host it is listed by");
        assert!(!listed.matches("python"));
    }

    #[test]
    fn a_site_without_a_directory_still_lists_its_home() {
        let home = SiteWorkspace {
            entry: url::Url::parse("https://tilo.coffee/").unwrap(),
            name: "tilo.coffee".into(),
            origin: "https://rooms.tilo.coffee".into(),
            admission: Some(url::Url::parse("https://rooms.tilo.coffee/").unwrap()),
            directory: None,
            app: None,
        };
        let listed = home_listing(&home);
        assert!(listed.entry.community);
        assert_eq!(listed.host, "tilo.coffee");
        assert_eq!(listed.site, Some(home.entry.clone()));
        assert_eq!(listed.door, home.admission);
    }

    #[test]
    fn the_hosts_answer_reads_as_a_publication() {
        let room = RoomId::new();
        assert_eq!(
            publication_of(
                &serde_json::json!({
                    "public": true, "description": "Coffee", "default_role": "viewer"
                }),
                room
            ),
            Ok(Publication {
                room,
                public: true,
                description: "Coffee".into(),
                default_role: RoomRole::Viewer,
                moved: false,
            })
        );
        assert_eq!(
            publication_of(&serde_json::json!({ "public": false }), room),
            Ok(Publication::private(room))
        );
        assert!(publication_of(&serde_json::json!({}), room).is_err());
        assert_eq!(
            relay_directory("https://eu1.tilo.coffee").map(String::from),
            Some("https://eu1.tilo.coffee/peer/v1/directory".to_owned())
        );
    }
}
