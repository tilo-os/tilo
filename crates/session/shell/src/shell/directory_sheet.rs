//! "Join a workspace…": the public workspaces a person can join.
//!
//! The sheet itself is an app ([`tilo_sheets::directory`]); this is what the
//! shell holds for it: what the sites this computer knows serve in their
//! directories ([`crate::shell::network::DirectoryListing`]), where the
//! joins of this session stand, and the [`DirectoryRequest`]s the person's
//! choices become, which the host carries out off the frame loop and
//! answers ([`DirectorySheet::show`], [`DirectorySheet::joined`],
//! [`DirectorySheet::failed`]).

use tilo_sheets::directory::{Directory, DirectoryEffect, Listing, ListingAction, Status};

use crate::shell::i18n::{t, t_args};
use crate::shell::network::join_field::{self, JoinText};
use crate::shell::network::{DirectoryListing, JoinProgress, SiteJoinState};
use tilo_relay_protocol::directory::Activity;
use tilo_relay_protocol::rooms::RoomRole;

/// What the host carries out for the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryRequest {
    /// Read the directories again; with `site`, that site's too.
    Fetch { site: Option<String> },
    /// Join this listing, with the role its owner set.
    Join(Box<DirectoryListing>),
    /// Switch to this listing's workspace, which this computer holds.
    Open(Box<DirectoryListing>),
    /// Open the join field with this text (a link, a code, or nothing).
    LinkOrCode(String),
    /// Take the sheet down.
    Close,
}

#[derive(Default)]
pub struct DirectorySheet {
    open: bool,
    listings: Vec<DirectoryListing>,
    status: Status,
    /// The listing being joined, by its key.
    joining: Option<String>,
    /// What became of the last join or read, when it went wrong.
    note: Option<String>,
    /// Where the joins this session runs stand, as the network last said:
    /// a listing joining shows it, one that failed says why.
    joins: Vec<SiteJoinState>,
}

/// The key a listing is joined and found again by.
fn key_of(listing: &DirectoryListing) -> String {
    listing.room.map_or_else(
        || {
            listing
                .site
                .as_ref()
                .map_or_else(|| listing.entry.name.clone(), url::Url::to_string)
        },
        |room| room.to_string(),
    )
}

/// The words a role is shown with.
fn role_label(role: RoomRole) -> String {
    t(match role {
        RoomRole::Viewer => "directory-role-viewer",
        RoomRole::Commenter => "directory-role-commenter",
        RoomRole::Editor => "directory-role-editor",
        RoomRole::Moderator => "directory-role-moderator",
    })
}

/// How lively a listing is, and how many belong to it, in a line.
pub(crate) fn activity_line(listing: &DirectoryListing, now_ms: u64) -> String {
    let activity = match listing.activity(now_ms) {
        Activity::HereNow(count) => t_args("directory-here-now", &[("count", count.into())]),
        Activity::ActiveToday => t("directory-active-today"),
        Activity::ActiveThisWeek => t("directory-active-week"),
        Activity::Quiet => t("directory-quiet"),
    };
    match listing.entry.members {
        0 => activity,
        members => format!(
            "{activity} · {}",
            t_args("directory-members", &[("count", members.into())])
        ),
    }
}

/// The role people who join `listing` get, as its button's description
/// says it.
fn joins_as(listing: &DirectoryListing) -> String {
    t_args(
        "directory-joins-as",
        &[("role", role_label(listing.entry.default_role).into())],
    )
}

/// What Enter does with what was typed into the search field: a link or an
/// invitation goes to the join field, a site is looked up; a word is a
/// search.
fn typed_request(query: &str) -> Option<DirectoryRequest> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let parsed = join_field::classify(query).ok();
    let link_or_code = matches!(
        parsed,
        Some(JoinText::InvitationLink { .. } | JoinText::Site { room: Some(_), .. })
    ) || (matches!(parsed, Some(JoinText::Invitation(_)))
        && join_field::is_invitation(query));
    if link_or_code {
        return Some(DirectoryRequest::LinkOrCode(query.to_owned()));
    }
    (matches!(parsed, Some(JoinText::Site { room: None, .. })) && query.contains('.')).then(|| {
        DirectoryRequest::Fetch {
            site: Some(query.to_owned()),
        }
    })
}

impl DirectorySheet {
    /// Open the sheet, asking the directories afresh.
    pub fn open(&mut self) -> DirectoryRequest {
        self.open = true;
        self.status = Status::Loading;
        self.note = None;
        self.joining = None;
        DirectoryRequest::Fetch { site: None }
    }

    /// Close it.
    pub fn close(&mut self) {
        self.open = false;
        self.joining = None;
    }

    /// Whether it is up.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Show what the directories listed.
    pub fn show(&mut self, listings: Vec<DirectoryListing>) {
        self.listings = listings;
        self.status = Status::Listed;
    }

    /// Say why the directories could not be read, or why a join failed.
    pub fn failed(&mut self, error: &str) {
        if let Some(key) = self.joining.take() {
            let name = self
                .listings
                .iter()
                .find(|listing| key_of(listing) == key)
                .map(|listing| listing.entry.name.clone())
                .unwrap_or_default();
            self.note = Some(t_args(
                "directory-join-failed",
                &[("name", name.into()), ("error", error.to_owned().into())],
            ));
            return;
        }
        if self.listings.is_empty() {
            self.status = Status::Failed(error.to_owned());
        } else {
            self.note = Some(t_args(
                "directory-failed",
                &[("error", error.to_owned().into())],
            ));
        }
    }

    /// A join finished: the sheet's work is done.
    pub fn joined(&mut self) {
        self.joining = None;
        self.close();
    }

    /// The listings that answer `query`.
    fn matching(&self, query: &str) -> Vec<&DirectoryListing> {
        self.listings
            .iter()
            .filter(|listing| listing.matches(query))
            .collect()
    }

    /// Whether `listing` is being joined: asked here, or joining in this
    /// session already.
    #[must_use]
    pub fn is_joining(&self, listing: &DirectoryListing) -> bool {
        self.joining.as_deref() == Some(key_of(listing).as_str())
            || self.progress(listing) == Some(&JoinProgress::Joining)
    }

    /// Say where the joins this session runs stand.
    pub fn set_joins(&mut self, joins: Vec<SiteJoinState>) {
        self.joins = joins;
    }

    /// Where joining `listing` stands in this session, if it does.
    #[must_use]
    pub fn progress(&self, listing: &DirectoryListing) -> Option<&JoinProgress> {
        if listing.joined {
            return None;
        }
        let site = listing.site.as_ref().map(url::Url::as_str);
        self.joins
            .iter()
            .find(|join| {
                listing.room == Some(join.room) || (site.is_some() && join.site.as_deref() == site)
            })
            .map(|join| &join.progress)
    }

    /// Why joining `listing` failed last, if it did.
    fn failure(&self, listing: &DirectoryListing) -> Option<&str> {
        match self.progress(listing) {
            Some(JoinProgress::Failed(reason)) => Some(reason),
            _ => None,
        }
    }

    /// What pressing `listing`'s button asks: Open once held, else Join,
    /// one join at a time.
    fn choose(&mut self, listing: &DirectoryListing) -> Option<DirectoryRequest> {
        if listing.joined {
            return Some(DirectoryRequest::Open(Box::new(listing.clone())));
        }
        if self.progress(listing) == Some(&JoinProgress::Joining) {
            // Joining already: say so, and that it opens by itself.
            self.note = Some(t_args(
                "directory-already-joining",
                &[("name", listing.entry.name.clone().into())],
            ));
            return None;
        }
        if self.joining.is_some() {
            // One join at a time: the first is still on its way.
            return None;
        }
        self.joining = Some(key_of(listing));
        self.note = None;
        Some(DirectoryRequest::Join(Box::new(listing.clone())))
    }

    /// What the sheet's request `effect` asks of the host, if anything.
    pub fn answer(&mut self, effect: DirectoryEffect) -> Option<DirectoryRequest> {
        match effect {
            DirectoryEffect::Join { key } | DirectoryEffect::Open { key } => {
                let listing = self
                    .listings
                    .iter()
                    .find(|listing| key_of(listing) == key)
                    .cloned()?;
                self.choose(&listing)
            }
            DirectoryEffect::Look { text } => match typed_request(&text) {
                Some(DirectoryRequest::Fetch { site }) => {
                    self.status = Status::Loading;
                    Some(DirectoryRequest::Fetch { site })
                }
                Some(request) => Some(request),
                // One workspace answers the search: Enter joins it.
                None => match self.matching(&text).as_slice() {
                    [only] => {
                        let only = (*only).clone();
                        self.choose(&only)
                    }
                    _ => None,
                },
            },
            // What was typed goes along when it is a link, a code or a site;
            // a search word does not.
            DirectoryEffect::LinkOrCode { text } => {
                let typed = text.trim().to_owned();
                Some(DirectoryRequest::LinkOrCode(
                    if typed_request(&typed).is_some() {
                        typed
                    } else {
                        String::new()
                    },
                ))
            }
            DirectoryEffect::Refresh => {
                self.status = Status::Loading;
                Some(DirectoryRequest::Fetch { site: None })
            }
            DirectoryEffect::Done => {
                self.close();
                Some(DirectoryRequest::Close)
            }
        }
    }

    /// What the sheet shows at `now_ms`.
    #[must_use]
    pub fn view(&self, now_ms: u64) -> Directory {
        let listings = self
            .listings
            .iter()
            .map(|listing| {
                let failure = self.failure(listing);
                Listing {
                    key: key_of(listing),
                    name: listing.entry.name.clone(),
                    about: if listing.entry.description.is_empty() {
                        listing.host.clone()
                    } else {
                        listing.entry.description.clone()
                    },
                    host: listing.host.clone(),
                    tag: listing.entry.community.then(|| {
                        t_args(
                            "directory-community-tag",
                            &[("host", listing.host.clone().into())],
                        )
                    }),
                    activity: failure.map_or_else(
                        || activity_line(listing, now_ms),
                        |reason| {
                            t_args(
                                "directory-join-failed-line",
                                &[("error", reason.to_owned().into())],
                            )
                        },
                    ),
                    joins_as: joins_as(listing),
                    here: listing
                        .entry
                        .here
                        .iter()
                        .map(|face| (face.name.clone(), face.user_public_key.clone()))
                        .collect(),
                    here_count: usize::try_from(listing.entry.here_count).unwrap_or(usize::MAX),
                    action: if listing.joined {
                        ListingAction::Open
                    } else if self.is_joining(listing) {
                        ListingAction::Joining
                    } else if failure.is_some() {
                        ListingAction::TryAgain
                    } else {
                        ListingAction::Join
                    },
                    waiting: self.joining.is_some() && !self.is_joining(listing) && !listing.joined,
                }
            })
            .collect();
        Directory {
            status: self.status.clone(),
            listings,
            note: self.note.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::RoomId;
    use tilo_relay_protocol::directory::DirectoryEntry;

    fn listing(name: &str, joined: bool) -> DirectoryListing {
        let room = RoomId::new();
        DirectoryListing {
            entry: DirectoryEntry {
                room: room.to_string(),
                name: name.into(),
                description: format!("{name} people"),
                default_role: RoomRole::Commenter,
                door: String::new(),
                here: Vec::new(),
                here_count: 0,
                members: 2,
                last_active_ms: None,
                community: false,
            },
            room: Some(room),
            door: url::Url::parse(&format!("https://eu1.example/peer/v1/directory/{room}/")).ok(),
            site: None,
            host: "eu1.example".into(),
            joined,
        }
    }

    fn press(sheet: &mut DirectorySheet, listing: &DirectoryListing) -> Option<DirectoryRequest> {
        sheet.answer(DirectoryEffect::Join {
            key: key_of(listing),
        })
    }

    #[test]
    fn opening_asks_the_directories_and_enter_joins_the_one_workspace_a_search_names() {
        let mut sheet = DirectorySheet::default();
        assert_eq!(sheet.open(), DirectoryRequest::Fetch { site: None });
        assert_eq!(sheet.view(0).status, Status::Loading);
        sheet.show(vec![listing("Rust Club", false), listing("Bakers", false)]);
        assert_eq!(sheet.view(0).listings.len(), 2);
        let outcome = sheet.answer(DirectoryEffect::Look {
            text: "rust".into(),
        });
        assert!(
            matches!(outcome, Some(DirectoryRequest::Join(ref joined)) if joined.entry.name == "Rust Club")
        );
        assert_eq!(sheet.view(0).listings[0].action, ListingAction::Joining);
        assert!(sheet.view(0).listings[1].waiting, "one join at a time");
    }

    #[test]
    fn a_held_workspace_opens_instead_of_joining_and_joins_go_one_at_a_time() {
        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        let (held, new, other) = (
            listing("Held", true),
            listing("New", false),
            listing("Other", false),
        );
        sheet.show(vec![held.clone(), new.clone(), other.clone()]);
        assert!(matches!(
            press(&mut sheet, &held),
            Some(DirectoryRequest::Open(_))
        ));
        assert!(matches!(
            press(&mut sheet, &new),
            Some(DirectoryRequest::Join(_))
        ));
        assert_eq!(
            press(&mut sheet, &other),
            None,
            "the first join is still on its way"
        );
        sheet.failed("the host did not answer");
        assert!(
            sheet.is_open(),
            "a failed join leaves the sheet up to try again"
        );
        assert!(sheet.view(0).note.is_some_and(|note| note.contains("New")));
        assert!(matches!(
            press(&mut sheet, &other),
            Some(DirectoryRequest::Join(_))
        ));
        sheet.joined();
        assert!(!sheet.is_open(), "a join that finished closes the sheet");
    }

    #[test]
    fn a_workspace_joining_says_so_and_pressing_join_again_says_it_is_on_its_way() {
        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        let joining = listing("Lounge", false);
        let failed = listing("Garden", false);
        sheet.show(vec![joining.clone(), failed.clone()]);
        let state = |listing: &DirectoryListing, progress| SiteJoinState {
            room: listing.room.unwrap(),
            site: None,
            workspace: None,
            progress,
        };
        sheet.set_joins(vec![
            state(&joining, JoinProgress::Joining),
            state(&failed, JoinProgress::Failed("the host refused".into())),
        ]);
        let view = sheet.view(0);
        assert_eq!(view.listings[0].action, ListingAction::Joining);
        assert_eq!(view.listings[1].action, ListingAction::TryAgain);
        assert!(view.listings[1].activity.contains("the host refused"));
        assert_eq!(
            press(&mut sheet, &joining),
            None,
            "a join already on its way is not asked again"
        );
        let note = sheet
            .view(0)
            .note
            .expect("the sheet says why nothing happened");
        assert!(note.contains("already joining Lounge"), "{note}");
        assert!(
            matches!(press(&mut sheet, &failed), Some(DirectoryRequest::Join(_))),
            "a failed join is tried again"
        );
    }

    #[test]
    fn enter_looks_up_a_typed_site_and_hands_links_and_codes_to_the_join_field() {
        assert_eq!(
            typed_request("example.org"),
            Some(DirectoryRequest::Fetch {
                site: Some("example.org".into())
            })
        );
        let room = RoomId::new();
        let link = format!("https://app.example/app/#/@rooms.example/w/{room}?invite=abc");
        assert_eq!(
            typed_request(&link),
            Some(DirectoryRequest::LinkOrCode(link.clone()))
        );
        assert_eq!(
            typed_request("rust"),
            None,
            "a word that is no invitation searches"
        );
        assert_eq!(typed_request("   "), None);

        let mut sheet = DirectorySheet::default();
        let _ = sheet.open();
        sheet.show(Vec::new());
        assert_eq!(
            sheet.answer(DirectoryEffect::LinkOrCode {
                text: "rust".into()
            }),
            Some(DirectoryRequest::LinkOrCode(String::new())),
            "a search word does not go to the join field"
        );
        assert_eq!(
            sheet.answer(DirectoryEffect::Look {
                text: "example.org".into()
            }),
            Some(DirectoryRequest::Fetch {
                site: Some("example.org".into())
            })
        );
        assert_eq!(sheet.view(0).status, Status::Loading);
    }

    #[test]
    fn the_activity_line_says_who_is_here_and_how_many_belong() {
        let mut busy = listing("Busy", false);
        busy.entry.here_count = 3;
        busy.entry.members = 12;
        let line = activity_line(&busy, 0);
        assert!(line.contains('3'), "{line}");
        assert!(line.contains("12"), "{line}");
        assert_eq!(
            line.matches(" · ").count(),
            1,
            "a row says one thing: {line}"
        );
        assert!(joins_as(&busy).contains(&role_label(RoomRole::Commenter)));
    }
}
