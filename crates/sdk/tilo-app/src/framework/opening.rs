//! A window shown before its record ([`ResourceApp::opening`](super::ResourceApp::opening)).
//!
//! The workspace's record opens off the frame
//! ([`ResourceStore::begin_opening`](ui_core::resource::ResourceStore::begin_opening)); meanwhile the
//! window's component is an [`OpeningRoot`] over what the app shows before it. Once the record
//! is decoded, the window, about to sync, asks the root for the component to show in its
//! place ([`AppWidget::take_component_replacement`]): the root takes the record in and builds
//! the component any launch builds over it, whose view takes over what was shown ([`Opened`]),
//! so nothing the person saw or did is built again or lost.
//!
//! Until then nothing of the root reaches the window's API: what was shown hands its edits to
//! the API once the view has taken it over, in the order they were made.

use std::cell::RefCell;

use tilo_ui_base::widget::Widget;
use ui_core::application::ComponentReplacement;
use ui_core::launch::LaunchError;
use ui_core::resource::{ResourceOpenError, ResourceOpening};
use ui_core::{widget::AppKind, widget::AppWidget, widget::IconDescriptor};

use super::app::OpeningView;

/// What a window showed before its record ([`ResourceApp::opening`](super::ResourceApp::opening)),
/// lent to the view built once the record is had
/// ([`ResourceEnv::take_lent`](super::ResourceEnv::take_lent)).
pub struct Opened(RefCell<Option<Box<dyn OpeningView>>>);

impl Opened {
    pub(crate) fn new(shown: Box<dyn OpeningView>) -> Self {
        Self(RefCell::new(Some(shown)))
    }

    /// What was shown, as the type the app built it as; once.
    #[must_use]
    pub fn take<W: 'static>(&self) -> Option<Box<W>> {
        let shown = self.0.borrow_mut().take()?;
        shown.into_lent().downcast::<W>().ok()
    }
}

/// Builds the component of the record, which takes over what was shown.
pub(crate) type Build =
    Box<dyn FnOnce(Box<dyn OpeningView>) -> Result<ComponentReplacement, LaunchError>>;

/// The root of a window's component while its record is read off the frame.
pub(crate) struct OpeningRoot {
    shown: Box<dyn OpeningView>,
    record: ResourceOpening,
    build: Option<Build>,
    title: &'static str,
    icon: IconDescriptor,
}

impl OpeningRoot {
    pub(crate) fn new(
        shown: Box<dyn OpeningView>,
        record: ResourceOpening,
        build: Build,
        (title, icon): (&'static str, IconDescriptor),
    ) -> Self {
        // The window wakes when the record is decoded, to take it in.
        record.wake_when_decoded(ui_core::wake::WakeHandle::new());
        Self {
            shown,
            record,
            build: Some(build),
            title,
            icon,
        }
    }
}

impl tilo_ui_base::widget::Decorator for OpeningRoot {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        &*self.shown
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut *self.shown
    }

    // What is shown keeps its controls, undo and presence to itself until the record's view
    // takes it over: its edits wait for the API that view hands them to.
    fn provide<'a>(&'a self, _request: &mut tilo_ui_base::widget::capability::Request<'a, '_>) {}

    fn provide_mut<'a>(
        &'a mut self,
        _request: &mut tilo_ui_base::widget::capability::RequestMut<'a, '_>,
    ) {
    }
}

impl AppWidget for OpeningRoot {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }

    fn app_title(&self) -> &str {
        self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        self.icon.clone()
    }

    /// Once the record is decoded: take it in and build its component over what was shown. A
    /// record the workspace holds none of is built too, as a new one, and so is one that does not
    /// decode: opening it shows why. A workspace that could not be reached says so in its place.
    fn take_component_replacement(&mut self) -> Option<ComponentReplacement> {
        self.build.as_ref()?;
        match self.record.take_in(false) {
            Ok(false) => return None,
            Ok(true) | Err(ResourceOpenError::MissingResource) => {}
            Err(error @ ResourceOpenError::Unreachable(_)) => {
                let refusal = LaunchError::Record(error).to_string();
                tracing::warn!(%refusal, "the workspace's record did not arrive");
                self.build = None;
                self.shown = Box::new(
                    ui_core::widget::EmptyState::new(tilo_ui_base::i18n::widget_text(
                        "framework-unreadable",
                    ))
                    .with_icon(tilo_ui_base::icons::Icon::CircleAlert)
                    .with_message(&refusal),
                );
                return None;
            }
            Err(error) => tracing::warn!(%error, "the workspace's record did not open"),
        }
        let build = self.build.take()?;
        let shown = std::mem::replace(&mut self.shown, Box::new(ui_core::widget::Absent::new()));
        match build(shown) {
            Ok(replacement) => Some(replacement),
            Err(error) => {
                tracing::error!(%error, "the workspace's record opened, and its window did not");
                None
            }
        }
    }
}
