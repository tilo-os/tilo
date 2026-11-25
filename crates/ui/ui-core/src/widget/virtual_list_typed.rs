//! A model's list, typed by the model and its keys ([`VirtualList`]), over the list's own code
//! ([`VirtualRows`]), which reads every model through [`AnyModel`] and keeps every key as a
//! [`ListKey`]: that code is compiled once in the toolkit rather than again in each app for
//! each model it lists.

use std::any::Any;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ops::Range;
use std::rc::Rc;

use vello::kurbo::{Point, Rect};

use super::{ListModel, RowBuild, TreeMarks, TreeRow, VecListModel, VirtualRows};
use crate::arriving::Arrival;
use crate::collection_transition::ListModelChange;
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::selection::KeySelection;
use crate::widget::{Element, MenuItem, RowGroup, Widget};

trait KeyValue {
    fn as_any(&self) -> &dyn Any;
    fn same(&self, other: &dyn KeyValue) -> bool;
    fn hash_into(&self, state: &mut dyn Hasher);
}

impl<K: Eq + Hash + 'static> KeyValue for K {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same(&self, other: &dyn KeyValue) -> bool {
        other.as_any().downcast_ref::<K>() == Some(self)
    }

    fn hash_into(&self, mut state: &mut dyn Hasher) {
        self.hash(&mut state);
    }
}

/// A row's key as the list keeps it, whatever its model's key type.
#[derive(Clone)]
pub struct ListKey(Rc<dyn KeyValue>);

impl ListKey {
    /// The key as the list keeps it; a [`ListKey`] stays itself.
    pub fn new<K: Eq + Hash + 'static>(key: K) -> Self {
        match (&key as &dyn Any).downcast_ref::<Self>() {
            Some(kept) => kept.clone(),
            None => Self(Rc::new(key)),
        }
    }

    pub fn of<K: Clone + Eq + Hash + 'static>(key: &K) -> Self {
        Self::new(key.clone())
    }

    /// The key as its model wrote it.
    #[must_use]
    pub fn get<K: 'static>(&self) -> Option<&K> {
        self.0.as_any().downcast_ref()
    }

    pub(super) fn typed<K: Clone + 'static>(&self) -> K {
        self.as_key::<K>().clone()
    }

    #[expect(clippy::expect_used, reason = "a list keeps only its model's keys")]
    fn as_key<K: 'static>(&self) -> &K {
        self.get().expect("a list keeps only its model's keys")
    }
}

impl PartialEq for ListKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.same(&*other.0)
    }
}

impl Eq for ListKey {}

impl Hash for ListKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash_into(state);
    }
}

impl std::fmt::Debug for ListKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ListKey")
    }
}

/// A list's model as [`VirtualRows`] reads it: keyed by [`ListKey`].
pub trait AnyModel: ListModel<Key = ListKey> {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

struct Typed<M>(M);

fn tree_row<K: Eq + Hash + 'static>(row: TreeRow<K>) -> TreeRow<ListKey> {
    TreeRow {
        parent: row.parent.map(ListKey::new),
        depth: row.depth,
        above: row.above.map(ListKey::new),
    }
}

fn change<K: Clone + Eq + Hash + 'static>(change: &ListModelChange<K>) -> ListModelChange<ListKey> {
    match change {
        ListModelChange::RowsInserted { index, count } => ListModelChange::RowsInserted {
            index: *index,
            count: *count,
        },
        ListModelChange::RowsRemoved { index, count } => ListModelChange::RowsRemoved {
            index: *index,
            count: *count,
        },
        ListModelChange::RowsMoved { from, to, count } => ListModelChange::RowsMoved {
            from: *from,
            to: *to,
            count: *count,
        },
        ListModelChange::RowsChanged { keys } => ListModelChange::RowsChanged {
            keys: keys.iter().cloned().map(ListKey::new).collect(),
        },
        ListModelChange::Reset => ListModelChange::Reset,
    }
}

macro_rules! to_model {
    (@one ref $name:ident($($arg:ident: $ty:ty),*) $($answer:ty)?) => {
        fn $name(&self $(, $arg: $ty)*) $(-> $answer)? {
            self.0.$name($($arg),*)
        }
    };
    (@one mut $name:ident($($arg:ident: $ty:ty),*) $($answer:ty)?) => {
        fn $name(&mut self $(, $arg: $ty)*) $(-> $answer)? {
            self.0.$name($($arg),*)
        }
    };
    ($($kind:tt $name:ident($($arg:ident: $ty:ty),*) $(-> $answer:ty)?;)*) => {
        $(to_model!(@one $kind $name($($arg: $ty),*) $($answer)?);)*
    };
}

impl<M: ListModel + 'static> ListModel for Typed<M> {
    type Key = ListKey;

    to_model! {
        ref len() -> usize;
        ref is_empty() -> bool;
        ref row_state(index: usize) -> Arrival<(), &str>;
        ref can_retry(index: usize) -> bool;
        mut retry(index: usize);
        mut poll_changed() -> bool;
        ref needs_poll() -> bool;
        ref label(index: usize) -> Option<String>;
        ref group_at(index: usize) -> Option<RowGroup>;
        ref done(index: usize) -> bool;
        ref accepts_children(index: usize) -> bool;
        ref renames(index: usize) -> bool;
        ref has_subtasks(index: usize) -> bool;
        ref discloses() -> bool;
        ref opens_in_place(index: usize) -> bool;
    }

    fn key_at(&self, index: usize) -> Option<ListKey> {
        self.0.key_at(index).map(ListKey::new)
    }

    fn index_of(&self, key: &ListKey) -> Option<usize> {
        self.0.index_of(key.get()?)
    }

    fn request_range(&mut self, range: Range<usize>, open: Option<&ListKey>) {
        self.0.request_range(range, open.and_then(ListKey::get));
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<ListKey>> {
        self.0.poll_changes().iter().map(change).collect()
    }

    fn key_presenting(&self, id: &str) -> Option<ListKey> {
        self.0.key_presenting(id).map(ListKey::new)
    }

    fn item_presented(&self, key: &ListKey) -> Option<String> {
        self.0.item_presented(key.get()?)
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<ListKey>> {
        self.0.tree_at(index).map(tree_row)
    }
}

impl<M: ListModel + 'static> AnyModel for Typed<M> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[expect(
    clippy::expect_used,
    reason = "a list reads only the model it was made with"
)]
fn model<M: 'static>(model: &dyn AnyModel) -> &M {
    &model
        .as_any()
        .downcast_ref::<Typed<M>>()
        .expect("a list reads the model it was made with")
        .0
}

#[expect(
    clippy::expect_used,
    reason = "a list reads only the model it was made with"
)]
fn model_mut<M: 'static>(model: &mut dyn AnyModel) -> &mut M {
    &mut model
        .as_any_mut()
        .downcast_mut::<Typed<M>>()
        .expect("a list reads the model it was made with")
        .0
}

/// A sparse, variable-height list of `M`'s rows: [`VirtualRows`], read as `M` and keyed by
/// its keys.
#[crate::component::not_a_component(
    part,
    "`ui.VirtualList` (`DescribedList`) is its described form"
)]
pub struct VirtualList<M: ListModel> {
    rows: VirtualRows,
    typed: PhantomData<fn() -> M>,
}

impl<M: ListModel> std::ops::Deref for VirtualList<M> {
    type Target = VirtualRows;

    fn deref(&self) -> &VirtualRows {
        &self.rows
    }
}

impl<M: ListModel> std::ops::DerefMut for VirtualList<M> {
    fn deref_mut(&mut self) -> &mut VirtualRows {
        &mut self.rows
    }
}

impl<M: ListModel + 'static> crate::widget::decorator::concrete::Decorator for VirtualList<M> {
    type Inner = VirtualRows;

    fn inner(&self) -> &VirtualRows {
        &self.rows
    }

    fn inner_mut(&mut self) -> &mut VirtualRows {
        &mut self.rows
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        self.rows.for_each_layout_child(visitor)
    }

    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        self.rows.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        self.rows.provide_mut(request);
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.rows.take_structure_change()
    }

    fn composition_follows_allocation(&self) -> bool {
        self.rows.composition_follows_allocation()
    }

    fn view_only_scroll(&self, event: &winit::event::WindowEvent) -> bool {
        self.rows.view_only_scroll(event)
    }

    fn accessibility_hidden(&self) -> bool {
        self.rows.accessibility_hidden()
    }

    fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
        self.rows.accessibility_shared()
    }

    fn accessibility_root(&self) -> Option<crate::a11y::AccessNode> {
        self.rows.accessibility_root()
    }

    fn mark_children_drawn(&mut self) {
        self.rows.mark_children_drawn();
    }
}

macro_rules! builders {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {$(
        #[must_use]
        pub fn $name(mut self, $($arg: $ty),*) -> Self {
            self.rows = self.rows.$name($($arg),*);
            self
        }
    )*};
}

macro_rules! by_key {
    (@one ref $name:ident($($arg:ident: $ty:ty),*) $answer:ty) => {
        pub fn $name(&self, key: &M::Key $(, $arg: $ty)*) -> $answer {
            self.rows.$name(&ListKey::of(key) $(, $arg)*)
        }
    };
    (@one mut $name:ident($($arg:ident: $ty:ty),*) $answer:ty) => {
        pub fn $name(&mut self, key: &M::Key $(, $arg: $ty)*) -> $answer {
            self.rows.$name(&ListKey::of(key) $(, $arg)*)
        }
    };
    ($($kind:tt $name:ident($($arg:ident: $ty:ty),*) => $answer:ty;)*) => {
        $(by_key!(@one $kind $name($($arg: $ty),*) $answer);)*
    };
}

macro_rules! by_setter {
    ($($name:ident = $set:ident($arg:ident: $ty:ty);)*) => {$(
        #[must_use]
        pub fn $name(mut self, $arg: $ty) -> Self {
            self.$set($arg);
            self
        }
    )*};
}

impl<M: ListModel + 'static> VirtualList<M> {
    pub fn new(
        model: M,
        estimated_row_height: f32,
        mut row_builder: impl FnMut(&M, usize, &M::Key) -> Element + 'static,
    ) -> Self {
        Self::build(
            model,
            estimated_row_height,
            false,
            move |model, index, key, _| row_builder(model, index, key),
        )
    }

    pub fn with_selection_delegate(
        model: M,
        estimated_row_height: f32,
        row_builder: impl FnMut(&M, usize, &M::Key, RowBuild) -> Element + 'static,
    ) -> Self {
        Self::build(model, estimated_row_height, true, row_builder)
    }

    fn build(
        model: M,
        estimated_row_height: f32,
        delegate: bool,
        mut row_builder: impl FnMut(&M, usize, &M::Key, RowBuild) -> Element + 'static,
    ) -> Self {
        let row_builder = Box::new(move |rows: &dyn AnyModel, index, key: &ListKey, build| {
            row_builder(self::model(rows), index, key.as_key(), build)
        });
        let mut rows = VirtualRows::build(
            Box::new(Typed(model)),
            estimated_row_height,
            row_builder,
            delegate,
        );
        rows.delegate_selection = delegate;
        Self {
            rows,
            typed: PhantomData,
        }
    }

    by_setter! {
        on_selected = set_on_selected(callback: impl FnMut(&[M::Key]) + 'static);
        with_selection = set_selection(selection: KeySelection<M::Key>);
        on_select = set_on_select(callback: impl FnMut(M::Key, bool) + 'static);
        on_activate = set_on_activate(callback: impl FnMut(M::Key) + 'static);
        on_opened = set_on_opened(callback: impl FnMut(M::Key) + 'static);
    }

    by_key! {
        mut reveal() => bool;
        mut begin_rename() => bool;
        mut fold_row(folded: bool) => bool;
        ref row_rect() => Option<Rect>;
        ref disclosure_rect() => Option<Rect>;
        ref row_depth() => u32;
        ref row_is_branch() => bool;
        ref row_folded() => bool;
        ref row_action_rect(action: usize) => Option<Rect>;
    }

    builders! {
        with_id(id: impl Into<String>);
        with_feel(feel: crate::motion::Feel);
        with_viewport_height(height: f32);
        overlay_scrollbar();
        flush();
        in_column();
        with_edges(edges: impl super::ListEdges + 'static);
        filling();
        fitting_rows();
        multi_select(enabled: bool);
        selection_follows_focus();
        reading();
        with_accessible_name(name: impl Into<String>);
        on_closed(callback: impl FnMut() + 'static);
        open_on_press();
        in_place(in_place: bool);
        rename_on_double_click();
        with_tail_title(title: impl Into<String>);
        in_view(title: impl Into<String>, count: Option<usize>);
        following_end();
        unread_from(item: Option<String>);
        with_indent_guides();
        tab_indents(enabled: bool);
    }

    #[must_use]
    pub fn model(&self) -> &M {
        model(self.rows.model())
    }

    pub fn update_model<R>(&mut self, update: impl FnOnce(&mut M) -> R) -> R {
        let anchor = self.rows.capture_anchor();
        let result = update(model_mut(&mut *self.rows.model));
        self.rows.model_updated(anchor.as_ref());
        result
    }

    pub fn update_model_changes<R>(
        &mut self,
        changes: &[ListModelChange<M::Key>],
        update: impl FnOnce(&mut M) -> R,
    ) -> R {
        let changes: Vec<_> = changes.iter().map(change).collect();
        let anchor = self.rows.capture_anchor();
        self.rows.capture_list_change(&changes);
        let result = update(model_mut(&mut *self.rows.model));
        self.rows.model_changed(&changes, anchor.as_ref());
        result
    }

    pub fn set_on_selected(&mut self, mut callback: impl FnMut(&[M::Key]) + 'static) {
        self.rows.set_on_selected(move |keys: &[ListKey]| {
            let keys: Vec<M::Key> = keys.iter().map(ListKey::typed).collect();
            callback(&keys);
        });
    }

    pub fn set_selection(&mut self, selection: KeySelection<M::Key>) {
        self.rows.set_selection(selection.map(ListKey::new));
    }

    /// The rows of the items a host chose for its person, each selected; an item the rows cannot
    /// answer for yet is held and selected as its row arrives.
    pub fn set_chosen(&mut self, items: Option<&[String]>) {
        self.rows.set_chosen(items);
    }

    #[must_use]
    pub fn selection(&self) -> KeySelection<M::Key> {
        self.rows.selection().clone().map(|key| key.typed())
    }

    /// Choose `key`'s row alone, or no row.
    pub fn select_only(&mut self, key: Option<M::Key>) {
        let selection = self.rows.selection_mut();
        match key {
            Some(key) => selection.select_only(ListKey::new(key)),
            None => selection.clear(),
        }
    }

    #[must_use]
    pub fn with_row_presence(
        mut self,
        presence: impl Fn(&M, usize, &M::Key) -> Option<crate::presence::PresenceKey> + 'static,
    ) -> Self {
        self.rows = self
            .rows
            .with_row_presence(move |rows, index, key| presence(model(rows), index, key.get()?));
        self
    }

    pub fn set_on_select(&mut self, mut callback: impl FnMut(M::Key, bool) + 'static) {
        self.rows
            .set_on_select(move |key: ListKey, chosen| callback(key.typed(), chosen));
    }

    pub fn set_on_activate(&mut self, mut callback: impl FnMut(M::Key) + 'static) {
        self.rows
            .set_on_activate(move |key: ListKey| callback(key.typed()));
    }

    pub fn set_on_revealed(&mut self, mut callback: impl FnMut(M::Key) + 'static) {
        self.rows
            .set_on_revealed(move |key: ListKey| callback(key.typed()));
    }

    /// Tell the view the person read the list to its end: once per arrival there.
    pub fn set_on_end_reached(&mut self, callback: Option<Box<dyn FnMut()>>) {
        self.rows.set_on_end_reached(callback);
    }

    pub fn set_on_opened(&mut self, mut callback: impl FnMut(M::Key) + 'static) {
        self.rows
            .set_on_opened(move |key: ListKey| callback(key.typed()));
    }

    /// Run a command the focused row declares (`Ctrl+Enter`), answering whether one did: the
    /// row answers the key however it is drawn, a list row and any other view alike.
    pub fn set_on_row_shortcut(
        &mut self,
        mut callback: impl FnMut(&M::Key, &crate::platform::Shortcut) -> bool + 'static,
    ) {
        self.rows
            .set_on_row_shortcut(move |key: &ListKey, pressed| {
                key.get().is_some_and(|key| callback(key, pressed))
            });
    }

    #[must_use]
    pub fn on_fold(mut self, mut callback: impl FnMut(&M::Key, bool) + 'static) -> Self {
        self.rows = self.rows.on_fold(move |key: &ListKey, folded| {
            if let Some(key) = key.get() {
                callback(key, folded);
            }
        });
        self
    }

    #[must_use]
    pub fn with_semantic_key(mut self, encode: impl Fn(&M::Key) -> String + 'static) -> Self {
        self.rows = self
            .rows
            .with_semantic_key(move |key: &ListKey| key.get().map(&encode).unwrap_or_default());
        self
    }

    #[must_use]
    pub fn with_reorder(
        mut self,
        mut validate: impl FnMut(&M, &M::Key, &DropTarget<M::Key>) -> bool + 'static,
        mut commit: impl FnMut(&mut M, ReorderDrop<M::Key>) -> bool + 'static,
    ) -> Self {
        self.rows = self.rows.with_reorder(
            move |rows, source: &ListKey, target: &DropTarget<ListKey>| {
                let target = DropTarget::new(target.key.typed(), target.placement);
                validate(model(rows), &source.typed(), &target)
            },
            move |rows, dropped: ReorderDrop<ListKey>| {
                let dropped = ReorderDrop {
                    source: dropped.source.typed(),
                    target: DropTarget::new(dropped.target.key.typed(), dropped.target.placement),
                };
                commit(model_mut(rows), dropped)
            },
        );
        self
    }

    #[must_use]
    pub fn reorder_target(&self) -> Option<DropTarget<M::Key>> {
        let target = self.rows.reorder_target()?;
        Some(DropTarget::new(target.key.typed(), target.placement))
    }

    /// [`VirtualRows::on_choose`], with the row's typed key.
    #[must_use]
    pub fn on_choose(mut self, id: impl Into<String>, mut f: impl FnMut(M::Key) + 'static) -> Self {
        self.rows = self.rows.on_choose(id, move |key: ListKey| f(key.typed()));
        self
    }

    #[must_use]
    pub fn on_rename(mut self, mut f: impl FnMut(M::Key, String) + 'static) -> Self {
        self.rows = self
            .rows
            .on_rename(move |key: ListKey, name| f(key.typed(), name));
        self
    }

    #[must_use]
    pub fn on_delete(mut self, mut f: impl FnMut(M::Key) + 'static) -> Self {
        self.rows = self.rows.on_delete(move |key: ListKey| f(key.typed()));
        self
    }

    #[must_use]
    pub fn with_context_menu(
        mut self,
        mut items: impl FnMut(&M, Option<&M::Key>) -> Vec<MenuItem> + 'static,
        mut chosen: impl FnMut(Option<M::Key>, usize) + 'static,
    ) -> Self {
        self.rows = self.rows.with_context_menu(
            move |rows, key: Option<&ListKey>| items(model(rows), key.and_then(ListKey::get)),
            move |key: Option<ListKey>, item| chosen(key.map(|key| key.typed()), item),
        );
        self
    }

    /// [`Self::with_context_menu`], where `offers` says which rows offer it: a row it does not
    /// name opens none, and assistive technology announces none for it.
    #[must_use]
    pub fn with_context_menu_offered_by(
        mut self,
        offers: impl Fn(&M, &M::Key) -> bool + 'static,
        items: impl FnMut(&M, Option<&M::Key>) -> Vec<MenuItem> + 'static,
        chosen: impl FnMut(Option<M::Key>, usize) + 'static,
    ) -> Self {
        self = self.with_context_menu(items, chosen);
        self.rows = self.rows.offering_rows(move |rows, key: &ListKey| {
            key.get().is_some_and(|key| offers(model(rows), key))
        });
        self
    }

    #[must_use]
    pub fn with_menu_around(mut self, offers: impl Fn(&M, &M::Key) -> bool + 'static) -> Self {
        self.rows = self.rows.with_menu_around(move |rows, key: &ListKey| {
            key.get().is_some_and(|key| offers(model(rows), key))
        });
        self
    }

    #[must_use]
    pub fn with_row_menu_reporter(
        mut self,
        mut report: impl FnMut(M::Key, Rect, Point) + 'static,
    ) -> Self {
        self.rows = self
            .rows
            .with_row_menu_reporter(move |key: ListKey, row, at| report(key.typed(), row, at));
        self
    }

    pub fn invalidate_rows(&mut self, keys: impl IntoIterator<Item = M::Key>) {
        self.rows
            .invalidate_rows(keys.into_iter().map(ListKey::new));
    }

    #[must_use]
    pub fn key_at_point(&self, point: Point) -> Option<M::Key> {
        Some(self.rows.key_at_point(point)?.typed())
    }

    #[must_use]
    pub fn open_row(&self) -> Option<&M::Key> {
        self.rows.open_row()?.get()
    }

    pub fn set_open(&mut self, key: Option<M::Key>) {
        self.rows.set_open(key.map(ListKey::new));
    }

    pub fn set_key_presenting(&mut self, f: impl Fn(&str) -> Option<M::Key> + 'static) {
        self.rows
            .set_key_presenting(move |id: &str| f(id).map(ListKey::new));
    }

    pub fn notify_model_changes(&mut self, changes: &[ListModelChange<M::Key>]) {
        let changes: Vec<_> = changes.iter().map(change).collect();
        self.rows.notify_model_changes(&changes);
    }

    #[must_use]
    pub fn with_tree(mut self, marks: Rc<TreeMarks<M::Key>>) -> Self {
        self.rows = self.rows.with_tree(Rc::clone(&marks.marks));
        self
    }

    pub fn focus_row(&mut self, key: M::Key) {
        self.rows.focus_row(ListKey::new(key));
    }

    /// Show the row `shown` names whenever it changes, without taking the keyboard: the
    /// list scrolls the row into view and stands it chosen. What a view asks when a press
    /// made the item the row shows, and the person is still typing where they made it.
    /// The ask is taken once: the list clears it, so a list built again shows its own
    /// place rather than asking for a row shown before.
    /// Show the row `key` names as the list is built: it scrolls into view at the list's motion,
    /// without taking the keyboard or the selection — what a view asks when it shows a row of its
    /// own, as a view of an address book does for the first card under a letter.
    #[must_use]
    pub fn revealing(mut self, item: impl Into<String>) -> Self {
        self.rows.scroll_reveal(item);
        self
    }

    /// Show the row of the item `item` names now, as [`Self::revealing`] does for a list being
    /// built.
    pub fn scroll_reveal(&mut self, item: impl Into<String>) {
        self.rows.scroll_reveal(item);
    }

    #[must_use]
    pub fn reveal_when(mut self, shown: &crate::LocalState<Option<M::Key>>) -> Self {
        let initial = shown.with(Clone::clone);
        if initial.is_some() {
            shown.set(None);
        }
        let asked = shown.clone();
        let mut observed = shown.revision();
        let poll = Box::new(move || {
            let revision = asked.revision();
            if revision == observed {
                return None;
            }
            observed = revision;
            let key = asked.with(Clone::clone);
            if key.is_some() {
                asked.set(None);
            }
            key.map(ListKey::new)
        });
        self.rows = self.rows.following_reveal(poll, initial.map(ListKey::new));
        self
    }

    #[must_use]
    pub fn focused_key(&self) -> Option<&M::Key> {
        self.rows.focused_key()?.get()
    }

    #[must_use]
    pub fn renaming(&self) -> Option<&M::Key> {
        self.rows.renaming()?.get()
    }

    pub fn open_context_menu(&mut self, key: Option<&M::Key>, at: Point) -> bool {
        let key = key.map(|key| ListKey::of(key));
        self.rows.open_context_menu(key.as_ref(), at)
    }

    pub fn open_row_menu(&mut self, key: M::Key, at: Point) -> bool {
        self.rows.open_row_menu(ListKey::new(key), at)
    }

    pub fn stage_reorder(&mut self, source: &M::Key, before: &M::Key) -> bool {
        self.rows
            .stage_reorder(&ListKey::of(source), &ListKey::of(before))
    }

    #[must_use]
    pub fn visible_key(&self, nth: usize) -> Option<M::Key> {
        Some(self.rows.visible_key(nth)?.typed())
    }
}

impl<T, K> VirtualList<VecListModel<T, K>>
where
    T: 'static,
    K: Clone + Eq + Hash + 'static,
{
    pub fn from_items(
        items: Vec<T>,
        estimated_row_height: f32,
        key: impl FnMut(&T) -> K,
        mut row: impl FnMut(&T, usize, &K) -> Element + 'static,
    ) -> Result<Self, super::DuplicateListKey> {
        let model = VecListModel::new(items, key)?;
        Ok(Self::new(
            model,
            estimated_row_height,
            move |model, index, key| {
                model.item(index).map_or_else(
                    || Box::new(crate::widget::Text::new("")) as Element,
                    |item| row(item, index, key),
                )
            },
        ))
    }

    #[must_use]
    pub fn reorderable(self) -> Self {
        self.with_reorder(
            |_, source, target| target.placement != DropPlacement::Inside && source != &target.key,
            |model, dropped| {
                let Some(from) = model.index_of(&dropped.source) else {
                    return false;
                };
                let Some(target) = model.index_of(&dropped.target.key) else {
                    return false;
                };
                let boundary = match dropped.target.placement {
                    DropPlacement::Before => target,
                    DropPlacement::After => target.saturating_add(1),
                    DropPlacement::Inside => return false,
                };
                let to = if boundary > from {
                    boundary.saturating_sub(1)
                } else {
                    boundary
                };
                model.move_item(from, to)
            },
        )
    }
}
