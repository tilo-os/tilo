//! Marks in place (task-legibility.md §3): what someone else changed that the person looking
//! has not seen, drawn by the host where the view shows it, with no code of the app's.
//!
//! The host knows what each part of a view is bound to: a `ui.ListItem` row presents its item,
//! a bound control edits a field, a window shows its resource. As each node of a view is built,
//! the host reads [`crate::seen::unseen`] there and marks it:
//!
//! - a row gets a `ui.ChangeMark` in its `change` slot: who, in which task run; a row a list
//!   folds away as done with stands with the rows above while the change is unseen, so the mark
//!   is read where it happened;
//! - a bound text field takes the changer's colour (`changed_by`), and says what it held before
//!   the change (`previous`);
//! - a window's header holds a mark with how many changes of what it shows are unseen, and by
//!   whom.
//!
//! A view that places `ui.ChangeMark(r)` itself has it filled in here, and the host places no
//! mark of its own for `r` in that view. Every mark activates to the run that made the change
//! ([`crate::host_ui::REVIEW_ACTION`]). What a mark read is the boundary's reads, so it moves
//! when the versions or the person's seen marks do.

use crate::core::Core;
use crate::error::RuntimeError;
use crate::ids::Principal;
use crate::render::Acc;
use crate::store::{FieldSpec, FieldValue, Scalar, Version};
use crate::value::{ActionRef, CollRef, ResourceRef, Value};
use crate::view::UiNode;

/// The node kind of a change mark.
pub const CHANGE_MARK: &str = "ui.ChangeMark";
/// The node kind of a row of a list.
const LIST_ITEM: &str = "ui.ListItem";
/// The node kind of a window's shell.
const NAV_SHELL: &str = "ui.NavShell";
/// The node kind of a text field.
const TEXT_INPUT: &str = "ui.TextInput";
/// The node kind of a place in a navigation list.
const NAV_ITEM: &str = "ui.NavItem";
/// The kind of a node holding a component's named slot.
const SLOT: &str = "slot";
/// The node kind of a list of any length, whose rows are its items'.
const VIRTUAL_LIST: &str = "ui.VirtualList";
/// The node kind of a list row.
const LIST_ROW: &str = "ui.ListRow";
/// How many resources of a window's document a window's mark reads changes of: one page of
/// them, so a window of a workspace of any size costs the same to open, and says when the
/// document holds more.
const WINDOW_PAGE: usize = 128;

/// The pseudo-field a mark reads versions through: of a resource, every version of it, and of
/// a document, every version of every resource in it. A mark reads what a step made, not only
/// the values the step wrote, so a step that makes a version of a resource reaches it even when
/// the value reads as the one already there, and the window's count moves when any of its
/// document's resources changes.
pub const DOC_VERSIONS: &str = "$versions";

impl Core {
    /// Mark `node` for `viewer` where it shows something they have not seen.
    ///
    /// # Errors
    ///
    /// The store could not read the versions of what the node shows.
    pub(crate) fn mark_unseen(
        &self,
        node: &mut UiNode,
        (viewer, window): (&Principal, &str),
        listed: Option<&CollRef>,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        match node.kind.as_str() {
            VIRTUAL_LIST => match listed {
                Some(coll) => self.mark_removed(node, viewer, coll, acc),
                None => Ok(()),
            },
            CHANGE_MARK => {
                if let Some(Value::Ref(r)) = node.props.get("of").cloned()
                    && let Some(unseen) = self.newest_unseen_noted(viewer, &r.target, acc)?
                {
                    self.fill_mark(node, &r.target, std::slice::from_ref(&unseen), acc);
                }
                Ok(())
            }
            LIST_ITEM => {
                let Some(Value::Ref(r)) = node.props.get("item").cloned() else {
                    return Ok(());
                };
                let Some(unseen) = self.newest_unseen_noted(viewer, &r.target, acc)? else {
                    return Ok(());
                };
                // The row does not hide in a list's folded tail while a change of it is
                // unseen: it stands with the rows above, where its mark is read, and goes
                // back under the tail once the person has seen it. A flag travels as the
                // integer it was boxed as, or as the flag itself.
                if matches!(
                    node.props.get("tail"),
                    Some(Value::Bool(true) | Value::Int(1))
                ) {
                    node.props.insert("tail".to_owned(), Value::Bool(false));
                }
                if places_mark(node, &r.target.id_string()) {
                    return Ok(());
                }
                let mut mark = UiNode::empty(CHANGE_MARK, &format!("{}~change", node.key));
                mark.props
                    .insert("of".to_owned(), Value::resource(&r.target));
                self.fill_mark(&mut mark, &r.target, std::slice::from_ref(&unseen), acc);
                node.children.push(slot("change", &node.key, mark));
                Ok(())
            }
            NAV_ITEM => self.mark_nav_item(node, viewer, acc),
            NAV_SHELL => self.mark_window(node, viewer, window, acc),
            TEXT_INPUT => self.mark_field(node, viewer, acc),
            _ => Ok(()),
        }
    }

    /// A list of the collection `coll`: a row after its items for each item someone else
    /// removed from it that `viewer` has not seen go, its title as it was struck through, its
    /// mark saying who removed it in which run and showing that run.
    fn mark_removed(
        &self,
        node: &mut UiNode,
        viewer: &Principal,
        coll: &CollRef,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        acc.reads
            .push((coll.target.doc.clone(), DOC_VERSIONS.to_owned()));
        let removed = match self
            .store
            .borrow()
            .removed(&coll.target, &coll.field, &coll.item)
        {
            Ok(removed) => removed,
            Err(crate::store::StoreError::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let runs = self.seen_runs(&viewer.0);
        // The field the items' type is named by, which a removed item's row shows.
        let title = {
            let registry = self.registry.borrow();
            registry.type_info(&coll.item).and_then(|(_, info)| {
                info.name_field().map(|f| FieldSpec {
                    id: f.id.clone(),
                    kind: f.kind,
                })
            })
        };
        let mut rows = Vec::new();
        for (resource, removal) in removed {
            self.note_seen_reads(viewer, &resource, acc);
            let mark = self.seen_mark(&viewer.0, &resource);
            let unseen = crate::seen::unseen(
                std::slice::from_ref(&removal),
                &viewer.0,
                mark.as_ref(),
                &runs,
            );
            if unseen.is_empty() {
                continue;
            }
            let Some(at) = ResourceRef::parse_id(&resource, coll.item.clone()) else {
                continue;
            };
            let said = match &title {
                Some(spec) => text_of(&self.store.borrow().read_before(
                    &at,
                    spec,
                    &removal.change,
                )?),
                None => None,
            };
            let key = format!("{}~removed~{resource}", node.key);
            let mut row = UiNode::empty(LIST_ROW, &format!("{key}~row"));
            row.props
                .insert("title".to_owned(), Value::Text(said.unwrap_or_default()));
            let mut item = UiNode::empty(LIST_ITEM, &key);
            item.props.insert("removed".to_owned(), Value::Bool(true));
            item.children.push(row);
            let mut mark = UiNode::empty(CHANGE_MARK, &format!("{key}~change"));
            mark.props.insert("of".to_owned(), Value::resource(&at));
            self.fill_mark(&mut mark, &at, &unseen, acc);
            item.children.push(slot("change", &key, mark));
            rows.push(item);
        }
        if !rows.is_empty() {
            let mut removed = UiNode::empty(SLOT, &format!("{}~slot-removed", node.key));
            removed
                .props
                .insert("name".to_owned(), Value::Text("removed".to_owned()));
            removed.children = rows;
            node.children.push(removed);
        }
        Ok(())
    }

    /// The boundary reads every mark of `resource` makes: the person's seen mark of it and the
    /// runs they reviewed, the record's versions of it, and each of its fields.
    fn note_unseen_reads(&self, viewer: &Principal, at: &ResourceRef, acc: &mut Acc) {
        let id = at.id_string();
        self.note_seen_reads(viewer, &id, acc);
        // A step that makes a version of it reaches the mark even when the value it writes
        // reads as the one already there ([`DOC_VERSIONS`]).
        acc.reads.push((id.clone(), DOC_VERSIONS.to_owned()));
        // Any field of it changing is a new version. A resource of no type the runtime knows
        // (one removed) is read as its versions alone.
        if let Ok((_, info)) = self.type_of_ref(&at.ty) {
            for field in &info.fields {
                acc.reads.push((id.clone(), field.id.clone()));
            }
        }
    }

    /// Whether `person` has seen `version` of `resource`: it is their own, or the one their seen
    /// mark of the resource, or of what it was made from and what holds it, names. A mark of a
    /// resource whose newest version is such a one has nothing unseen to show, and says so
    /// without reading the resource's log.
    fn seen_before(&self, person: &str, at: &ResourceRef, version: &Version) -> bool {
        !crate::seen::is_marked(version, person)
            || self
                .seen_mark_of_tree(person, at)
                .is_some_and(|mark| mark.change == version.change)
    }

    /// The newest version of `at` that `viewer` has not seen, if any, read by the boundary
    /// being built: what a row's mark says, answered by the index's newest version of the
    /// resource without its log.
    fn newest_unseen_noted(
        &self,
        viewer: &Principal,
        at: &ResourceRef,
        acc: &mut Acc,
    ) -> Result<Option<Version>, RuntimeError> {
        self.note_unseen_reads(viewer, at, acc);
        Ok(self.newest_unseen_of(&viewer.0, at)?)
    }

    /// The boundary read `viewer`'s seen mark of `resource` and the runs they reviewed.
    fn note_seen_reads(&self, viewer: &Principal, resource: &str, acc: &mut Acc) {
        let states = self.view_states.borrow();
        for key in [
            crate::seen::seen_key(&viewer.0, resource),
            crate::seen::runs_key(&viewer.0),
        ] {
            let generation = states.generation(&key);
            let value = states.get(&key).cloned().unwrap_or_else(Value::none);
            acc.state_reads.push((key.clone(), generation));
            acc.state_seen.push((key, value));
        }
    }

    /// Fill a change mark of `at` from what is unseen of it: who made the newest change, in
    /// which run, what it did, how many there are, everyone who made one of them, and what
    /// activating it does.
    fn fill_mark(&self, mark: &mut UiNode, at: &ResourceRef, unseen: &[Version], acc: &mut Acc) {
        let Some(newest) = unseen.last() else {
            return;
        };
        mark.props.insert(
            "by".to_owned(),
            Value::Principal(Principal(newest.who.clone())),
        );
        // Everyone who made one of the changes, newest first, each once: a window's mark shows
        // all of them, not only whoever happened to change it last.
        let mut by_all: Vec<Value> = Vec::new();
        for who in unseen.iter().rev().map(|v| &v.who) {
            if !by_all
                .iter()
                .any(|seen| matches!(seen, Value::Principal(seen) if &seen.0 == who))
            {
                by_all.push(Value::Principal(Principal(who.clone())));
            }
        }
        mark.props.insert("by_all".to_owned(), Value::List(by_all));
        // What the newest change did, in the words history says it with ([`Core::version_line`]):
        // a row says it under its title, and the catch-up lists it step by step.
        mark.props.insert(
            "said".to_owned(),
            Value::Text(self.version_line(newest, at)),
        );
        // What one item of it is called, plural, for a window's count: the type's own name. A
        // window's mark stands for the document's own resource, whose type only the store
        // names.
        let info = self
            .type_of_ref(&at.ty)
            .ok()
            .map(|(_, info)| info)
            .or_else(|| {
                let ty = self.store.borrow().type_of(at).ok()?;
                let (_, info) = self.registry.borrow().type_info(&ty)?;
                Some(info)
            });
        if let Some(info) = info {
            let noun = tilolang::meta::plural(&tilolang::meta::in_words(&info.name));
            mark.props.insert("noun".to_owned(), Value::Text(noun));
        }
        if let Some(run) = &newest.task {
            mark.props
                .insert("run".to_owned(), Value::Text(run.title.clone()));
        }
        mark.props.insert(
            "kind".to_owned(),
            Value::Text(newest.kind.word().to_owned()),
        );
        mark.props.insert(
            "count".to_owned(),
            Value::Int(i64::try_from(unseen.len()).unwrap_or(i64::MAX)),
        );
        if let Some(run) = &newest.task
            && !mark.events.contains_key("activate")
        {
            let review = Value::Action(ActionRef::new(
                crate::host_ui::REVIEW_ACTION,
                vec![Value::Text(run.id.clone()), Value::resource(at)],
            ));
            let retained = self.frames.borrow_mut().retain(review);
            acc.retained.push(retained);
            mark.events.insert("activate".to_owned(), retained);
        }
    }

    /// A text field bound to a field someone else changed unseen: their colour, and what it
    /// held before.
    fn mark_field(
        &self,
        node: &mut UiNode,
        viewer: &Principal,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        let Some((target, field)) = node
            .binding
            .as_ref()
            .and_then(|b| b.field())
            .map(|(t, f)| (t.clone(), f.clone()))
        else {
            return Ok(());
        };
        self.note_unseen_reads(viewer, &target, acc);
        // What last changed the field answers the mark when the person has seen it: nothing
        // else can be unseen of the field, and the resource's log is not read.
        let Some(last) = self.store.borrow().field_changed(&target, &field)? else {
            return Ok(());
        };
        if self.seen_before(&viewer.0, &target, &last) {
            return Ok(());
        }
        let unseen = self.unseen_of(&viewer.0, &target)?;
        let touching: Vec<&Version> = unseen
            .iter()
            .filter(|v| v.fields.contains(&field))
            .collect();
        let (Some(first), Some(newest)) = (touching.first(), touching.last()) else {
            return Ok(());
        };
        node.props.insert(
            "changed_by".to_owned(),
            Value::Principal(Principal(newest.who.clone())),
        );
        let (_, info) = self
            .type_of_ref(&target.ty)
            .map_err(RuntimeError::NotFound)?;
        let Some(spec) = info
            .fields
            .iter()
            .find(|f| f.id == field)
            .map(|f| FieldSpec {
                id: f.id.clone(),
                kind: f.kind,
            })
        else {
            return Err(RuntimeError::NotFound(format!("the field {field}")));
        };
        let before = self
            .store
            .borrow()
            .read_before(&target, &spec, &first.change)?;
        if let Some(text) = text_of(&before) {
            node.props.insert("previous".to_owned(), Value::Text(text));
        }
        Ok(())
    }

    /// A place in a navigation list that names a resource (`ui.NavItem(item: c)`, a channel):
    /// how much of it the person has not seen stands on the row, and the row's label lifts
    /// when anything does. What a sidebar shows without the app counting anything.
    fn mark_nav_item(
        &self,
        node: &mut UiNode,
        viewer: &Principal,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        let Some(Value::Ref(r)) = node.props.get("item").cloned() else {
            return Ok(());
        };
        self.note_unseen_reads(viewer, &r.target, acc);
        // Its newest version answers the row when the person has seen it: nothing is unseen of
        // it, and the resource's log is not read.
        let newest = self.store.borrow().newest(&r.target)?;
        if newest
            .as_ref()
            .is_none_or(|newest| self.seen_before(&viewer.0, &r.target, newest))
        {
            return Ok(());
        }
        let unseen = self.unseen_of(&viewer.0, &r.target)?;
        if unseen.is_empty() {
            return Ok(());
        }
        // A `live` the view set itself (a count of what it knows) is the view's; the host
        // fills only what the view left for it, as with a row's change mark.
        let count = i64::try_from(unseen.len()).unwrap_or(i64::MAX);
        node.props
            .entry("live".to_owned())
            .or_insert(Value::Int(count));
        node.props
            .entry("unread".to_owned())
            .or_insert(Value::Bool(true));
        Ok(())
    }

    /// A window's header mark: how many changes of what it shows (its resource and everything
    /// in its document) are unseen, and who made the newest.
    fn mark_window(
        &self,
        node: &mut UiNode,
        viewer: &Principal,
        window: &str,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        // A window of no resource (a view of its own) shows nothing anyone changed.
        let Some(at) = ResourceRef::parse_id(window, String::new()) else {
            return Ok(());
        };
        acc.reads.push((at.doc.clone(), DOC_VERSIONS.to_owned()));
        // The newest version of a page of the document's resources: a window of a workspace of
        // any size reads no more of it than the page, and says when it holds more.
        let (within, more) = match self.store.borrow().newest_page(&at.doc, WINDOW_PAGE) {
            Ok(page) => page,
            // A window named for no document (one a host opened by a name of its own) shows
            // no resource anyone changed.
            Err(crate::store::StoreError::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let runs = self.seen_runs(&viewer.0);
        // The window of an item shows that item; the window of a document every item in it,
        // each counted once (the document's own versions are its collections moving as they
        // are made and removed).
        let shown = |resource: &str| match &at.item {
            Some(_) => resource == window,
            None => resource != at.doc,
        };
        let mut unseen: Vec<Version> = Vec::new();
        for (resource, newest) in within.into_iter().filter(|(r, _)| shown(r)) {
            self.note_seen_reads(viewer, &resource, acc);
            let mark = self.seen_mark(&viewer.0, &resource);
            // One per resource: its newest unseen change.
            if let Some(newest) = crate::seen::unseen(
                std::slice::from_ref(&newest),
                &viewer.0,
                mark.as_ref(),
                &runs,
            )
            .pop()
            {
                // A change a record made from `resource` stands under both, as that record's
                // own change and as a change to what it was made from: the window counts it
                // once, where the record that made it stands.
                if newest.resource == resource
                    || !unseen.iter().any(|other| other.change == newest.change)
                {
                    unseen.push(newest);
                }
            }
        }
        if unseen.is_empty() {
            return Ok(());
        }
        // Oldest first, the change's place in the document's log settling changes of one
        // moment, so the mark names everyone newest first.
        unseen.sort_by(|a, b| {
            a.at.cmp(&b.at)
                .then_with(|| change_index(a).cmp(&change_index(b)))
        });
        let mut mark = UiNode::empty(CHANGE_MARK, &format!("{}~change", node.key));
        mark.props.insert("of".to_owned(), Value::resource(&at));
        mark.props.insert("window".to_owned(), Value::Bool(true));
        // The document holds more resources than the page read: what it counted of them is a
        // lower bound, and the mark says so.
        if more {
            mark.props.insert("more".to_owned(), Value::Bool(true));
        }
        self.fill_mark(&mut mark, &at, &unseen, acc);
        // First among the header's actions, beside the title.
        match node.children.iter_mut().find(|child| {
            child.kind == SLOT
                && matches!(child.props.get("name"), Some(Value::Text(name)) if name == "actions")
        }) {
            Some(actions) => actions.children.insert(0, mark),
            None => node.children.push(slot("actions", &node.key, mark)),
        }
        Ok(())
    }
}

/// A slot `name` of the node `key` holding `child`.
fn slot(name: &str, key: &str, child: UiNode) -> UiNode {
    let mut slot = UiNode::empty(SLOT, &format!("{key}~slot-{name}"));
    slot.props
        .insert("name".to_owned(), Value::Text(name.to_owned()));
    slot.children.push(child);
    slot
}

/// Where a change stands in its document's log, as [`Version::change`] names it.
fn change_index(version: &Version) -> usize {
    version.change.parse().unwrap_or(0)
}

/// Whether the view placed a `ui.ChangeMark` of `resource` inside `node`.
fn places_mark(node: &UiNode, resource: &str) -> bool {
    node.children.iter().any(|child| {
        (child.kind == CHANGE_MARK
            && matches!(child.props.get("of"), Some(Value::Ref(r)) if r.target.id_string() == resource))
            || places_mark(child, resource)
    })
}

/// What a field held, as a person reads it.
fn text_of(value: &FieldValue) -> Option<String> {
    match value {
        FieldValue::Text(text) | FieldValue::Scalar(Scalar::Text(text)) => Some(text.clone()),
        FieldValue::Rich(rich) => Some(rich.written_text()),
        FieldValue::Scalar(Scalar::Int(n)) | FieldValue::Counter(n) => Some(n.to_string()),
        FieldValue::Scalar(Scalar::Float(n)) => Some(n.to_string()),
        FieldValue::Scalar(Scalar::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}
