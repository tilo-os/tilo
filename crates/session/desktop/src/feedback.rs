//! Device-private feedback custody shared by native and browser desktops.
//! A host supplies an already-admitted destination and its existing adapter;
//! neither a window, a URL nor a draft chooses transport authority.

use std::{cell::RefCell, rc::Rc};

use tilo_desktop_model::{AppKind, DesktopTree, GroupId, StripId, TabId, TileContent, WorkspaceId};

use tilo_app::feedback::{Composer, DurableStore, Error, Outbox, StorageKey};
pub use tilo_app::feedback::{Destination, FeedbackHost, Subject};
use tilo_app::feedback_composer::{self, FeedbackServices};
use ui::{application::AppComponent, launch::LaunchError};

pub mod transport;

pub const PACKAGE: &str = "so.tilo.feedback-composer";

/// One profile's pinned destination and durable approved outbox. The draft
/// component uses its own ephemeral store and is never a workspace record.
pub struct PrivateFeedback {
    services: FeedbackServices,
}

impl PrivateFeedback {
    /// Wake for private persistence even when its presentation is closed.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.services.next_wake_deadline()
    }

    /// Bind existing host authority without joining a room or creating a grant.
    ///
    /// # Errors
    /// Refuses invalid destinations, concurrent outbox ownership and storage
    /// that cannot recover its existing approved submissions.
    pub fn new(
        store: Rc<dyn DurableStore>,
        destination: Destination,
        host: Box<dyn FeedbackHost>,
    ) -> Result<Self, Error> {
        destination.validate()?;
        let namespace = blake3::hash(&serde_json::to_vec(&destination)?);
        let key = StorageKey::package(format!("so.tilo.feedback-outbox.{}", namespace.to_hex()));
        let outbox = Outbox::open(store, key)?;
        let controller = Composer::new(destination, outbox, host)?;
        Ok(Self {
            services: FeedbackServices::new(Rc::new(RefCell::new(controller))),
        })
    }

    /// Mount the actual private review controls over an isolated draft store.
    ///
    /// # Errors
    /// Refuses invalid component identity or a second concurrent presentation.
    pub fn prepare(
        &self,
        device: &str,
        author: &str,
        instance: u64,
    ) -> Result<AppComponent, LaunchError> {
        feedback_composer::prepare(Some(self.services.clone()), device, author, instance)
    }

    /// Name the window the next report is about. The desktop chooses it from
    /// where the person asked for Feedback; nothing about it is read until
    /// the person includes context in a review.
    ///
    /// # Errors
    /// Refuses while the controller is busy in an effect.
    pub fn about(&self, subject: Option<Subject>) -> Result<(), Error> {
        self.services.about(subject)
    }

    /// Service storage/transport wakes even after the person closes the window.
    ///
    /// # Errors
    /// Preserves and presents the existing controller's actual failure.
    pub fn pump(&self) -> Result<(), Error> {
        self.services.pump()
    }
}

/// The desktop catalogue's internal component, not an installed application.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    feedback_composer::definition()
}

/// An unconfigured desktop still opens private controls and explicitly refuses
/// review/send. It never guesses a public room or lends a generic writer.
///
/// # Errors
/// Refuses invalid private component identity.
pub fn unconfigured(
    device: &str,
    author: &str,
    instance: u64,
) -> Result<AppComponent, LaunchError> {
    feedback_composer::prepare(None, device, author, instance)
}

/// The composer's tab kind where a host registers it as a built-in.
pub const KIND: &str = "Feedback";

/// What the window in front is, for a report about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Focused {
    /// Nothing is in front: general feedback.
    Nothing,
    /// The composer itself: it keeps the subject it already has.
    Composer,
    App(Subject),
}

/// The window in front of `tree`. `name` is an app's own display name by
/// id, where the host knows one; a tile's label is not used, since it can be
/// a window title, which may be private.
pub fn focused(tree: &DesktopTree, name: impl Fn(&str) -> Option<String>) -> Focused {
    let Some(workspace) = tree.active_workspace() else {
        return Focused::Nothing;
    };
    let tile = tree
        .active_strip
        .get(&workspace.id)
        .and_then(|id| workspace.strips.iter().find(|strip| strip.id == *id))
        .or_else(|| workspace.strips.first())
        .and_then(|strip| {
            tree.active_group_for(strip.id)
                .and_then(|id| strip.window_groups.iter().find(|group| group.id == id))
                .or_else(|| strip.window_groups.first())
        })
        .and_then(|group| group.tabs.iter().find(|tab| tab.id == group.active_tab))
        .map(|tab| &tab.tile);
    match tile {
        None => Focused::Nothing,
        Some(tile) if is_composer(tile) => Focused::Composer,
        Some(tile) => subject(tile, name).map_or(Focused::Nothing, Focused::App),
    }
}

/// Whether `tile` is the feedback composer, under either host's registration.
pub fn is_composer(tile: &TileContent) -> bool {
    match tile {
        TileContent::AppPlaceholder {
            kind: AppKind::Other(kind),
            ..
        } => kind == KIND,
        TileContent::LocalApp { app_id, .. } => app_id == PACKAGE,
        _ => false,
    }
}

/// Where the composer is open, if anywhere: there is only ever one.
pub fn composer_tab(tree: &DesktopTree) -> Option<(WorkspaceId, StripId, GroupId, TabId)> {
    tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group
                    .tabs
                    .iter()
                    .find(|tab| is_composer(&tab.tile))
                    .map(|tab| (workspace.id, strip.id, group.id, tab.id))
            })
        })
    })
}

fn subject(tile: &TileContent, name: impl Fn(&str) -> Option<String>) -> Option<Subject> {
    let app = tilo_desktop_model::apps::canonical_app_id(tile)?;
    let name = match tile {
        // A built-in's kind is the app's own name.
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            kind.as_str().to_owned()
        }
        _ => name(&app).unwrap_or_default(),
    };
    Some(Subject { app, name })
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use tilo_desktop_model::{Strip, Tab, WindowGroup, WindowWidth, Workspace};

    fn tree(tiles: Vec<TileContent>) -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Here");
        let mut strip = Strip::new(StripId(2), "Main");
        for (index, tile) in tiles.into_iter().enumerate() {
            let id = 10_u64.saturating_add(u64::try_from(index).unwrap());
            strip.window_groups.push(WindowGroup {
                id,
                tabs: vec![Tab {
                    id,
                    title: "Bank statement".into(),
                    tile,
                }],
                active_tab: id,
                width: WindowWidth::Fraction(500),
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        workspace.strips.push(strip);
        DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    #[test]
    fn the_window_in_front_names_its_app_never_its_title() {
        let none = |_: &str| None;
        assert_eq!(focused(&tree(Vec::new()), none), Focused::Nothing);
        let mut desktop = tree(vec![
            TileContent::LocalApp {
                app_id: "org.mozilla.firefox".into(),
                label: "Bank statement".into(),
                note: String::new(),
            },
            TileContent::AppPlaceholder {
                kind: AppKind::Other(KIND.into()),
                label: "Feedback".into(),
            },
        ]);
        assert_eq!(
            focused(&desktop, none),
            Focused::App(Subject {
                app: "org.mozilla.firefox".into(),
                name: String::new(),
            }),
            "a label can be a window title"
        );
        assert_eq!(
            focused(&desktop, |app: &str| Some(format!("Name of {app}"))),
            Focused::App(Subject {
                app: "org.mozilla.firefox".into(),
                name: "Name of org.mozilla.firefox".into(),
            })
        );
        desktop.set_active_group(StripId(2), 11);
        assert_eq!(focused(&desktop, none), Focused::Composer);
        assert_eq!(
            composer_tab(&desktop),
            Some((WorkspaceId(1), StripId(2), 11, 11))
        );
    }
}
