//! What the person can do to one window, as the desktop offers it.
//!
//! The rows are the desktop's own and no app's: naming a window, filling
//! the strip with it and giving the strip back, widening or narrowing it,
//! and closing it. Every row is a navigation command the desktop already
//! answers, so a menu — the overflow menu in the window's own bar, or one a
//! sheet mounts beside it — offers exactly what the desktop can do, and
//! choosing a row sends the same command a chord or a control sends.
//!
//! Which rows apply follows the window: a window filling the strip offers
//! the strip back instead of filling it, the widest window cannot be
//! widened, and a window someone else holds can be neither named nor
//! closed.

use tilo_desktop_model::width::{
    WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE,
};
use tilo_desktop_model::{DesktopTree, TabId, WindowGroup};

use crate::i18n::{t, t_args};
use crate::navigation::{Command, Route, RouteIds};

pub const RENAME: &str = "window-rename";
pub const MAXIMIZE: &str = "window-maximize";
pub const RESTORE: &str = "window-restore";
pub const WIDER: &str = "window-wider";
pub const NARROWER: &str = "window-narrower";
pub const CLOSE: &str = "window-close";

/// The widths a wider or a narrower window lands on, narrowest first.
const WIDTHS: [u32; 3] = [
    WIDTH_NARROW_PERMILLE,
    WIDTH_NORMAL_PERMILLE,
    WIDTH_WIDE_PERMILLE,
];

/// One row of a window's menu: what it does, how it reads, and why it
/// cannot be chosen when it cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The stable id a menu host sends back with the window it is for.
    pub id: &'static str,
    pub label: String,
    /// Whether the person can choose it now; a row they cannot is shown
    /// with `reason` beside it.
    pub enabled: bool,
    pub reason: Option<String>,
}

/// The menu rows that apply to the window at `route`, in the order to show
/// them. `held` names the person who holds the window on a shared desktop:
/// their window is theirs to close and to name, not the viewer's.
pub fn rows(tree: &DesktopTree, route: &RouteIds, held: Option<&str>) -> Vec<Row> {
    let Some(group) = group_at(tree, route) else {
        return Vec::new();
    };
    let mine = held.is_none();
    let why = || held.map(|name| t_args("window-menu-someone-elses", &[("name", name.into())]));
    let permille = group.width.permille();
    let wider = WIDTHS.iter().copied().find(|width| *width > permille);
    let narrower = WIDTHS.iter().rev().copied().find(|width| *width < permille);
    vec![
        Row {
            id: RENAME,
            label: t("window-menu-rename"),
            enabled: mine,
            reason: why(),
        },
        Row {
            id: if group.maximized { RESTORE } else { MAXIMIZE },
            label: t(if group.maximized {
                "window-menu-restore"
            } else {
                "window-menu-maximize"
            }),
            enabled: true,
            reason: None,
        },
        Row {
            id: WIDER,
            label: t("window-menu-wider"),
            enabled: wider.is_some(),
            reason: wider.is_none().then(|| t("window-menu-widest")),
        },
        Row {
            id: NARROWER,
            label: t("window-menu-narrower"),
            enabled: narrower.is_some(),
            reason: narrower.is_none().then(|| t("window-menu-narrowest")),
        },
        Row {
            id: CLOSE,
            label: t("window-menu-close"),
            enabled: mine,
            reason: why(),
        },
    ]
}

/// The navigation command row `id` of the window at `route` means, for a
/// desktop whose generation is `generation`; `name` is what the person
/// typed for [`RENAME`]. `None` for a row that does not apply here or a
/// name the desktop would refuse.
pub fn command(
    tree: &DesktopTree,
    route: &RouteIds,
    id: &str,
    name: Option<&str>,
    generation: &str,
) -> Option<Command> {
    let group = group_at(tree, route)?;
    let route = route_of(route);
    Some(match id {
        RENAME => {
            let name = name?;
            if name.trim().chars().count() > crate::navigation::MAX_NAME_CHARS {
                return None;
            }
            Command::Rename {
                generation: generation.to_owned(),
                route,
                name: name.to_owned(),
            }
        }
        MAXIMIZE | RESTORE => Command::Maximize {
            generation: generation.to_owned(),
            route,
            maximized: id == MAXIMIZE,
        },
        WIDER | NARROWER => {
            let permille = group.width.permille();
            let next = if id == WIDER {
                WIDTHS.iter().copied().find(|width| *width > permille)?
            } else {
                WIDTHS
                    .iter()
                    .rev()
                    .copied()
                    .find(|width| *width < permille)?
            };
            Command::Width {
                generation: generation.to_owned(),
                route,
                permille: next,
            }
        }
        CLOSE => Command::Close {
            generation: generation.to_owned(),
            route,
        },
        _ => return None,
    })
}

/// The window group at `route`, while the tree still holds it.
fn group_at<'a>(tree: &'a DesktopTree, route: &RouteIds) -> Option<&'a WindowGroup> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find(|strip| strip.id == route.strip)?
        .window_groups
        .iter()
        .find(|group| group.id == route.group)
}

/// `ids` as a command names it.
fn route_of(ids: &RouteIds) -> Route {
    Route {
        workspace: ids.workspace.0.to_string(),
        strip: ids.strip.0.to_string(),
        group: ids.group.to_string(),
        tab: ids.tab.to_string(),
    }
}

#[cfg(test)]
#[path = "window_menu_tests.rs"]
mod tests;

impl crate::surface::DesktopHandle {
    /// Who holds tab `tab` on a shared desktop, when someone else does: the
    /// menu offers that person's window only what a viewer may do to it.
    #[must_use]
    pub fn held_by(&self, tab: TabId) -> Option<String> {
        self.shared
            .borrow()
            .holders
            .get(&tab)
            .map(|holder| holder.name.clone())
    }

    /// Fill the strip with the window showing `tab`, or give it its width
    /// back, on the next update pass. False when no window shows it.
    pub fn maximize_tab(&self, tab: TabId, maximized: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = crate::surface::route_of(&shared.tree, tab) else {
            return false;
        };
        shared
            .queue
            .push(crate::navigation::Admitted::Maximize(ids, maximized));
        true
    }

    /// The menu rows of the window showing `tab`, as the desktop would show
    /// them now. Empty when no window shows it.
    pub fn window_menu(&self, tab: TabId) -> Vec<Row> {
        let held = self.held_by(tab);
        self.read(|tree| {
            let Some(route) = super::route_of(tree, tab) else {
                return Vec::new();
            };
            rows(tree, &route, held.as_deref())
        })
    }

    /// Choose row `id` of the window showing `tab`: the command the row
    /// means, dispatched. `name` is what the person typed for [`RENAME`].
    pub fn choose_window_menu(
        &self,
        tab: TabId,
        id: &str,
        name: Option<&str>,
        generation: &str,
    ) -> Result<(), String> {
        let command = self.read(|tree| {
            let route = super::route_of(tree, tab)?;
            command(tree, &route, id, name, generation)
        });
        match command {
            Some(command) => self.dispatch(&command, generation),
            None => Err("That is not something this window offers".into()),
        }
    }
}
