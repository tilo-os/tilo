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
use ui::framework::{Dispatcher, Live, PersonRecord, ResourceSelection};
use ui::presence::PresenceKey;
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{Demand, Doc, Invoke, ModelError, Resource, Tracker};
use ui::widget::{Edits, PresenceKeyFn};

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
    fn sync(&mut self, doc: &Doc);

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
    fn apply(&mut self, edits: &mut Edits<'_>, edit: &RichTextAction) -> Result<(), ActionError>;

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

    fn sync(&mut self, doc: &Doc) {
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

    fn apply(&mut self, edits: &mut Edits<'_>, edit: &RichTextAction) -> Result<(), ActionError> {
        match &self.dispatch {
            Some(dispatch) => dispatch.apply_now(edits, edit),
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
    pub(crate) fn read(doc: &Doc, root: ObjId, revision: u64) -> Result<Self, ModelError> {
        let mut field = Self::read_only(doc, root, revision)?;
        field.tracker = Some(Tracker::at(doc.committed_heads()));
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
    pub(crate) fn refresh(&mut self, doc: &Doc) -> Result<bool, ModelError> {
        let state = &self.state;
        let Some(tracker) = self.tracker.as_mut() else {
            return Err(ModelError::invalid("nothing tracked yet"));
        };
        let delta = tracker.committed_delta_located(doc, &|obj| RichText::locate(state, obj));
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

    fn reread(&mut self, doc: &Doc, root: ObjId) {
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

    fn sync(&mut self, doc: &Doc) {
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

    fn apply(&mut self, edits: &mut Edits<'_>, edit: &RichTextAction) -> Result<(), ActionError> {
        match &self.dispatch {
            Some(dispatch) => dispatch.apply_now(edits, edit),
            None => Err(ActionError::Refused(
                "this text is open to view only".into(),
            )),
        }
    }

    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        self.presence.clone()
    }
}

/// Makes an edit of a person record's rich-text field the record's action.
type Wrap<P> = Box<dyn Fn(&RichTextAction) -> <P as Resource>::Action>;

/// Rich text stored in a field of one of the person's own records
/// ([`PersonRecord`]): a draft, a private note. It follows the person to
/// their other devices and never reaches the room the app is open in.
///
/// `locate` finds the field's map in the record; `wrap` turns an edit of
/// it into the record's own action (a field edit at its path), so the
/// record's history and undo hold it. `start`, when given, is the action
/// that creates the field, applied before the first edit of a field that
/// does not exist yet.
pub struct PersonField<P: Resource> {
    record: PersonRecord<P>,
    locate: Locate,
    wrap: Wrap<P>,
    start: Option<Box<dyn Fn() -> Invoke<P>>>,
    field: Option<FieldState>,
    /// The record's heads when the field was last read.
    heads: Option<Vec<ui::automerge::ChangeHash>>,
}

impl<P: Resource> PersonField<P> {
    /// The rich text `locate` finds in `record`, each edit applied as the
    /// action `wrap` makes of it.
    pub fn new(
        record: PersonRecord<P>,
        locate: impl Fn(&Doc) -> Option<ObjId> + 'static,
        wrap: impl Fn(&RichTextAction) -> P::Action + 'static,
    ) -> Self {
        Self {
            record,
            locate: Box::new(locate),
            wrap: Box::new(wrap),
            start: None,
            field: None,
            heads: None,
        }
    }

    /// Create the field with `start` before its first edit.
    #[must_use]
    pub fn with_start(mut self, start: impl Fn() -> Invoke<P> + 'static) -> Self {
        self.start = Some(Box::new(start));
        self
    }

    /// Create the field when it does not exist yet, so an editor has a
    /// document to write in before the first keystroke.
    ///
    /// # Errors
    /// The record refused the start action.
    pub fn ensure(&mut self) -> Result<(), ActionError> {
        self.refresh();
        if self.field.is_none()
            && let Some(start) = &self.start
        {
            self.record.invoke(&start())?;
            self.refresh();
        }
        Ok(())
    }

    /// Read the field again from the record when the record moved: an
    /// edit here, or one from the person's other device.
    fn refresh(&mut self) {
        let heads = self.record.record().heads();
        if self.heads.as_ref() == Some(&heads) && self.field.is_some() {
            return;
        }
        let revision = self
            .field
            .as_ref()
            .map_or(0, |field| field.revision.wrapping_add(1));
        let read = self.record.record().read(|doc| {
            let root = (self.locate)(doc)?;
            FieldState::read_only(doc, root, revision).ok()
        });
        self.field = read.flatten();
        self.heads = Some(heads);
    }
}

impl<P: Resource> RichTextSource for PersonField<P> {
    fn read_only(&self) -> bool {
        !self.record.editable()
    }

    fn sync(&mut self, _doc: &Doc) {
        self.refresh();
    }

    fn revision(&self) -> Option<u64> {
        self.field.as_ref().map(|field| field.revision)
    }

    fn read_from(&mut self, _doc: &Doc) -> bool {
        self.refresh();
        self.field.is_some()
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.field.as_ref().map(|field| read(&field.state))
    }

    fn apply(&mut self, _edits: &mut Edits<'_>, edit: &RichTextAction) -> Result<(), ActionError> {
        self.ensure()?;
        self.record.apply(&(self.wrap)(edit))?;
        self.refresh();
        Ok(())
    }
}

/// A shared handle, for an app that keeps the source to change it later
/// (its permissions) while the surface holds it.
impl<S: RichTextSource> RichTextSource for Rc<std::cell::RefCell<S>> {
    fn read_only(&self) -> bool {
        self.borrow().read_only()
    }

    fn sync(&mut self, doc: &Doc) {
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

    fn apply(&mut self, edits: &mut Edits<'_>, edit: &RichTextAction) -> Result<(), ActionError> {
        self.borrow_mut().apply(edits, edit)
    }

    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        self.borrow().presence_keys()
    }
}
