//! The public workspaces a site lists, the door each one admits people
//! through, and where a join stands: one implementation for the desktop
//! session and for a browser page.
//!
//! A site names its home workspace and the directory of its public ones in
//! its `entry.json`; the directory then lists every public workspace it
//! serves with what it is, who is in it now and how lively it is
//! ([`tilo_relay_protocol::directory`]). [`Store`] reads those sites and asks
//! their doors on one app task ([`Store::read`], [`Store::join`],
//! [`Store::take`]), and answers with what the sites said
//! ([`Store::answered`]) — never on the caller's loop.
//!
//! Joining one asks its door to admit a person ([`join`]) and verifies the
//! invitation the door answers with for them: the workspace that invitation
//! grants, and nothing the directory claims, is the workspace joined.
//!
//! Everything network-facing runs over [`tilo_net::Client`]: the desktop's
//! connection pool, or a page's `fetch`. The person a join is made as, the
//! community site to read and the workspaces the caller already holds are
//! supplied by the caller, so the same crate serves a session that owns an
//! identity on disk and a page that does not.

mod client;
mod store;
mod view;

use std::collections::BTreeSet;

use tilo_relay_protocol::directory::{Activity, DirectoryEntry, DirectoryPage};
use tilo_relay_protocol::workspaces::WorkspaceRole;

pub use client::{
    Granted, Person, SiteEntry, SiteWorkspace, client, fetch_directory, invited_workspace, join,
    read_site, sealed_invitation, site_entry, verify_invitation, web_origin,
};
pub use store::{Joined, Shelved, Store, Wake};
pub use view::Held;

/// One public workspace a directory lists, with where it is joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// What its directory says about it.
    pub entry: DirectoryEntry,
    /// Its door, resolved; `None` for a site's home workspace that lets
    /// people in by invitation only.
    pub door: Option<url::Url>,
    /// The site whose own workspace it is: joining it joins that site's
    /// home.
    pub site: Option<url::Url>,
    /// The host name it is listed by ("tilo.sh").
    pub host: String,
}

impl Listed {
    /// The key it is joined and found again by: its workspace, else the site
    /// whose home it is, else the name it is listed under.
    pub fn key(&self) -> String {
        if self.entry.workspace.is_empty() {
            self.site
                .as_ref()
                .map_or_else(|| self.entry.name.clone(), url::Url::to_string)
        } else {
            self.entry.workspace.clone()
        }
    }

    /// How lively it is at `now_ms`.
    fn activity(&self, now_ms: u64) -> Activity {
        self.entry.activity(now_ms)
    }
}

/// The listings of `page`, read from the directory at `directory`. `site`
/// is the site the directory belongs to, when it is a site's: its community
/// listing joins that site's home.
pub fn listings_of(
    page: DirectoryPage,
    directory: &url::Url,
    site: Option<&url::Url>,
) -> Vec<Listed> {
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
pub fn home_listing(site: &url::Url, home: &SiteWorkspace) -> Listed {
    Listed {
        entry: DirectoryEntry {
            workspace: String::new(),
            name: home.name.clone(),
            description: String::new(),
            default_role: WorkspaceRole::Editor,
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
pub fn ordered(listings: Vec<Listed>, now_ms: u64) -> Vec<Listed> {
    let mut seen = BTreeSet::new();
    let mut kept: Vec<_> = listings
        .into_iter()
        .filter(|listing| {
            if listing.entry.workspace.is_empty() {
                listing
                    .site
                    .as_ref()
                    .is_none_or(|site| seen.insert(site.to_string()))
            } else {
                seen.insert(listing.entry.workspace.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::directory::{Face, door_path};

    fn entry(name: &str) -> DirectoryEntry {
        let workspace = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
        DirectoryEntry {
            workspace: workspace.into(),
            name: name.into(),
            description: format!("All about {name}"),
            default_role: WorkspaceRole::Commenter,
            door: door_path(workspace),
            here: Vec::new(),
            here_count: 0,
            members: 3,
            last_active_ms: None,
            community: false,
        }
    }

    fn directory() -> url::Url {
        url::Url::parse("https://eu1.tilo.sh/peer/v1/directory").unwrap()
    }

    #[test]
    fn a_listings_door_is_read_against_its_directory() {
        let page = DirectoryPage {
            workspaces: vec![entry("Coffee")],
        };
        let listed = listings_of(page, &directory(), None);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].host, "eu1.tilo.sh");
        assert_eq!(
            listed[0].door.as_ref().map(url::Url::as_str),
            Some(
                format!(
                    "https://eu1.tilo.sh/peer/v1/directory/{}/",
                    listed[0].entry.workspace
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
        let workspace = |listing: &mut DirectoryEntry, index: u8| {
            listing.workspace = format!("6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b{index:02}");
        };
        let mut quiet = entry("Bakery");
        workspace(&mut quiet, 1);
        quiet.last_active_ms = Some(now - 5 * 24 * 60 * 60 * 1000);
        let mut busy = entry("Garden");
        workspace(&mut busy, 2);
        busy.here_count = 2;
        busy.here = vec![Face {
            user_public_key: hex::encode([1; 32]),
            name: "Ana".into(),
        }];
        let mut busier = entry("Zoo");
        workspace(&mut busier, 3);
        busier.here_count = 9;
        let mut recent = entry("Atelier");
        workspace(&mut recent, 4);
        recent.last_active_ms = Some(now - 60_000);
        let mut community = entry("tilo.sh");
        workspace(&mut community, 5);
        community.community = true;
        let page = DirectoryPage {
            workspaces: vec![quiet, busy, busier, recent, community],
        };
        let shown: Vec<String> = ordered(listings_of(page, &directory(), None), now)
            .into_iter()
            .map(|listing| listing.entry.name)
            .collect();
        assert_eq!(shown, ["tilo.sh", "Zoo", "Garden", "Atelier", "Bakery"]);
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
        let site = url::Url::parse("https://tilo.sh/").unwrap();
        let home = SiteWorkspace {
            entry: site.clone(),
            name: "tilo.sh".into(),
            admission: Some(url::Url::parse("https://rooms.tilo.sh/").unwrap()),
        };
        let listed = home_listing(&site, &home);
        assert!(listed.entry.community);
        assert_eq!(listed.host, "tilo.sh");
        assert_eq!(listed.site, Some(site));
        assert_eq!(listed.door, home.admission);
        assert_eq!(listed.key(), "https://tilo.sh/");
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
}
