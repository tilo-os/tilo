//! Device-private feedback custody shared by native and browser desktops.
//! A host supplies an already-admitted destination and its existing adapter;
//! neither a window, a URL nor a draft chooses transport authority.

use std::{cell::RefCell, rc::Rc};

use tilo_desktop_model::{
    AppKind, DesktopTree, GroupId, StripId, TabId, TileContent, WorkspaceSlot,
};

use tilo_app::feedback::{Composer, DurableStore, Error, Outbox, StorageKey};
pub use tilo_app::feedback::{Control, Destination, FeedbackHost, ShownRecord, Subject};
use tilo_app::feedback_composer::{self, FeedbackServices};
use ui::{application::AppComponent, launch::LaunchError};

pub mod recorder;

pub const PACKAGE: &str = "so.tilo.feedback-composer";

/// Install `startup` into the profile's feedback slot, the custody every
/// composer window of the profile borrows. A profile holds one: a second
/// startup is refused rather than replacing the authority reports go under.
///
/// # Errors
///
/// Refuses a second custody, changed authority and storage that cannot keep
/// the approved outbox.
pub fn install(
    startup: tilo_feedback_transport::Startup,
    store: Rc<dyn DurableStore>,
    slot: &mut Option<PrivateFeedback>,
) -> Result<(), Error> {
    if slot.is_some() {
        return Err(Error::Conflict);
    }
    let destination = startup.destination().clone();
    let host = startup.host(store.clone())?;
    *slot = Some(PrivateFeedback::new(store, destination, host)?);
    Ok(())
}

/// One profile's pinned destination and durable approved outbox. The draft component uses its own
/// ephemeral store and is never a workspace record.
pub struct PrivateFeedback {
    services: FeedbackServices,
}

impl PrivateFeedback {
    /// Wake for private persistence even when its presentation is closed.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.services.next_wake_deadline()
    }

    /// Bind existing host authority without joining a workspace or creating a grant.
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
        let key = StorageKey::app(format!("so.tilo.feedback-outbox.{}", namespace.to_hex()));
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

    /// The services a composer's preparation borrows, for a host that has the composer's code from
    /// elsewhere than [`Self::prepare`] (the browser desktop fetches it the first time Feedback
    /// opens).
    #[must_use]
    pub fn services(&self) -> FeedbackServices {
        self.services.clone()
    }

    /// Name the window the next report is about. The desktop chooses it from where the person asked
    /// for Feedback; nothing about it is read until the person includes context in a review.
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
/// review/send. It never guesses a public workspace or lends a generic writer.
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

/// The window in front of `tree` . `name` is an app's own display name by id, where the host knows
/// one; a tile's label is not used, since it can be a window title, which may be private.
pub fn focused(tree: &DesktopTree, name: impl Fn(&str) -> Option<String>) -> Focused {
    match front_tab(tree).map(|tab| &tab.tile) {
        None => Focused::Nothing,
        Some(tile) if is_composer(tile) => Focused::Composer,
        Some(tile) => subject(tile, name).map_or(Focused::Nothing, Focused::App),
    }
}

/// What a report about the parts picked in `draft` is about, for a recording of its window: the
/// window in front (`front`), when every pick is in its app; otherwise no window, so none is
/// recorded.
pub fn picked_subject(draft: &tilo_app::feedback::ReportDraft, front: Focused) -> Option<Subject> {
    let Focused::App(subject) = front else {
        return None;
    };
    (!draft.picks.is_empty() && draft.picks.iter().all(|pick| pick.app == subject.app))
        .then_some(subject)
}

/// The window in front of `tree` , by its tab.
pub fn focused_tab(tree: &DesktopTree) -> Option<TabId> {
    front_tab(tree).map(|tab| tab.id)
}

fn front_tab(tree: &DesktopTree) -> Option<&tilo_desktop_model::Tab> {
    let workspace = tree.active_workspace()?;
    tree.active_strip
        .get(&workspace.slot)
        .and_then(|id| workspace.strips.iter().find(|strip| strip.id == *id))
        .or_else(|| workspace.strips.first())
        .and_then(|strip| {
            tree.active_group_for(strip.id)
                .and_then(|id| strip.window_groups.iter().find(|group| group.id == id))
                .or_else(|| strip.window_groups.first())
        })
        .and_then(|group| group.tabs.iter().find(|tab| tab.id == group.active_tab))
}

/// The app the window `tab` shows, as a report names it; `None` for a tab that is not in `tree` or
/// shows no app.
pub fn tab_subject(
    tree: &DesktopTree,
    tab: TabId,
    name: impl Fn(&str) -> Option<String>,
) -> Option<Subject> {
    let tile = tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group
                    .tabs
                    .iter()
                    .find(|shown| shown.id == tab)
                    .map(|shown| &shown.tile)
            })
        })
    })?;
    subject(tile, name)
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
pub fn composer_tab(tree: &DesktopTree) -> Option<(WorkspaceSlot, StripId, GroupId, TabId)> {
    tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                group
                    .tabs
                    .iter()
                    .find(|tab| is_composer(&tab.tile))
                    .map(|tab| (workspace.slot, strip.id, group.id, tab.id))
            })
        })
    })
}

fn subject(tile: &TileContent, name: impl Fn(&str) -> Option<String>) -> Option<Subject> {
    let app = tilo_desktop_model::apps::canonical_app_id(tile)?;
    let app = String::from(app);
    let name = match tile {
        // A built-in's kind is the app's own name.
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            kind.as_str().to_owned()
        }
        _ => name(&app).unwrap_or_default(),
    };
    Some(Subject {
        app,
        name,
        control: None,
        record: None,
    })
}

#[cfg(test)]
mod install_tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::cell::RefCell;
    use tilo_actions::remote::Caller;
    use tilo_app::feedback::MemoryStore;
    use tilo_feedback_transport::{Enrollment, NoCapture, SessionTransport, Startup};
    use ui::permissions::{PermissionContext, Role};

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    const REPORTER: [u8; 32] = [12; 32];

    fn startup() -> Startup {
        let enrollment = Enrollment {
            destination: Destination {
                room: WORKSPACE.into(),
                provider: "record:so.tilo.chat:".into(),
                channel: "feedback".into(),
            },
            owner: Caller::authenticated(
                [11; 32],
                SigningKey::from_bytes(&[41; 32]).verifying_key().to_bytes(),
            )
            .in_workspace(WORKSPACE),
            reporter: Caller::authenticated(
                REPORTER,
                SigningKey::from_bytes(&[42; 32]).verifying_key().to_bytes(),
            )
            .in_workspace(WORKSPACE),
            permissions: PermissionContext::member_of(
                WORKSPACE,
                hex::encode(REPORTER),
                Role::Owner,
            )
            .at_unix_time(0),
        };
        let held = Rc::new(RefCell::new(enrollment.clone()));
        let session = SessionTransport::new(
            Box::new(move || Ok(held.borrow().clone())),
            Box::new(|_, _| Ok(())),
            Box::new(|_| Ok(None)),
        );
        Startup::new(enrollment, Box::new(session), Box::new(NoCapture)).unwrap()
    }

    /// The profile holds one custody: its composer mounts with the private report controls, and a
    /// second startup cannot replace the authority reports already go under.
    #[test]
    fn a_profile_keeps_one_custody_and_its_composer_mounts_protected() {
        let store: Rc<dyn DurableStore> = Rc::new(MemoryStore::new());
        let mut slot = None;
        install(startup(), store.clone(), &mut slot).unwrap();
        let component = slot
            .as_ref()
            .unwrap()
            .prepare("device", &hex::encode(REPORTER), 42)
            .unwrap();
        let frame = ui::headless::FrameHarness::from_component(
            component,
            (1000, 700),
            ui::application::ComponentRunnerInputs::from_native_process(),
        )
        .unwrap();
        assert!(frame.capture_protected());
        assert!(matches!(
            install(startup(), store, &mut slot),
            Err(Error::Conflict)
        ));
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use tilo_desktop_model::{Strip, Tab, WindowGroup, WindowWidth, Workspace};

    fn tree(tiles: Vec<TileContent>) -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Here");
        let mut strip = Strip::new(StripId(2), "Main");
        for (index, tile) in tiles.into_iter().enumerate() {
            let id = 10_u64.saturating_add(u64::try_from(index).unwrap());
            strip.window_groups.push(WindowGroup {
                id,
                tabs: vec![Tab {
                    id,
                    title: "Bank statement".into(),
                    tile,
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                }],
                active_tab: id,
                width: WindowWidth::Fraction(500),
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        workspace.strips.push(strip);
        DesktopTree {
            active: workspace.slot,
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
                control: None,
                record: None,
            }),
            "a label can be a window title"
        );
        assert_eq!(
            focused(&desktop, |app: &str| Some(format!("Name of {app}"))),
            Focused::App(Subject {
                app: "org.mozilla.firefox".into(),
                name: "Name of org.mozilla.firefox".into(),
                control: None,
                record: None,
            })
        );
        desktop.set_active_group(StripId(2), 11);
        assert_eq!(focused(&desktop, none), Focused::Composer);
        assert_eq!(
            composer_tab(&desktop),
            Some((WorkspaceSlot(1), StripId(2), 11, 11))
        );
    }

    /// Picked parts are about the window in front only when every one is in its app: a recording
    /// never records a window nothing was picked in.
    #[test]
    fn picked_parts_are_about_the_window_in_front_only_when_all_are_in_it() {
        use tilo_app::feedback::report::{Pick, ReportDraft};
        let notes = Subject {
            record: Some(ShownRecord {
                workspace: "w".into(),
                record: "note-1".into(),
            }),
            ..Subject::of_app("so.tilo.notes", "Notes")
        };
        let draft = |apps: &[&str]| ReportDraft {
            picks: apps
                .iter()
                .map(|app| Pick {
                    app: (*app).into(),
                    ..Pick::default()
                })
                .collect(),
            ..ReportDraft::default()
        };
        let front = || Focused::App(notes.clone());
        assert_eq!(
            picked_subject(&draft(&["so.tilo.notes", "so.tilo.notes"]), front()),
            Some(notes.clone())
        );
        assert_eq!(
            picked_subject(&draft(&["so.tilo.notes", "so.tilo.chat"]), front()),
            None
        );
        assert_eq!(picked_subject(&draft(&[]), front()), None);
        assert_eq!(
            picked_subject(&draft(&["so.tilo.notes"]), Focused::Composer),
            None
        );
    }
}
