//! Marks in place (task-legibility.md §3): what someone else changed that the person looking
//! has not seen, drawn by the host where the view shows it, with no code of the app's.
//!
//! The host knows what each part of a view is bound to: a `ui.ListItem` row presents its item,
//! a bound control edits a field, a window shows its resource. As each node of a view is built,
//! the host reads [`crate::seen::unseen`] there and marks it:
//!
//! - a row gets a `ui.ChangeMark` in its `change` slot: who, in which task run;
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
use crate::value::{ActionRef, ResourceRef, Value};
use crate::view::UiNode;

/// The node kind of a change mark.
pub const CHANGE_MARK: &str = "ui.ChangeMark";
/// The node kind of a row of a list.
const LIST_ITEM: &str = "ui.ListItem";
/// The node kind of a window's shell.
const NAV_SHELL: &str = "ui.NavShell";
/// The node kind of a text field.
const TEXT_INPUT: &str = "ui.TextInput";
/// The kind of a node holding a component's named slot.
const SLOT: &str = "slot";

/// The pseudo-field a window's mark reads of its document: every version of every resource in
/// it, so the count moves when any of them changes.
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
        viewer: &Principal,
        window: &str,
        acc: &mut Acc,
    ) -> Result<(), RuntimeError> {
        match node.kind.as_str() {
            CHANGE_MARK => {
                if let Some(Value::Ref(r)) = node.props.get("of").cloned() {
                    let unseen = self.unseen_noted(viewer, &r.target, acc)?;
                    self.fill_mark(node, &r.target, &unseen, acc);
                }
                Ok(())
            }
            LIST_ITEM => {
                let Some(Value::Ref(r)) = node.props.get("item").cloned() else {
                    return Ok(());
                };
                if places_mark(node, &r.target.id_string()) {
                    return Ok(());
                }
                let unseen = self.unseen_noted(viewer, &r.target, acc)?;
                if unseen.is_empty() {
                    return Ok(());
                }
                let mut mark = UiNode::empty(CHANGE_MARK, &format!("{}~change", node.key));
                mark.props
                    .insert("of".to_owned(), Value::resource(&r.target));
                self.fill_mark(&mut mark, &r.target, &unseen, acc);
                node.children.push(slot("change", &node.key, mark));
                Ok(())
            }
            NAV_SHELL => self.mark_window(node, viewer, window, acc),
            TEXT_INPUT => self.mark_field(node, viewer, acc),
            _ => Ok(()),
        }
    }

    /// The versions of `at` `viewer` has not seen, read by the boundary being built.
    fn unseen_noted(
        &self,
        viewer: &Principal,
        at: &ResourceRef,
        acc: &mut Acc,
    ) -> Result<Vec<Version>, RuntimeError> {
        let id = at.id_string();
        self.note_seen_reads(viewer, &id, acc);
        // Any field of it changing is a new version. A resource of no type the runtime knows
        // (one removed) is read as its versions alone.
        if let Ok((_, info)) = self.type_of_ref(&at.ty) {
            for field in &info.fields {
                acc.reads.push((id.clone(), field.id.clone()));
            }
        }
        Ok(self.unseen_of(&viewer.0, at)?)
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
    /// which run, what it did, how many there are, and what activating it does.
    fn fill_mark(&self, mark: &mut UiNode, at: &ResourceRef, unseen: &[Version], acc: &mut Acc) {
        let Some(newest) = unseen.last() else {
            return;
        };
        mark.props.insert(
            "by".to_owned(),
            Value::Principal(Principal(newest.who.clone())),
        );
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
        let unseen = self.unseen_noted(viewer, &target, acc)?;
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
        let within = match self.store.borrow().versions_within(&at.doc) {
            Ok(within) => within,
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
        for (resource, versions) in within.into_iter().filter(|(r, _)| shown(r)) {
            self.note_seen_reads(viewer, &resource, acc);
            let mark = self.seen_mark(&viewer.0, &resource);
            // One per resource: its newest unseen change.
            if let Some(newest) =
                crate::seen::unseen(&versions, &viewer.0, mark.as_ref(), &runs).pop()
            {
                unseen.push(newest);
            }
        }
        if unseen.is_empty() {
            return Ok(());
        }
        unseen.sort_by(|a, b| a.at.cmp(&b.at));
        let mut mark = UiNode::empty(CHANGE_MARK, &format!("{}~change", node.key));
        mark.props.insert("of".to_owned(), Value::resource(&at));
        mark.props.insert("window".to_owned(), Value::Bool(true));
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
