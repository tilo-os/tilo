//! Where an editor's rich text lives and where its edits go.
//!
//! [`Hosted`] is the framework's: a [`RichText`] resource, edited through
//! the app's [`Dispatcher`]. [`RichTextField`] is rich text stored inside
//! another resource — a message body, a note's page, a comment — as a child
//! resource of it, edited through the child dispatcher that wraps each edit
//! in that resource's own action, so history, undo, permissions and replay
//! stay the owning resource's.

use std::rc::Rc;

use tilo_document::{RichText, RichTextAction, RichTextState};
use ui::automerge::ObjId;
use ui::automerge::transaction::Transactable;
use ui::framework::{Dispatcher, Live, ResourceSelection};
use ui::presence::PresenceKey;
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{Demand, Doc, ModelError, Resource, Tracker};
use ui::widget::PresenceKeyFn;

/// Everything the editor reads.
fn all() -> Demand {
    Demand::range(0..usize::MAX)
}

/// Where a [`RichTextSurface`](crate::RichTextSurface) reads its document
/// and sends its edits.
pub trait RichTextSource {
    /// Whether edits will be refused.
    fn read_only(&self) -> bool;

    /// Bring the state up to date with `doc`, every paragraph read.
    fn sync(&mut self, doc: &mut Doc);

    /// Increases whenever the state changes; `None` before there is one.
    fn revision(&self) -> Option<u64>;

    /// Read the state from `doc` without writing to it, for a view shown
    /// before its first sync; `false` when this source cannot.
    fn read_from(&mut self, _doc: &Doc) -> bool {
        false
    }

    /// Read the state, once there is one.
    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T>;

    /// Apply one edit to `doc` now.
    ///
    /// # Errors
    /// Returns the refusal; nothing is written.
    fn apply(&mut self, doc: &mut Doc, edit: &RichTextAction) -> Result<(), ActionError>;

    /// The presence keys collaborators address paragraphs by.
    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        None
    }
}

/// A [`RichText`] resource of a framework app: attached by the framework,
/// edited through its dispatcher.
#[derive(Default)]
pub struct Hosted {
    live: Option<Live<RichText>>,
    dispatch: Option<Dispatcher<RichText>>,
    demanded: bool,
}

impl Hosted {
    pub(crate) fn attach(&mut self, live: Live<RichText>, dispatch: Dispatcher<RichText>) {
        self.live = Some(live);
        self.dispatch = Some(dispatch);
        self.demanded = false;
    }
}

impl RichTextSource for Hosted {
    fn read_only(&self) -> bool {
        self.dispatch.as_ref().is_none_or(Dispatcher::read_only)
    }

    fn sync(&mut self, doc: &mut Doc) {
        let Some(live) = &self.live else {
            return;
        };
        if !self.demanded {
            // The editor lays out every paragraph, so it reads them all.
            match live.demand(doc, all()) {
                Ok(_) => self.demanded = true,
                Err(error) => tracing::warn!(%error, "rich text unreadable"),
            }
        }
        if let Err(error) = live.sync(doc) {
            tracing::warn!(%error, "rich text unreadable");
        }
    }

    fn revision(&self) -> Option<u64> {
        self.live.as_ref().map(Live::revision)
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.live.as_ref().map(|live| live.with(read))
    }

    fn apply(&mut self, doc: &mut Doc, edit: &RichTextAction) -> Result<(), ActionError> {
        match &self.dispatch {
            Some(dispatch) => dispatch.apply_now(doc, edit),
            None => Err(ActionError::Refused("the editor is not attached".into())),
        }
    }

    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        let app = self.dispatch.as_ref()?.app_id();
        Some(Rc::new(move |block: &str| {
            ResourceSelection::Item(block.to_owned())
                .presence::<RichText>(app)
                .0
        }))
    }
}

/// Finds a field's map in the owning document.
type Locate = Box<dyn Fn(&Doc) -> Option<ObjId>>;

/// The state of rich text under one field, kept current as the document
/// changes by re-reading what changed.
pub(crate) struct FieldState {
    pub(crate) root: ObjId,
    /// `None` for a state read without writing: the next sync reads again,
    /// and tracks from there.
    tracker: Option<Tracker>,
    pub(crate) state: RichTextState,
    revision: u64,
}

impl FieldState {
    /// The rich text under `root`, every paragraph read, with changes
    /// tracked from here.
    pub(crate) fn read(doc: &mut Doc, root: ObjId, revision: u64) -> Result<Self, ModelError> {
        let mut field = Self::read_only(doc, root, revision)?;
        let mut tracker = Tracker::new();
        tracker.start(doc);
        field.tracker = Some(tracker);
        Ok(field)
    }

    /// The rich text under `root`, every paragraph read, without writing to
    /// `doc`; nothing is tracked yet.
    pub(crate) fn read_only(doc: &Doc, root: ObjId, revision: u64) -> Result<Self, ModelError> {
        let mut state = RichText::read_under(doc, &root)?;
        RichText::load(&mut state, doc, &all())?;
        Ok(Self {
            root,
            tracker: None,
            state,
            revision,
        })
    }

    /// Increases whenever the state changes.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Re-read what changed since the last read; `true` when anything did.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the change cannot be placed; a fresh
    /// [`read`](Self::read) is then the way back.
    pub(crate) fn refresh(&mut self, doc: &mut Doc) -> Result<bool, ModelError> {
        let state = &self.state;
        let Some(tracker) = self.tracker.as_mut() else {
            return Err(ModelError::invalid("nothing tracked yet"));
        };
        let delta = tracker.delta_located(doc, &|obj| RichText::locate(state, obj));
        if delta.is_empty() {
            return Ok(false);
        }
        RichText::refresh_under(&mut self.state, doc, &delta)?;
        RichText::load(&mut self.state, doc, &all())?;
        self.revision = self.revision.wrapping_add(1);
        Ok(true)
    }
}

/// Rich text stored in a field of another resource — a message body, a
/// note's page, a comment — edited as a child resource of it.
///
/// `locate` finds the field's map in the document (creating nothing: the
/// owning resource creates the field with [`RichText::initialize_under`]).
/// Edits go through `dispatcher`, the owning resource's child dispatcher
/// for the field ([`Dispatcher::child`]), which wraps each edit in the
/// owning resource's action; that action's `apply` applies the edit under
/// the field with [`RichTextAction::apply_under`]. So the owning resource's
/// action is what history attributes, undo takes back, permissions check
/// and a collaborator replays, and typing a word is one undo step, as in a
/// rich-text resource.
///
/// Without a dispatcher ([`view_only`](Self::view_only)) the field shows the
/// text and refuses every edit: displaying is never editing.
pub struct RichTextField {
    locate: Locate,
    dispatch: Option<Dispatcher<RichText>>,
    field: Option<FieldState>,
    presence: Option<PresenceKeyFn>,
}

impl RichTextField {
    /// The rich text `locate` finds, edited through `dispatcher`.
    pub fn new(
        dispatcher: Dispatcher<RichText>,
        locate: impl Fn(&Doc) -> Option<ObjId> + 'static,
    ) -> Self {
        let mut field = Self::view_only(locate);
        field.attach(dispatcher);
        field
    }

    /// The rich text `locate` finds, shown and never edited.
    pub fn view_only(locate: impl Fn(&Doc) -> Option<ObjId> + 'static) -> Self {
        Self {
            locate: Box::new(locate),
            dispatch: None,
            field: None,
            presence: None,
        }
    }

    /// Edit through `dispatcher` from now on.
    pub fn attach(&mut self, dispatcher: Dispatcher<RichText>) {
        let app = dispatcher.app_id();
        self.dispatch = Some(dispatcher);
        if self.presence.is_none() {
            self.presence = Some(Rc::new(move |block: &str| {
                ResourceSelection::Item(block.to_owned())
                    .presence::<RichText>(app)
                    .0
            }));
        }
    }

    /// Key collaborators' carets by paragraph with `key`.
    #[must_use]
    pub fn with_presence(mut self, key: impl Fn(&str) -> PresenceKey + 'static) -> Self {
        self.presence = Some(Rc::new(key));
        self
    }

    fn reread(&mut self, doc: &mut Doc, root: ObjId) {
        let revision = self
            .field
            .as_ref()
            .map_or(0, |field| field.revision.wrapping_add(1));
        match FieldState::read(doc, root, revision) {
            Ok(field) => self.field = Some(field),
            Err(error) => {
                tracing::warn!(%error, "rich-text field unreadable");
                self.field = None;
            }
        }
    }
}

impl RichTextSource for RichTextField {
    fn read_only(&self) -> bool {
        self.dispatch.as_ref().is_none_or(Dispatcher::read_only)
    }

    fn sync(&mut self, doc: &mut Doc) {
        let Some(root) = (self.locate)(doc) else {
            self.field = None;
            return;
        };
        let current = self.field.as_ref().is_some_and(|field| {
            field.root == root && field.tracker.is_some() && doc.pending_ops() == 0
        });
        if !current {
            self.reread(doc, root);
            return;
        }
        let Some(field) = &mut self.field else {
            return;
        };
        if field.refresh(doc).is_err() {
            self.reread(doc, root);
        }
    }

    fn revision(&self) -> Option<u64> {
        self.field.as_ref().map(|field| field.revision)
    }

    fn read_from(&mut self, doc: &Doc) -> bool {
        let Some(root) = (self.locate)(doc) else {
            self.field = None;
            return false;
        };
        let revision = self
            .field
            .as_ref()
            .map_or(0, |field| field.revision.wrapping_add(1));
        match FieldState::read_only(doc, root, revision) {
            Ok(field) => {
                self.field = Some(field);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "rich-text field unreadable");
                false
            }
        }
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.field.as_ref().map(|field| read(&field.state))
    }

    fn apply(&mut self, doc: &mut Doc, edit: &RichTextAction) -> Result<(), ActionError> {
        match &self.dispatch {
            Some(dispatch) => dispatch.apply_now(doc, edit),
            None => Err(ActionError::Refused(
                "this text is open to view only".into(),
            )),
        }
    }

    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        self.presence.clone()
    }
}

/// A shared handle, for an app that keeps the source to change it later
/// (its permissions) while the surface holds it.
impl<S: RichTextSource> RichTextSource for Rc<std::cell::RefCell<S>> {
    fn read_only(&self) -> bool {
        self.borrow().read_only()
    }

    fn sync(&mut self, doc: &mut Doc) {
        self.borrow_mut().sync(doc);
    }

    fn revision(&self) -> Option<u64> {
        self.borrow().revision()
    }

    fn read_from(&mut self, doc: &Doc) -> bool {
        self.borrow_mut().read_from(doc)
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.borrow().with_state(read)
    }

    fn apply(&mut self, doc: &mut Doc, edit: &RichTextAction) -> Result<(), ActionError> {
        self.borrow_mut().apply(doc, edit)
    }

    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        self.borrow().presence_keys()
    }
}

/// Rich text at a place in a shared document, edited by writing to it
/// directly: shared working state that is not history, such as a draft two
/// people type into before either sends it. Collaborators see every
/// keystroke; nothing is recorded as an action or undone as one.
pub struct DirectField {
    locate: Locate,
    field: Option<FieldState>,
    read_only: bool,
    presence: Option<PresenceKeyFn>,
}

impl DirectField {
    /// The rich text `locate` finds (created by its owner with
    /// [`RichText::initialize_under`]).
    pub fn new(locate: impl Fn(&Doc) -> Option<ObjId> + 'static) -> Self {
        Self {
            locate: Box::new(locate),
            field: None,
            read_only: false,
            presence: None,
        }
    }

    /// Key collaborators' carets by paragraph with `key`.
    #[must_use]
    pub fn with_presence(mut self, key: impl Fn(&str) -> PresenceKey + 'static) -> Self {
        self.presence = Some(Rc::new(key));
        self
    }

    /// Whether the participant may only view: edits are refused while set.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    fn reread(&mut self, doc: &mut Doc, root: ObjId) {
        let revision = self
            .field
            .as_ref()
            .map_or(0, |field| field.revision.wrapping_add(1));
        match FieldState::read(doc, root, revision) {
            Ok(field) => self.field = Some(field),
            Err(error) => {
                tracing::warn!(%error, "rich text unreadable");
                self.field = None;
            }
        }
    }
}

impl RichTextSource for DirectField {
    fn read_only(&self) -> bool {
        self.read_only
    }

    fn sync(&mut self, doc: &mut Doc) {
        let Some(root) = (self.locate)(doc) else {
            self.field = None;
            return;
        };
        if doc.pending_ops() > 0 {
            // Typing in progress: read without committing it, so the writes
            // stay one open transaction the host commits as it chooses.
            let revision = self
                .field
                .as_ref()
                .map_or(0, |field| field.revision.wrapping_add(1));
            match FieldState::read_only(doc, root, revision) {
                Ok(field) => self.field = Some(field),
                Err(error) => {
                    tracing::warn!(%error, "rich text unreadable");
                    self.field = None;
                }
            }
            return;
        }
        let current = self
            .field
            .as_ref()
            .is_some_and(|field| field.root == root && field.tracker.is_some());
        if !current {
            self.reread(doc, root);
            return;
        }
        let Some(field) = &mut self.field else {
            return;
        };
        if field.refresh(doc).is_err() {
            self.reread(doc, root);
        }
    }

    fn revision(&self) -> Option<u64> {
        self.field.as_ref().map(|field| field.revision)
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.field.as_ref().map(|field| read(&field.state))
    }

    fn apply(&mut self, doc: &mut Doc, edit: &RichTextAction) -> Result<(), ActionError> {
        if self.read_only {
            return Err(ActionError::Refused(
                "this text is open to view only".into(),
            ));
        }
        let Some(root) = (self.locate)(doc) else {
            return Err(ActionError::Refused("the text is gone".into()));
        };
        edit.apply_under(doc, &root)
    }

    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        self.presence.clone()
    }
}
