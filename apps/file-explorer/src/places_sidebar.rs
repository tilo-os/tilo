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
use ui::widget::{NavAction, NavDrop, NavItem, NavList, NavSection};

/// The section holding the person's favourites.
const FAVORITES_SECTION: &str = "favorites";
/// The section holding the fixed locations.
const LOCATIONS_SECTION: &str = "locations";
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

/// The folder a row id names, if it is one of ours.
fn path_of_id(id: &str) -> Option<PathBuf> {
    id.strip_prefix("fav:")
        .or_else(|| id.strip_prefix("loc:"))
        .map(PathBuf::from)
}

/// One fixed location: a place that is not the person's to reorder.
struct Location {
    icon: Icon,
    label: String,
    path: PathBuf,
}

/// What the places list shows for the current folder.
pub struct Places {
    favorites: Vec<Favorite>,
    locations: Vec<Location>,
    active: PathBuf,
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
            },
            Location {
                icon: Icon::HardDrive,
                label: t("files-place-root"),
                path: crate::breadcrumbs::filesystem_root(&active),
            },
        ];
        if let Some(trash) = crate::fileops::trash_dir() {
            locations.push(Location {
                icon: Icon::Trash2,
                label: t("files-place-trash"),
                path: trash,
            });
        }
        Self {
            favorites: favs.borrow().favorites.clone(),
            locations,
            active,
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
                icon: Icon::CloudDownload,
                // Shared with the breadcrumb, which names the same place.
                label: crate::model::workspace_label(&path),
                path,
            })
            .collect();
        // Workspaces lead the locations; Root (and any future devices) follow.
        entries.append(&mut self.locations);
        self.locations = entries;
        self
    }

    /// Surface mounted cloud accounts (`~/Remote/<label>`) as locations after
    /// the workspaces. An account that is offline or needs attention wears
    /// the matching icon.
    #[must_use]
    pub fn with_remote_accounts(
        mut self,
        accounts: Vec<(String, PathBuf, Option<String>)>,
    ) -> Self {
        let at = self
            .locations
            .iter()
            .take_while(|place| place.icon == Icon::CloudDownload)
            .count();
        for (offset, (label, path, status)) in accounts.into_iter().enumerate() {
            let icon = match status.as_deref() {
                None => Icon::CloudDownload,
                Some("Offline") => Icon::WifiOff,
                Some(_) => Icon::TriangleAlert,
            };
            self.locations.insert(
                at.saturating_add(offset).min(self.locations.len()),
                Location { icon, label, path },
            );
        }
        self
    }

    /// The row of the chosen place: the favourite for the current folder,
    /// else the fixed location that is it.
    pub fn selected_id(&self) -> Option<String> {
        self.favorites
            .iter()
            .find(|favorite| favorite.path == self.active)
            .map(|favorite| favorite_id(&favorite.path))
            .or_else(|| {
                self.locations
                    .iter()
                    .find(|place| place.path == self.active)
                    .map(|place| location_id(&place.path))
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
                NavItem::new(location_id(&place.path), place.label.clone())
                    .icon(place.icon)
                    .without_actions()
            })
            .collect();
        sections.push(NavSection::new(
            LOCATIONS_SECTION,
            t("files-place-system"),
            items,
        ));
        sections
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
    /// reorders them, and a favourite's row action unpins it.
    pub fn nav_list(self, sink: &CommandSink) -> NavList {
        let selected = self.selected_id();
        let sections = self.sections();
        let places = Rc::new(self);
        let mut list = NavList::new(sections)
            .with_id("places_sidebar")
            .with_accessible_name(t("files-a11y-places"))
            .on_select({
                let sink = sink.clone();
                move |id| {
                    if let Some(path) = path_of_id(&id) {
                        push(&sink, FileCommand::Navigate(path));
                    }
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

    /// Workspaces lead the locations; mounted cloud accounts follow them and
    /// say when they are offline or need attention.
    #[test]
    fn workspaces_and_cloud_accounts_are_locations_with_a_status_icon() {
        let places = Places::new(&new_store(Vec::new()), PathBuf::from("/"))
            .with_workspaces(vec![PathBuf::from("/var/workspace")])
            .with_remote_accounts(vec![
                ("Cloud".into(), PathBuf::from("/home/a/Remote/Cloud"), None),
                (
                    "Work".into(),
                    PathBuf::from("/home/a/Remote/Work"),
                    Some("Offline".into()),
                ),
                (
                    "Mail".into(),
                    PathBuf::from("/home/a/Remote/Mail"),
                    Some("Sign in".into()),
                ),
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
        let icon = |id: &str| {
            locations
                .items
                .iter()
                .find(|i| i.id == id)
                .and_then(|i| i.icon)
        };
        assert_eq!(icon("loc:/home/a/Remote/Cloud"), Some(Icon::CloudDownload));
        assert_eq!(icon("loc:/home/a/Remote/Work"), Some(Icon::WifiOff));
        assert_eq!(icon("loc:/home/a/Remote/Mail"), Some(Icon::TriangleAlert));
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

    /// The list built from the places carries the chosen row and its name.
    #[test]
    fn the_list_shows_the_chosen_place() {
        let sink = new_sink();
        let list = sample().nav_list(&sink);
        assert_eq!(list.selected_id().as_deref(), Some("fav:/home/u/Pictures"));
    }
}
