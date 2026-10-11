//! Device-private feedback custody shared by native and browser desktops.
//! A host supplies an already-admitted destination and its existing adapter;
//! neither a window, a URL nor a draft chooses transport authority.

use std::{cell::RefCell, rc::Rc};

use tilo_app::feedback::{Composer, DurableStore, Error, Outbox, StorageKey};
pub use tilo_app::feedback::{Destination, FeedbackHost};
use tilo_app::feedback_composer::{self, FeedbackServices};
use ui::{application::AppComponent, launch::LaunchError};

pub mod capture;
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
