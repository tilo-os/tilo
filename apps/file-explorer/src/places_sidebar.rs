//! The places list down the left of the file explorer: Favorites (which the
//! person reorders by dragging and removes with the row's action) and the
//! system locations (Recent, workspaces, cloud accounts, Root, Trash).
//!
//! The list is the shared navigation list ([`ui::widget::NavList`]) hosted in
//! the window shell's sidebar; this module only says what the destinations
//! are, which one is chosen, and what a choice, a drag or a row action does.
//! The favourites themselves live in a [`FavStore`] the app owns, so they
//! survive the rebuild of the list when the window's structure changes.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::command::{CommandSink, FileCommand, push};
use crate::favorites::Favorite;
use crate::i18n::t;
use ui::icons::Icon;
use ui::widget::{NavAction, NavDrop, NavItem, NavList, NavSection, RowStatus};

/// The section holding the person's favourites.
const FAVORITES_SECTION: &str = "favorites";
/// The section holding the fixed locations.
const LOCATIONS_SECTION: &str = "locations";
/// The section holding NTFS drives (a Windows partition, external drives).
const DRIVES_SECTION: &str = "drives";
/// Row action: unpin a favourite.
const REMOVE_ACTION: &str = "remove";

/// Shared, persistent favourites state the app owns and every freshly built
/// places list borrows.
pub struct FavState {
    pub favorites: Vec<Favorite>,
}

impl FavState {
    pub fn new(favorites: Vec<Favorite>) -> Self {
        Self { favorites }
    }
}

pub type FavStore = Rc<RefCell<FavState>>;

/// Build a shared favourites store.
pub fn new_store(favorites: Vec<Favorite>) -> FavStore {
    Rc::new(RefCell::new(FavState::new(favorites)))
}

/// The row id of a favourite pointing at `path`.
fn favorite_id(path: &Path) -> String {
    format!("fav:{}", path.display())
}

/// The row id of a fixed location at `path`.
fn location_id(path: &Path) -> String {
    format!("loc:{}", path.display())
}

/// The row id of a cloud account whose folder is not mounted: choosing it
/// opens its import browser or connection details rather than a folder.
fn account_id(id: &str) -> String {
    format!("{ACCOUNT_PREFIX}{id}")
}

/// The prefix of a row id naming an account rather than a folder.
const ACCOUNT_PREFIX: &str = "account:";
/// The prefix of a row id naming a drive: choosing it asks the shell to
/// open the drive, which asks the person first.
const DRIVE_PREFIX: &str = "drive:";

/// The folder a row id names, if it is one of ours.
fn path_of_id(id: &str) -> Option<PathBuf> {
    id.strip_prefix("fav:")
        .or_else(|| id.strip_prefix("loc:"))
        .map(PathBuf::from)
}

/// What choosing the row `id` asks of the window.
fn choose(sink: &CommandSink, id: &str) {
    if let Some(account) = id.strip_prefix(ACCOUNT_PREFIX) {
        push(sink, FileCommand::ShowRemoteAccount(account.to_owned()));
    } else if let Some(drive) = id.strip_prefix(DRIVE_PREFIX) {
        push(sink, FileCommand::OpenDrive(drive.to_owned()));
    } else if let Some(path) = path_of_id(id) {
        push(sink, FileCommand::Navigate(path));
    }
}

/// One fixed location: a place that is not the person's to reorder.
struct Location {
    icon: Icon,
    label: String,
    path: PathBuf,
    /// The account this row stands for, when its folder is not mounted: the
    /// row then opens its import browser or account state; `path` is not a folder.
    account: Option<String>,
    /// A cloud account's short state when it is not well ("Offline", "Sign
    /// in"): its second line, with a dot.
    status: Option<String>,
}

/// A cloud account as the places list shows it.
pub struct AccountPlace {
    /// The account's own id.
    pub id: String,
    pub label: String,
    /// Where its folder is mounted, when it is.
    pub mount: Option<PathBuf>,
    /// A short state ("Offline", "Sign in") when the account is not well.
    pub status: Option<String>,
}

/// An NTFS drive as the places list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrivePlace {
    /// The shell's id for the drive while it stays attached.
    pub id: String,
    pub label: String,
    /// Built into this computer, as opposed to external.
    pub internal: bool,
    /// Where it is open, when it is.
    pub open_at: Option<PathBuf>,
    /// Its second line: its size, or that it is open.
    pub detail: String,
}

/// What the places list shows for the current folder.
pub struct Places {
    favorites: Vec<Favorite>,
    locations: Vec<Location>,
    drives: Vec<DrivePlace>,
    active: PathBuf,
    active_account: Option<String>,
}

impl Places {
    pub fn new(favs: &FavStore, active: PathBuf) -> Self {
        // Recent leads the fixed places: what you used last, from any folder.
        // Then the filesystem root, then the Trash (when the platform exposes
        // one): deleting moves files there.
        let mut locations = vec![
            Location {
                icon: Icon::History,
                label: t("files-place-recent"),
                path: crate::recent::location(),
                account: None,
                status: None,
            },
            Location {
                icon: Icon::HardDrive,
                label: t("files-place-root"),
                path: crate::breadcrumbs::filesystem_root(&active),
                account: None,
                status: None,
            },
        ];
        if let Some(trash) = crate::fileops::trash_dir() {
            locations.push(Location {
                icon: Icon::Trash2,
                label: t("files-place-trash"),
                path: trash,
                account: None,
                status: None,
            });
        }
        Self {
            favorites: favs.borrow().favorites.clone(),
            locations,
            drives: Vec::new(),
            active,
            active_account: None,
        }
    }

    /// Surface the Tilo collaborative-workspace mounts as locations, above
    /// Root. Each root the FUSE driver mounts (e.g. `/var/workspace`) is a
    /// place with a cloud emblem, so the person reaches their synced
    /// workspace without typing a path.
    #[must_use]
    pub fn with_workspaces(mut self, roots: Vec<PathBuf>) -> Self {
        let mut entries: Vec<Location> = roots
            .into_iter()
            .map(|path| Location {
                icon: Icon::Cloud,
                // Shared with the breadcrumb, which names the same place.
                label: crate::model::workspace_label(&path),
                path,
                account: None,
                status: None,
            })
            .collect();
        // Workspaces lead the locations; Root (and any future devices) follow.
        entries.append(&mut self.locations);
        self.locations = entries;
        self
    }

    /// Surface cloud accounts as locations after the workspaces. A mounted
    /// account opens its folder; one that is offline, syncing or needs
    /// attention says so on its second line with a status dot, and one whose
    /// folder is not mounted opens its import browser or connection details
    /// (so import accounts and accounts needing sign-in stay visible).
    #[must_use]
    pub fn with_remote_accounts(mut self, accounts: Vec<AccountPlace>) -> Self {
        let at = self
            .locations
            .iter()
            .take_while(|place| place.icon == Icon::Cloud)
            .count();
        for (offset, account) in accounts.into_iter().enumerate() {
            let unmounted = account.mount.is_none();
            self.locations.insert(
                at.saturating_add(offset).min(self.locations.len()),
                Location {
                    icon: Icon::Cloud,
                    label: account.label,
                    path: account.mount.unwrap_or_default(),
                    account: unmounted.then_some(account.id),
                    status: account.status,
                },
            );
        }
        self
    }

    /// Surface NTFS drives in their own section after the locations. A
    /// drive is never opened by being listed; choosing it asks the shell.
    #[must_use]
    pub fn with_drives(mut self, drives: Vec<DrivePlace>) -> Self {
        self.drives = drives;
        self
    }

    /// Select an unmounted account's browser instead of the last local folder.
    #[must_use]
    pub fn with_active_account(mut self, account: Option<String>) -> Self {
        self.active_account = account;
        self
    }

    /// The row of the chosen place: the favourite for the current folder,
    /// else the fixed location that is it.
    pub fn selected_id(&self) -> Option<String> {
        if let Some(account) = &self.active_account {
            return self
                .locations
                .iter()
                .find(|place| place.account.as_ref() == Some(account))
                .map(|_| account_id(account));
        }
        self.favorites
            .iter()
            .find(|favorite| favorite.path == self.active)
            .map(|favorite| favorite_id(&favorite.path))
            .or_else(|| {
                self.locations
                    .iter()
                    .find(|place| place.account.is_none() && place.path == self.active)
                    .map(|place| location_id(&place.path))
            })
            .or_else(|| {
                self.drives
                    .iter()
                    .find(|drive| drive.open_at.as_deref() == Some(self.active.as_path()))
                    .map(|drive| format!("{DRIVE_PREFIX}{}", drive.id))
            })
    }

    /// The destinations, as the shared list shows them.
    pub fn sections(&self) -> Vec<NavSection> {
        let mut sections = Vec::new();
        if !self.favorites.is_empty() {
            let items = self
                .favorites
                .iter()
                .map(|favorite| {
                    let item = NavItem::new(favorite_id(&favorite.path), favorite.label.clone())
                        .icon(favorite.icon());
                    // Home is always there; every other favourite can be
                    // unpinned from its row.
                    if favorite.is_home {
                        item.without_actions()
                    } else {
                        item
                    }
                })
                .collect();
            sections.push(NavSection::new(
                FAVORITES_SECTION,
                t("files-place-favorites"),
                items,
            ));
        }
        let items = self
            .locations
            .iter()
            .map(|place| {
                let id = match &place.account {
                    Some(account) => account_id(account),
                    None => location_id(&place.path),
                };
                let item = NavItem::new(id, place.label.clone())
                    .icon(place.icon)
                    .without_actions();
                match place.status.as_deref() {
                    None => item,
                    Some(status) => {
                        let dot = match status {
                            status if status == t("files-account-import-only") => RowStatus::Idle,
                            "Offline" => RowStatus::Idle,
                            "Syncing" => RowStatus::Active,
                            _ => RowStatus::Warning,
                        };
                        item.secondary(status).status(dot)
                    }
                }
            })
            .collect();
        sections.push(NavSection::new(
            LOCATIONS_SECTION,
            t("files-place-system"),
            items,
        ));
        if !self.drives.is_empty() {
            let items = self
                .drives
                .iter()
                .map(|drive| {
                    let item =
                        NavItem::new(format!("{DRIVE_PREFIX}{}", drive.id), drive.label.clone())
                            .icon(if drive.internal {
                                Icon::HardDrive
                            } else {
                                Icon::Usb
                            })
                            .secondary(drive.detail.clone())
                            .without_actions();
                    if drive.open_at.is_some() {
                        item.status(RowStatus::Active)
                    } else {
                        item
                    }
                })
                .collect();
            sections.push(NavSection::new(
                DRIVES_SECTION,
                t("files-place-drives"),
                items,
            ));
        }
        sections
    }

    /// The last favourite and the one it would be dragged in front of, for a
    /// capture of a reorder in progress.
    pub fn favorite_drag(&self) -> Option<(String, String)> {
        let movable: Vec<&Favorite> = self.favorites.iter().filter(|f| !f.is_home).collect();
        let (first, last) = (movable.first()?, movable.last()?);
        (movable.len() > 1).then(|| (favorite_id(&last.path), favorite_id(&first.path)))
    }

    /// The favourite move a drop asks for: `(from, to)` indices into the
    /// favourites, or `None` when the drop is not a reorder of two favourites.
    pub fn reorder_for(&self, source: &str, drop: &NavDrop) -> Option<(usize, usize)> {
        let index_of = |id: &str| {
            self.favorites
                .iter()
                .position(|favorite| favorite_id(&favorite.path) == id)
        };
        let from = index_of(source)?;
        let (before, target) = match drop {
            NavDrop::Before(id) => (true, index_of(id)?),
            NavDrop::After(id) => (false, index_of(id)?),
            NavDrop::Into(_) => return None,
        };
        // The target index is read before the dragged row leaves its slot,
        // so a move down lands one place earlier than the slot named.
        let slot = if before { target } else { target + 1 };
        let to = if from < slot { slot - 1 } else { slot };
        (from != to).then_some((from, to))
    }

    /// The navigation list: choosing a place opens it, dragging a favourite
    /// reorders them, and a favourite's row action unpins it. The row the
    /// person chooses in the list is also left in `chosen`.
    pub fn nav_list(self, sink: &CommandSink, chosen: &Rc<RefCell<Option<String>>>) -> NavList {
        let selected = self.selected_id();
        let sections = self.sections();
        let places = Rc::new(self);
        let mut list = NavList::new(sections)
            .with_id("places_sidebar")
            .with_accessible_name(t("files-a11y-places"))
            .on_select({
                let sink = sink.clone();
                let chosen = Rc::clone(chosen);
                move |id| {
                    choose(&sink, &id);
                    *chosen.borrow_mut() = Some(id);
                }
            })
            .item_actions(
                vec![NavAction::new(
                    REMOVE_ACTION,
                    Icon::X,
                    t("files-menu-remove-favourite"),
                )],
                {
                    let sink = sink.clone();
                    move |id, action| {
                        if action == REMOVE_ACTION
                            && let Some(path) = path_of_id(id)
                        {
                            push(&sink, FileCommand::RemoveFavorite(path));
                        }
                    }
                },
            )
            .on_move({
                let sink = sink.clone();
                let places = Rc::clone(&places);
                move |source, drop| {
                    if let Some((from, to)) = places.reorder_for(source, &drop) {
                        push(&sink, FileCommand::ReorderFavorite { from, to });
                    }
                }
            });
        if let Some(id) = selected {
            list = list.selected(&id);
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::new_sink;
    use crate::favorites::FavKind;

    fn favorite(label: &str, path: &str, kind: FavKind) -> Favorite {
        Favorite {
            label: label.into(),
            path: PathBuf::from(path),
            is_home: kind == FavKind::Home,
            kind,
        }
    }

    fn sample() -> Places {
        let store = new_store(vec![
            favorite("Home", "/home/u", FavKind::Home),
            favorite("Documents", "/home/u/Documents", FavKind::Documents),
            favorite("Pictures", "/home/u/Pictures", FavKind::Pictures),
            favorite("Projects", "/home/u/Projects", FavKind::Folder),
        ]);
        Places::new(&store, PathBuf::from("/home/u/Pictures"))
    }

    fn ids(section: &NavSection) -> Vec<String> {
        section.items.iter().map(|item| item.id.clone()).collect()
    }

    /// The favourites and the fixed locations are two sections, and the
    /// current folder is the chosen row.
    #[test]
    fn favourites_and_locations_are_sections_with_the_current_folder_chosen() {
        let places = sample();
        let sections = places.sections();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, t("files-place-favorites"));
        assert_eq!(
            ids(&sections[0]),
            [
                "fav:/home/u",
                "fav:/home/u/Documents",
                "fav:/home/u/Pictures",
                "fav:/home/u/Projects"
            ]
        );
        assert_eq!(
            places.selected_id().as_deref(),
            Some("fav:/home/u/Pictures")
        );
        assert!(
            sections[1]
                .items
                .iter()
                .any(|item| item.id == format!("loc:{}", crate::recent::location().display()))
        );
    }

    /// Without favourites there is no empty "Favorites" header.
    #[test]
    fn no_favourites_means_no_favourites_section() {
        let places = Places::new(&new_store(Vec::new()), PathBuf::from("/"));
        let sections = places.sections();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].id, LOCATIONS_SECTION);
    }

    #[test]
    fn unmounted_import_account_has_an_account_navigation_row() {
        let places =
            Places::new(&new_store(Vec::new()), PathBuf::from("/")).with_remote_accounts(vec![
                AccountPlace {
                    id: "import-cloud".into(),
                    label: "Import cloud".into(),
                    mount: None,
                    status: None,
                },
            ]);
        let sections = places.sections();
        let row = sections
            .iter()
            .flat_map(|section| &section.items)
            .find(|item| item.id == "account:import-cloud")
            .expect("imports remain navigable without a mount");
        assert!(row.status.is_none());
        assert!(path_of_id(&row.id).is_none());
    }

    #[test]
    fn import_browser_selects_account_instead_of_home() {
        let store = new_store(vec![favorite("Home", "/home/u", FavKind::Home)]);
        let places = Places::new(&store, PathBuf::from("/home/u")).with_remote_accounts(vec![
            AccountPlace {
                id: "drive".into(),
                label: "Google Drive".into(),
                mount: None,
                status: None,
            },
        ]);
        assert_eq!(places.selected_id().as_deref(), Some("fav:/home/u"));
        let places = places.with_active_account(Some("drive".into()));
        assert_eq!(places.selected_id().as_deref(), Some("account:drive"));
        assert_eq!(
            places.with_active_account(None).selected_id().as_deref(),
            Some("fav:/home/u")
        );
    }

    #[test]
    fn synced_account_keeps_its_folder_selection() {
        let places = Places::new(&new_store(Vec::new()), PathBuf::from("/home/u/Drive"))
            .with_remote_accounts(vec![AccountPlace {
                id: "drive".into(),
                label: "Google Drive".into(),
                mount: Some(PathBuf::from("/home/u/Drive")),
                status: None,
            }]);
        assert_eq!(places.selected_id().as_deref(), Some("loc:/home/u/Drive"));
    }

    /// Workspaces lead the locations; mounted cloud accounts follow them and
    /// say when they are offline or need attention.
    #[test]
    fn workspaces_and_cloud_accounts_are_locations_with_a_status_icon() {
        let places = Places::new(&new_store(Vec::new()), PathBuf::from("/"))
            .with_workspaces(vec![PathBuf::from("/var/workspace")])
            .with_remote_accounts(vec![
                AccountPlace {
                    id: "cloud".into(),
                    label: "Cloud".into(),
                    mount: Some(PathBuf::from("/home/a/Remote/Cloud")),
                    status: None,
                },
                AccountPlace {
                    id: "work".into(),
                    label: "Work".into(),
                    mount: Some(PathBuf::from("/home/a/Remote/Work")),
                    status: Some("Offline".into()),
                },
                AccountPlace {
                    id: "mail".into(),
                    label: "Mail".into(),
                    mount: Some(PathBuf::from("/home/a/Remote/Mail")),
                    status: Some("Sign in".into()),
                },
                AccountPlace {
                    id: "archive".into(),
                    label: "Archive".into(),
                    mount: None,
                    status: Some("Sign in".into()),
                },
            ]);
        let locations = places
            .sections()
            .pop()
            .expect("the locations section is always present");
        let order = ids(&locations);
        assert_eq!(order[0], "loc:/var/workspace");
        assert_eq!(order[1], "loc:/home/a/Remote/Cloud");
        assert_eq!(order[2], "loc:/home/a/Remote/Work");
        assert_eq!(order[3], "loc:/home/a/Remote/Mail");
        let item = |id: &str| {
            locations
                .items
                .iter()
                .find(|i| i.id == id)
                .expect("the account is listed")
        };
        // Every account is a cloud; one that is not well says so on its
        // second line, with a dot that needs the person only when it does.
        for id in [
            "loc:/home/a/Remote/Cloud",
            "loc:/home/a/Remote/Work",
            "loc:/home/a/Remote/Mail",
        ] {
            assert_eq!(item(id).icon, Some(Icon::Cloud), "{id}");
        }
        assert!(item("loc:/home/a/Remote/Cloud").status.is_none());
        assert_eq!(
            item("loc:/home/a/Remote/Work").secondary.as_deref(),
            Some("Offline")
        );
        assert_eq!(
            item("loc:/home/a/Remote/Work")
                .status
                .as_ref()
                .map(|s| s.row_status()),
            Some(RowStatus::Idle)
        );
        assert_eq!(
            item("loc:/home/a/Remote/Mail")
                .status
                .as_ref()
                .map(|s| s.row_status()),
            Some(RowStatus::Warning)
        );
        // An account with no mounted folder is still a row: it opens the
        // account's state instead of a folder.
        assert_eq!(order[4], "account:archive");
        assert_eq!(
            item("account:archive").secondary.as_deref(),
            Some("Sign in")
        );
    }

    /// Home cannot be unpinned; other favourites can, fixed locations cannot.
    #[test]
    fn only_removable_favourites_offer_the_unpin_action() {
        let sections = sample().sections();
        assert!(!sections[0].items[0].actions, "Home stays");
        assert!(sections[0].items[1].actions);
        assert!(sections[1].items.iter().all(|item| !item.actions));
    }

    /// A drop before or after a favourite becomes the index move
    /// `favorites::reorder` makes; anything involving a location is not a move.
    #[test]
    fn drops_map_to_favourite_reorders() {
        let places = sample();
        let fav = |name: &str| format!("fav:/home/u/{name}");
        // Pictures (2) dropped before Documents (1) moves up one.
        assert_eq!(
            places.reorder_for(&fav("Pictures"), &NavDrop::Before(fav("Documents"))),
            Some((2, 1))
        );
        // Documents (1) dropped after Projects (3) moves to the end.
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::After(fav("Projects"))),
            Some((1, 3))
        );
        // Documents dropped before Pictures is where it already is.
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::Before(fav("Pictures"))),
            None
        );
        // A location is neither a source nor a target.
        assert_eq!(
            places.reorder_for("loc:/", &NavDrop::Before(fav("Documents"))),
            None
        );
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::Before("loc:/".into())),
            None
        );
        // Into is not a reorder.
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::Into(fav("Projects"))),
            None
        );
    }

    /// The reorder the list reports is exactly what the store applies.
    #[test]
    fn a_reported_move_reorders_the_stored_favourites() {
        let store = new_store(vec![
            favorite("Home", "/home/u", FavKind::Home),
            favorite("Documents", "/home/u/Documents", FavKind::Documents),
            favorite("Pictures", "/home/u/Pictures", FavKind::Pictures),
        ]);
        let places = Places::new(&store, PathBuf::from("/home/u"));
        let (from, to) = places
            .reorder_for(
                "fav:/home/u/Documents",
                &NavDrop::After("fav:/home/u/Pictures".into()),
            )
            .expect("moving down is a reorder");
        crate::favorites::reorder(&mut store.borrow_mut().favorites, from, to);
        let order: Vec<_> = store
            .borrow()
            .favorites
            .iter()
            .map(|favorite| favorite.label.clone())
            .collect();
        assert_eq!(order, ["Home", "Pictures", "Documents"]);
    }

    /// Row ids round-trip to the folder they name and nothing else.
    #[test]
    fn ids_name_their_folder() {
        assert_eq!(
            path_of_id(&favorite_id(Path::new("/a/b"))),
            Some(PathBuf::from("/a/b"))
        );
        assert_eq!(
            path_of_id(&location_id(Path::new("/"))),
            Some(PathBuf::from("/"))
        );
        assert_eq!(path_of_id("other:/a"), None);
    }

    fn drive(id: &str, internal: bool, open_at: Option<&str>) -> DrivePlace {
        DrivePlace {
            id: id.into(),
            label: if internal { "Windows" } else { "Stick" }.into(),
            internal,
            open_at: open_at.map(PathBuf::from),
            detail: "64 GB".into(),
        }
    }

    /// Drives are their own section after the locations: internal ones with
    /// a disk icon, external ones with a USB icon, an open one with a dot and
    /// selected while its folder is shown.
    #[test]
    fn drives_are_a_section_and_an_open_one_is_chosen_while_shown() {
        let none = Places::new(&new_store(Vec::new()), PathBuf::from("/"));
        assert!(
            none.sections()
                .iter()
                .all(|section| section.id != DRIVES_SECTION)
        );

        let places = Places::new(&new_store(Vec::new()), PathBuf::from("/run/media/u/Stick"))
            .with_drives(vec![
                drive("nvme0n1p3:1", true, None),
                drive("sdb1:7", false, Some("/run/media/u/Stick")),
            ]);
        let sections = places.sections();
        let drives = sections.last().unwrap();
        assert_eq!(drives.id, DRIVES_SECTION);
        assert_eq!(ids(drives), ["drive:nvme0n1p3:1", "drive:sdb1:7"]);
        assert_eq!(drives.items[0].icon, Some(Icon::HardDrive));
        assert_eq!(drives.items[1].icon, Some(Icon::Usb));
        assert!(drives.items[0].status.is_none());
        assert_eq!(
            drives.items[1].status.as_ref().map(|s| s.row_status()),
            Some(RowStatus::Active)
        );
        assert_eq!(places.selected_id().as_deref(), Some("drive:sdb1:7"));
        assert!(
            path_of_id("drive:sdb1:7").is_none(),
            "a drive is not a folder row"
        );
    }

    /// Choosing a drive asks to open it; it never navigates by itself.
    #[test]
    fn choosing_a_drive_asks_to_open_it() {
        let sink = new_sink();
        let places = Places::new(&new_store(Vec::new()), PathBuf::from("/"))
            .with_drives(vec![drive("nvme0n1p3:1", true, None)]);
        let id = ids(places.sections().last().unwrap())[0].clone();
        choose(&sink, &id);
        let commands = sink.borrow();
        assert!(
            matches!(commands.as_slice(), [FileCommand::OpenDrive(id)] if id == "nvme0n1p3:1"),
            "{commands:?}"
        );
    }

    /// The list built from the places carries the chosen row and its name.
    #[test]
    fn the_list_shows_the_chosen_place() {
        let sink = new_sink();
        let list = sample().nav_list(&sink, &Rc::new(RefCell::new(None)));
        assert_eq!(list.selected_id().as_deref(), Some("fav:/home/u/Pictures"));
    }
}
