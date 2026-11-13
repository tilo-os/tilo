//! The places list down the left: the person's favourites, then the fixed
//! locations, cloud accounts and drives. Choosing a row opens it; a favourite
//! can be dragged to reorder the list.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::command::{CommandSink, FileCommand, push};
use crate::favorites::Favorite;
use crate::i18n::t;
use ui::icons::Icon;
use ui::widget::{NavAction, NavDrop, NavItem, NavList, NavSection, RowStatus};

const FAVORITES_SECTION: &str = "favorites";
const LOCATIONS_SECTION: &str = "locations";
const DRIVES_SECTION: &str = "drives";
const REMOVE_ACTION: &str = "remove";

pub struct FavState {
    pub favorites: Vec<Favorite>,
}

impl FavState {
    pub fn new(favorites: Vec<Favorite>) -> Self {
        Self { favorites }
    }
}

pub type FavStore = Rc<RefCell<FavState>>;

pub fn new_store(favorites: Vec<Favorite>) -> FavStore {
    Rc::new(RefCell::new(FavState::new(favorites)))
}

fn favorite_id(path: &Path) -> String {
    format!("fav:{}", path.display())
}

fn location_id(path: &Path) -> String {
    format!("loc:{}", path.display())
}

fn account_id(id: &str) -> String {
    format!("{ACCOUNT_PREFIX}{id}")
}

const ACCOUNT_PREFIX: &str = "account:";
const DRIVE_PREFIX: &str = "drive:";

pub(crate) fn path_of_id(id: &str) -> Option<PathBuf> {
    id.strip_prefix("fav:")
        .or_else(|| id.strip_prefix("loc:"))
        .map(PathBuf::from)
}

fn choose(sink: &CommandSink, id: &str) {
    if let Some(account) = id.strip_prefix(ACCOUNT_PREFIX) {
        push(sink, FileCommand::ShowRemoteAccount(account.to_owned()));
    } else if let Some(drive) = id.strip_prefix(DRIVE_PREFIX) {
        push(sink, FileCommand::OpenDrive(drive.to_owned()));
    } else if let Some(path) = path_of_id(id) {
        push(sink, FileCommand::Navigate(path));
    }
}

struct Location {
    icon: Icon,
    label: String,
    path: PathBuf,
    account: Option<String>,
    status: Option<String>,
}

pub struct AccountPlace {
    pub id: String,
    pub label: String,
    pub mount: Option<PathBuf>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrivePlace {
    pub id: String,
    pub label: String,
    pub internal: bool,
    pub open_at: Option<PathBuf>,
    pub detail: String,
}

pub struct Places {
    favorites: Vec<Favorite>,
    locations: Vec<Location>,
    drives: Vec<DrivePlace>,
    active: PathBuf,
    active_account: Option<String>,
}

impl Places {
    pub fn new(favs: &FavStore, active: PathBuf) -> Self {
        let mut locations = vec![
            Location {
                icon: Icon::History,
                label: t("files-place-recent"),
                path: crate::recent::location(),
                account: None,
                status: None,
            },
            Location {
                icon: Icon::LayoutGrid,
                label: t("files-place-applications"),
                path: crate::applications::location(),
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

    #[must_use]
    pub fn with_workspaces(mut self, roots: Vec<PathBuf>) -> Self {
        let mut entries: Vec<Location> = roots
            .into_iter()
            .map(|path| Location {
                icon: Icon::Cloud,
                label: crate::model::workspace_label(&path),
                path,
                account: None,
                status: None,
            })
            .collect();
        entries.append(&mut self.locations);
        self.locations = entries;
        self
    }

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

    #[must_use]
    pub fn with_drives(mut self, drives: Vec<DrivePlace>) -> Self {
        self.drives = drives;
        self
    }

    #[must_use]
    pub fn with_active_account(mut self, account: Option<String>) -> Self {
        self.active_account = account;
        self
    }

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

    pub fn favorite_drag(&self) -> Option<(String, String)> {
        let movable: Vec<&Favorite> = self.favorites.iter().filter(|f| !f.is_home).collect();
        let (first, last) = (movable.first()?, movable.last()?);
        (movable.len() > 1).then(|| (favorite_id(&last.path), favorite_id(&first.path)))
    }

    pub fn reorder_for(&self, source: &str, drop: &NavDrop) -> Option<(usize, usize)> {
        let (before, target) = match drop {
            NavDrop::Before(id) => (true, id),
            NavDrop::After(id) => (false, id),
            NavDrop::Into(_) => return None,
        };
        crate::favorites::move_for(
            &self.favorites,
            &path_of_id(source)?,
            &path_of_id(target)?,
            before,
        )
    }

    pub fn nav_list(self, sink: &CommandSink, chosen: &Rc<RefCell<Option<String>>>) -> NavList {
        let selected = self.selected_id();
        let sections = self.sections();
        let mut list = NavList::new(sections)
            .with_accessible_name(t("files-a11y-places"))
            .on_view("places_sidebar", {
                let sink = sink.clone();
                let chosen = Rc::clone(chosen);
                move |id| {
                    choose(&sink, &id);
                    *chosen.borrow_mut() = Some(id);
                }
            });
        if let Some(binding) = crate::bindings::remove_favorite() {
            list = list.item_action(
                NavAction::new(REMOVE_ACTION, Icon::X, t("files-menu-remove-favourite")),
                binding,
            );
        }
        if let Some(binding) = crate::bindings::move_favorite() {
            list = list.on_move_action(binding);
        }
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
        assert_eq!(order[4], "account:archive");
        assert_eq!(
            item("account:archive").secondary.as_deref(),
            Some("Sign in")
        );
    }

    #[test]
    fn only_removable_favourites_offer_the_unpin_action() {
        let sections = sample().sections();
        assert!(!sections[0].items[0].actions, "Home stays");
        assert!(sections[0].items[1].actions);
        assert!(sections[1].items.iter().all(|item| !item.actions));
    }

    #[test]
    fn drops_map_to_favourite_reorders() {
        let places = sample();
        let fav = |name: &str| format!("fav:/home/u/{name}");
        assert_eq!(
            places.reorder_for(&fav("Pictures"), &NavDrop::Before(fav("Documents"))),
            Some((2, 1))
        );
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::After(fav("Projects"))),
            Some((1, 3))
        );
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::Before(fav("Pictures"))),
            None
        );
        assert_eq!(
            places.reorder_for("loc:/", &NavDrop::Before(fav("Documents"))),
            None
        );
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::Before("loc:/".into())),
            None
        );
        assert_eq!(
            places.reorder_for(&fav("Documents"), &NavDrop::Into(fav("Projects"))),
            None
        );
    }

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

    #[test]
    fn the_list_shows_the_chosen_place() {
        let sink = new_sink();
        let list = sample().nav_list(&sink, &Rc::new(RefCell::new(None)));
        assert_eq!(list.selected_id().as_deref(), Some("fav:/home/u/Pictures"));
    }
}
