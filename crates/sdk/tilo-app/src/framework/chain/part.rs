//! What a chain needs of its parts' resource, as one table of functions per
//! resource: the only code a chain compiles for each part type. The parts'
//! states, the app's resource and the actions reach these functions as `dyn
//! Any`, and each reads them back as the resource's own types.

use std::any::Any;
use std::rc::Rc;

use std::collections::{BTreeMap, HashSet};

use tilo_actions::{ActionError, ActionInvoker, ActionNeed, EncodedAction};
use tilo_resource::{Reader, Refusal, Resource};
use ui_core::automerge::{AutoCommit, ChangeHash};
use ui_core::resource::{DocumentId, ResourceStore};

use super::super::history::PartEdit;
use super::super::live::Live;
use super::Chained;
use super::listing::Filter;

/// The parts a typed function reads: `Vec<(u64, Rc<P::State>)>`, oldest first.
pub(super) type TypedParts = Box<dyn Any>;

/// A part's state, read as its resource's `State`.
type State = Rc<dyn Any>;

/// An action of the parts' resource.
type Action = Box<dyn Any>;

/// Where an action happens, and the part the app places it in, when it
/// places it itself ([`Chained::part_for`]).
type Route = (Option<String>, Option<Result<u64, String>>);

type ReadPart = fn(&ResourceStore, &DocumentId) -> Result<Box<dyn HeldPart>, String>;
type Prepare = fn(&[&dyn Any], &dyn Any, &dyn Any, &ActionInvoker) -> Result<Vec<Action>, Refusal>;
type RouteOf = fn(&dyn Any, &dyn Any, &dyn Any) -> Route;
type Written =
    for<'a> fn(&'a dyn Any) -> Result<(&'a dyn tilo_actions::Action, EncodedAction), ActionError>;
type Group = fn(Vec<Action>) -> Result<(Box<dyn tilo_actions::Action>, EncodedAction), ActionError>;
type Carry = fn(u64, &dyn Any) -> Option<Result<Action, String>>;
/// [`Chained::tree`] of the parts held: each item's parent among them.
type PartsTree = fn(&dyn Any) -> Vec<(tilo_resource::Id, Option<tilo_resource::Id>)>;

/// Told of a write as it lands, with the version it was made on and the ids it minted.
pub(super) type OnWritten<'a> = dyn FnMut(&mut AutoCommit, &[ChangeHash], &[String]) + 'a;
type Answer = fn(&AutoCommit, &str, &[ui_core::automerge::Patch]) -> Option<serde_json::Value>;
type Spread = fn(&dyn Any, u64, &dyn Any) -> Vec<(u64, Action)>;
type Shares = Result<Option<Vec<(u64, Action)>>, String>;
type Content = fn(&dyn Any, &AutoCommit, Option<&str>) -> Result<serde_json::Value, String>;

/// An action as it was invoked or read back: one of the parts' resource, or
/// several invoked as one.
pub(super) enum Decoded {
    One(Action),
    Group(Vec<Action>),
}

/// What [`super::ChainCore`] and [`super::ChainState`] need of the parts'
/// resource.
pub(super) struct PartType {
    pub(super) kind: &'static str,
    pub(super) label: &'static str,
    pub(super) held_whole: bool,
    pub(super) table: Option<&'static tilo_resource::table::ModelTable>,
    pub(super) initialize: fn(&mut AutoCommit) -> Result<(), String>,
    /// The part's type, which its records are founded by.
    pub(super) surface: fn() -> tilo_resource::surface::Surface,
    /// Read the part a document holds, brought up to the resource's storage
    /// version first.
    pub(super) read: ReadPart,
    /// The state `doc` holds, read as a part.
    pub(super) state: fn(&AutoCommit) -> Option<State>,
    /// The item `id` of a part's state, built alone: what a path naming one
    /// costs, however many items the part holds ([`Resource::child`]).
    pub(super) child: fn(&dyn Any, &str) -> Option<tilo_actions::ResourceSpec>,
    /// View state of the parts held: a `LocalState<Parts<P>>`.
    pub(super) new_view: fn() -> Box<dyn Any>,
    /// Set what [`Self::new_view`] made to the parts [`Self::typed`] read.
    pub(super) set_view: fn(&dyn Any, TypedParts),
    pub(super) typed: fn(&[(u64, State)]) -> TypedParts,
    /// A copy of `action` read against the app's resource and the parts held
    /// ([`Chained::prepare`]).
    pub(super) prepare: Prepare,
    /// Where in the app's resource a prepared action happens
    /// ([`Chained::place`]), and the part it changes ([`Chained::part_for`]).
    pub(super) route: RouteOf,
    /// Each item a part's state keeps.
    pub(super) each_item: super::place::EachItem,
    /// The item an action changes, or the one the new item it makes is
    /// kept beside ([`Chained::beside`]).
    pub(super) item: fn(&dyn Any) -> Option<tilo_resource::Id>,
    /// The items a verb over several lists ([`Resource::listed_items`]).
    pub(super) listed_items: fn(&dyn Any) -> Option<Vec<Option<tilo_resource::Id>>>,
    /// That verb over only the entries at the positions given.
    pub(super) keep_listed: fn(&dyn Any, &[usize]) -> Option<Action>,
    /// What an action is for the grants deciding it.
    pub(super) need: fn(&dyn Any) -> Option<ActionNeed>,
    pub(super) verb: fn(&dyn Any) -> Option<String>,
    /// Whether an action is a step someone takes, which Undo may take back.
    pub(super) is_step: fn(&dyn Any) -> bool,
    /// Whether the verb is declared to answer nothing.
    pub(super) answers_nothing: fn(&str) -> bool,
    /// What a verb written into a document answers its caller with
    /// ([`tilo_resource::Resource::answer`]); `None` when it answers nothing.
    pub(super) answer: Option<Answer>,
    /// The share of an action each part carries out ([`Chained::split`]).
    pub(super) split: fn(&dyn Any, &dyn Any) -> Shares,
    /// Further actions an action written into a part means for other parts
    /// ([`Chained::spread`]).
    pub(super) spread: Spread,
    /// The items the chain does not show ([`Chained::hidden`]).
    pub(super) hidden: fn(&dyn Any) -> HashSet<String>,
    /// What a read of an item answers, from a part's state and document.
    pub(super) content: Content,
    /// The same, from a past version of a part's document.
    pub(super) content_at: fn(&AutoCommit, Option<&str>) -> Result<serde_json::Value, String>,
    /// An action as it is applied and recorded.
    pub(super) written: Written,
    /// Prepared actions applied as one, as it is applied and recorded.
    pub(super) group: Group,
    /// What a part started after the part numbered first, whose state is
    /// second, carries of it ([`Chained::carry`]).
    pub(super) carry: Carry,
    /// An action the API delivered; refused when the app alone invokes it.
    pub(super) decode: fn(&EncodedAction) -> Result<Decoded, String>,
    /// An action as the list of waiting actions keeps it, and back.
    pub(super) from_json: fn(serde_json::Value) -> Result<Action, serde_json::Error>,
    pub(super) to_json: fn(&dyn Any) -> Result<Vec<u8>, ActionError>,
    pub(super) clone: fn(&dyn Any) -> Action,
    /// A history verb on a part, as the part's history applies it.
    pub(super) history: fn(&PartEdit) -> Box<dyn tilo_actions::Action>,
    /// What a part's state publishes, under the chain's address.
    pub(super) publish: fn(&str, &dyn Any) -> tilo_actions::ResourceSpec,
    /// The same without its children ([`tilo_resource::publish_head`]).
    pub(super) publish_head: fn(&str, &dyn Any) -> tilo_actions::ResourceSpec,
    /// A part's children in the parts a publisher keeps between versions
    /// ([`tilo_resource::Resource::children_parts`]).
    pub(super) children: fn(&dyn Any) -> tilo_resource::items::ChildrenParts,
    /// The parts' resource as a publisher of its items sees it.
    pub(super) resource: fn() -> super::super::root::ResourceType,
    /// [`Chained::places`] published, from the app's resource and the parts.
    pub(super) places: fn(&dyn Any, &dyn Any) -> Vec<tilo_actions::ResourceSpec>,
    pub(super) tree: PartsTree,
    pub(super) search: fn(&dyn Any) -> Vec<tilo_resource::SearchEntry>,
    /// What search finds in a part read with every field
    /// ([`tilo_resource::Reader::read_all`]), its reader kept in the first and
    /// brought up to the document: a part whose window reads fewer fields
    /// (`#[model(demanded)]`) is still found by every word it holds.
    pub(super) search_all: SearchAll,
    pub(super) search_over: fn(&dyn Any, &dyn Any) -> Vec<tilo_resource::SearchEntry>,
    pub(super) prepare_read: fn(&mut Filter, &dyn Any, &dyn Any) -> Result<(), String>,
    /// [`Chained::read_over`], from the app's resource and the parts.
    pub(super) read_over: fn(&dyn Any, &dyn Any, &str) -> ReadOver,
}

/// [`PartType::search_all`].
pub(super) type SearchAll =
    fn(&mut Option<Box<dyn Any>>, &AutoCommit) -> Vec<tilo_resource::SearchEntry>;

/// What [`Chained::read_over`] answers: the item's content, or why not;
/// `None` for an item a part keeps.
type ReadOver = Option<Result<serde_json::Value, String>>;

/// The table of `P`, one for the whole program.
pub(super) struct Of<P>(std::marker::PhantomData<P>);

impl PartType {
    /// What a write on a part of this type answers `answering` with, made on
    /// `before` and minting `minted`. The answer names the text it wrote,
    /// whatever else the verb answers: where it landed is known here, even for
    /// a verb that named its item by what it says. A write that made items
    /// names what it made beside the answer, not instead of it. Only a write
    /// that made items and answers nothing needs neither, and skips the diff:
    /// the changes are worked out only for what reads them.
    pub(super) fn answered(
        &self,
        answering: Option<&str>,
        doc: &mut AutoCommit,
        before: &[ChangeHash],
        minted: &[String],
    ) -> Option<serde_json::Value> {
        let answer = answering.zip(self.answer);
        let wrote = answer.is_some() || minted.is_empty();
        if answer.is_none() && !wrote {
            return None;
        }
        let after = doc.get_heads();
        let changes = doc.diff(before, &after);
        let value = answer.and_then(|(verb, answer)| answer(doc, verb, &changes));
        match wrote
            .then(|| tilo_doc::written::written_by(doc, &changes))
            .flatten()
        {
            Some(wrote) => tilo_actions::ActionOutput::noting_wrote(value, &wrote),
            None => value,
        }
    }
}

impl<P: Chained> Of<P> {
    pub(super) const PART: PartType = PartType {
        kind: P::KIND,
        label: P::LABEL,
        held_whole: P::HELD_WHOLE,
        table: P::RESOURCE_TABLE,
        initialize: initialize::<P>,
        surface: <P as Resource>::surface,
        read: read_part::<P>,
        state: state::<P>,
        child: child::<P>,
        new_view: new_view::<P>,
        set_view: set_view::<P>,
        typed: typed::<P>,
        prepare: prepare::<P>,
        route: route::<P>,
        each_item: each_item::<P>,
        item: item::<P>,
        listed_items: listed_items::<P>,
        keep_listed: keep_listed::<P>,
        need: need::<P>,
        verb: verb::<P>,
        is_step: is_step::<P>,
        answers_nothing: answers_nothing::<P>,
        answer: if P::ANSWERS { Some(P::answer) } else { None },
        split: split::<P>,
        spread: spread::<P>,
        hidden: hidden::<P>,
        content: content::<P>,
        content_at: content_at::<P>,
        written: written::<P>,
        group: group::<P>,
        carry: carry::<P>,
        decode: decode::<P>,
        from_json: from_json::<P>,
        to_json: to_json::<P>,
        clone: clone::<P>,
        history: history::<P>,
        publish: publish::<P>,
        publish_head: publish_head::<P>,
        children: children::<P>,
        resource: super::super::root::ResourceType::of::<P>,
        places: places::<P>,
        tree: tree::<P>,
        search: search::<P>,
        search_all: search_all::<P>,
        search_over: search_over::<P>,
        prepare_read: prepare_read::<P>,
        read_over: read_over::<P>,
    };
}

/// A part held, followed as its document changes.
pub(super) trait HeldPart {
    fn sync(&self, doc: &tilo_resource::Doc) -> Result<bool, tilo_resource::ModelError>;
    fn take_change(&self);
    fn state(&self) -> Rc<dyn Any>;
    /// What the part's state holds: its revision and its loads
    /// ([`Live::revision`], [`Live::loads`]), which move with every change
    /// to what it says.
    fn held_at(&self) -> (u64, u64);
    fn as_any(&self) -> &dyn Any;
}

impl<R: Resource> HeldPart for Live<R> {
    fn sync(&self, doc: &tilo_resource::Doc) -> Result<bool, tilo_resource::ModelError> {
        Live::sync(self, doc)
    }

    fn take_change(&self) {
        let _ = Live::take_change(self);
    }

    fn state(&self) -> Rc<dyn Any> {
        self.get()
    }

    fn held_at(&self) -> (u64, u64) {
        (Live::revision(self), Live::loads(self))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `part` as a part of a chain of `P`.
pub(super) fn live_of<P: Chained>(part: &dyn HeldPart) -> Option<&Live<P>> {
    part.as_any().downcast_ref::<Live<P>>()
}

/// Why a value handed to a part type's function is not of that type: a
/// chain's state mixed up with another's.
fn mismatch<P: Chained>() -> String {
    format!("not of a chain of {}", P::LABEL)
}

fn action_of<P: Chained>(action: &dyn Any) -> Result<&P::Action, String> {
    action.downcast_ref().ok_or_else(mismatch::<P>)
}

fn base_of<P: Chained>(base: &dyn Any) -> Result<&<P::Base as Resource>::State, String> {
    base.downcast_ref().ok_or_else(mismatch::<P>)
}

fn parts_of<P: Chained>(parts: &dyn Any) -> &[(u64, Rc<P::State>)] {
    parts
        .downcast_ref::<Vec<(u64, Rc<P::State>)>>()
        .map_or(&[], Vec::as_slice)
}

fn initialize<P: Chained>(doc: &mut AutoCommit) -> Result<(), String> {
    P::initialize(doc).map_err(|error| error.to_string())
}

/// Part `part` of a chain of `P`, read from `store`. An older part is
/// brought up to the part's storage version before it is read, as the
/// app's resource is when it opens: once, as the same housekeeping change on
/// every device.
fn read_part<P: Chained>(
    store: &ResourceStore,
    part: &DocumentId,
) -> Result<Box<dyn HeldPart>, String> {
    let older = store
        .read(part, tilo_resource::evolution::needs_migrate::<P>)
        .ok_or("the part is not open here")?;
    if older {
        // Not while a frame reads the store: the part is held at the
        // next refresh, as a part this device asked for is.
        if !store.takes_writes() {
            return Err("the part is migrated once the store takes writes".into());
        }
        let workspace = store.workspace();
        store.migrate(part, |doc| {
            tilo_resource::founding::bring_up_resource::<P>(&workspace, part.route(), doc)
                .map_err(|error| error.to_string())
        })?;
    }
    let reader = store
        .read(part, Reader::<P>::read)
        .ok_or("the part is not open here")?
        .map_err(|error| error.to_string())?;
    Ok(Box::new(Live::new(reader)))
}

fn state<P: Chained>(doc: &AutoCommit) -> Option<Rc<dyn Any>> {
    match Reader::<P>::read(doc) {
        Ok(reader) => Some(Rc::clone(reader.state()) as Rc<dyn Any>),
        Err(error) => {
            tracing::warn!(kind = P::KIND, %error, "chain part unreadable");
            None
        }
    }
}

fn child<P: Chained>(state: &dyn Any, id: &str) -> Option<tilo_actions::ResourceSpec> {
    let state: &<P as Resource>::State = state.downcast_ref()?;
    P::child(state, id)
}

fn new_view<P: Chained>() -> Box<dyn Any> {
    Box::new(tilo_ui_base::LocalState::<super::Parts<P>>::new(
        Rc::default(),
    ))
}

fn set_view<P: Chained>(view: &dyn Any, parts: TypedParts) {
    if let (Some(view), Ok(parts)) = (
        view.downcast_ref::<tilo_ui_base::LocalState<super::Parts<P>>>(),
        parts.downcast::<Vec<(u64, Rc<P::State>)>>(),
    ) {
        view.set((*parts).into());
    }
}

fn typed<P: Chained>(parts: &[(u64, Rc<dyn Any>)]) -> TypedParts {
    let parts: Vec<(u64, Rc<P::State>)> = parts
        .iter()
        .filter_map(|(index, state)| Some((*index, Rc::clone(state).downcast().ok()?)))
        .collect();
    Box::new(parts)
}

fn prepare<P: Chained>(
    actions: &[&dyn Any],
    base: &dyn Any,
    parts: &dyn Any,
    invoker: &ActionInvoker,
) -> Result<Vec<Action>, Refusal> {
    let mut prepared = actions
        .iter()
        .map(|action| action_of::<P>(*action).cloned())
        .collect::<Result<Vec<_>, _>>()
        .map_err(Refusal::new)?;
    P::prepare(
        &mut prepared,
        base_of::<P>(base).map_err(Refusal::new)?,
        parts_of::<P>(parts),
        invoker,
    )?;
    Ok(prepared
        .into_iter()
        .map(|action| Box::new(action) as Action)
        .collect())
}

fn route<P: Chained>(action: &dyn Any, base: &dyn Any, parts: &dyn Any) -> Route {
    let action = match action_of::<P>(action) {
        Ok(action) => action,
        Err(error) => return (None, Some(Err(error))),
    };
    let place = base_of::<P>(base)
        .ok()
        .and_then(|base| P::place(action, base));
    (place, P::part_for(action, parts_of::<P>(parts)))
}

pub(super) fn each_item<P: Chained>(
    state: &dyn Any,
    found: &mut dyn FnMut(tilo_resource::Id, &dyn Any, &'static tilo_resource::table::ModelTable),
) {
    if let Some(table) = P::RESOURCE_TABLE {
        tilo_resource::stored_items::each_item(table, state, found);
    }
}

fn item<P: Chained>(action: &dyn Any) -> Option<tilo_resource::Id> {
    let action = action.downcast_ref::<P::Action>()?;
    P::beside(action).or_else(|| P::action_item(action))
}

fn listed_items<P: Chained>(action: &dyn Any) -> Option<Vec<Option<tilo_resource::Id>>> {
    P::listed_items(action.downcast_ref()?)
}

fn keep_listed<P: Chained>(action: &dyn Any, keep: &[usize]) -> Option<Action> {
    P::keep_listed(action.downcast_ref()?, keep).map(|action| Box::new(action) as Action)
}

fn need<P: Chained>(action: &dyn Any) -> Option<ActionNeed> {
    Some(tilo_resource::need_of::<P>(action.downcast_ref()?))
}

fn verb<P: Chained>(action: &dyn Any) -> Option<String> {
    super::super::workspace::verb_of::<P>(action.downcast_ref()?)
}

fn is_step<P: Chained>(action: &dyn Any) -> bool {
    action
        .downcast_ref::<P::Action>()
        .is_some_and(|action| tilo_actions::Action::change_kind(action).is_step())
}

fn answers_nothing<P: Chained>(verb: &str) -> bool {
    P::declarations()
        .iter()
        .any(|decl| decl.id == verb && decl.result.is_none())
}

fn split<P: Chained>(action: &dyn Any, parts: &dyn Any) -> Shares {
    let shares = P::split(action_of::<P>(action)?, parts_of::<P>(parts))?;
    Ok(shares.map(|shares| {
        shares
            .into_iter()
            .map(|(index, share)| (index, Box::new(share) as Action))
            .collect()
    }))
}

fn spread<P: Chained>(action: &dyn Any, index: u64, parts: &dyn Any) -> Vec<(u64, Action)> {
    let Ok(action) = action_of::<P>(action) else {
        return Vec::new();
    };
    P::spread(action, index, parts_of::<P>(parts))
        .into_iter()
        .map(|(part, further)| (part, Box::new(further) as Action))
        .collect()
}

fn hidden<P: Chained>(parts: &dyn Any) -> HashSet<String> {
    P::hidden(parts_of::<P>(parts))
}

fn content<P: Chained>(
    state: &dyn Any,
    doc: &AutoCommit,
    item: Option<&str>,
) -> Result<serde_json::Value, String> {
    let state = state.downcast_ref().ok_or_else(mismatch::<P>)?;
    // The window's reader leaves what no view demanded unread
    // (`#[model(demanded)]`): a read answers what the part holds.
    if P::RESOURCE_TABLE.is_some_and(tilo_resource::table::reads_on_demand) {
        let whole = P::read(doc).map_err(|error| error.to_string())?;
        return tilo_resource::read::content::<P>(&whole, doc, item);
    }
    tilo_resource::read::content::<P>(state, doc, item)
}

fn content_at<P: Chained>(
    doc: &AutoCommit,
    item: Option<&str>,
) -> Result<serde_json::Value, String> {
    let past = P::read(doc).map_err(|error| error.to_string())?;
    tilo_resource::read::content::<P>(&past, doc, item)
}

fn written<P: Chained>(
    action: &dyn Any,
) -> Result<(&dyn tilo_actions::Action, EncodedAction), ActionError> {
    let action = action_of::<P>(action).map_err(ActionError::Refused)?;
    Ok((action, tilo_resource::encode::<P>(action)?))
}

fn group<P: Chained>(
    actions: Vec<Box<dyn Any>>,
) -> Result<(Box<dyn tilo_actions::Action>, EncodedAction), ActionError> {
    let actions = actions
        .into_iter()
        .map(|action| action.downcast::<P::Action>().map(|action| *action))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ActionError::Refused(mismatch::<P>()))?;
    let group = tilo_resource::group::ActionGroup { actions };
    let encoded = EncodedAction::encode(tilo_resource::group::group_type::<P>(), &group)?;
    Ok((Box::new(group), encoded))
}

fn carry<P: Chained>(previous: u64, part: &dyn Any) -> Option<Result<Box<dyn Any>, String>> {
    let carried = P::carry(previous, part.downcast_ref()?)?;
    Some(
        carried
            .build(tilo_resource::origin_now_ms())
            .map(|action| Box::new(action) as Box<dyn Any>),
    )
}

fn decode<P: Chained>(action: &EncodedAction) -> Result<Decoded, String> {
    if action.type_id == tilo_resource::group::group_type::<P>() {
        // As every group's payload is read: one written with what its
        // members share once reads as the list of them.
        let group: tilo_resource::group::ActionGroup<P::Action> =
            tilo_actions::decode_payload(&action.type_id, &action.payload)
                .map_err(|error| error.to_string())?;
        let mut members = Vec::with_capacity(group.actions.len());
        for member in group.actions {
            super::super::resources::not_by_app::<P>(&member)?;
            members.push(Box::new(member) as Box<dyn Any>);
        }
        return Ok(Decoded::Group(members));
    }
    if action.type_id != P::ACTION_TYPE {
        return Err(format!("{} take no `{}` action", P::LABEL, action.type_id));
    }
    let action: P::Action =
        tilo_actions::decode_json(&action.payload).map_err(|error| error.to_string())?;
    super::super::resources::not_by_app::<P>(&action)?;
    Ok(Decoded::One(Box::new(action)))
}

fn from_json<P: Chained>(action: serde_json::Value) -> Result<Box<dyn Any>, serde_json::Error> {
    let action: P::Action = serde::Deserialize::deserialize(action)?;
    Ok(Box::new(action))
}

fn to_json<P: Chained>(action: &dyn Any) -> Result<Vec<u8>, ActionError> {
    let action = action_of::<P>(action).map_err(ActionError::Refused)?;
    serde_json::to_vec(action).map_err(ActionError::Encode)
}

fn clone<P: Chained>(action: &dyn Any) -> Box<dyn Any> {
    match action.downcast_ref::<P::Action>() {
        Some(action) => Box::new(action.clone()),
        None => Box::new(()),
    }
}

fn history<P: Chained>(edit: &PartEdit) -> Box<dyn tilo_actions::Action> {
    Box::new(super::super::history::part_action::<P>(edit))
}

fn publish<P: Chained>(id: &str, state: &dyn Any) -> tilo_actions::ResourceSpec {
    match state.downcast_ref::<P::State>() {
        Some(state) => tilo_resource::publish::<P>(id, P::LABEL, state),
        None => tilo_actions::ResourceSpec::new(id, P::KIND, P::LABEL),
    }
}

fn publish_head<P: Chained>(id: &str, state: &dyn Any) -> tilo_actions::ResourceSpec {
    match state.downcast_ref::<P::State>() {
        Some(state) => tilo_resource::publish_head::<P>(id, P::LABEL, state),
        None => tilo_actions::ResourceSpec::new(id, P::KIND, P::LABEL),
    }
}

fn children<P: Chained>(state: &dyn Any) -> tilo_resource::items::ChildrenParts {
    match state.downcast_ref::<P::State>() {
        Some(state) => P::children_parts(state),
        None => tilo_resource::items::ChildrenParts {
            others: Vec::new(),
            kept: Rc::default(),
            items: Vec::new(),
            item_actions: Vec::new(),
        },
    }
}

fn places<P: Chained>(base: &dyn Any, parts: &dyn Any) -> Vec<tilo_actions::ResourceSpec> {
    let Ok(base) = base_of::<P>(base) else {
        return Vec::new();
    };
    tilo_resource::derived::published::<P>(P::places(base, parts_of::<P>(parts))).to_resources()
}

fn tree<P: Chained>(parts: &dyn Any) -> Vec<(tilo_resource::Id, Option<tilo_resource::Id>)> {
    P::tree(parts_of::<P>(parts))
}

fn search<P: Chained>(state: &dyn Any) -> Vec<tilo_resource::SearchEntry> {
    state.downcast_ref().map(P::search).unwrap_or_default()
}

fn search_all<P: Chained>(
    reader: &mut Option<Box<dyn Any>>,
    doc: &AutoCommit,
) -> Vec<tilo_resource::SearchEntry> {
    let held = reader
        .as_mut()
        .and_then(|reader| reader.downcast_mut::<tilo_resource::Reader<P>>());
    let synced = match held {
        Some(held) => held.sync(doc).map(drop),
        None => tilo_resource::Reader::<P>::read_all(doc)
            .map(|read| *reader = Some(Box::new(read) as Box<dyn Any>)),
    };
    if let Err(error) = synced {
        tracing::warn!(kind = P::KIND, %error, "chain part unreadable for search");
        *reader = None;
        return Vec::new();
    }
    reader
        .as_ref()
        .and_then(|reader| reader.downcast_ref::<tilo_resource::Reader<P>>())
        .map(|reader| P::search(reader.state()))
        .unwrap_or_default()
}

fn search_over<P: Chained>(base: &dyn Any, parts: &dyn Any) -> Vec<tilo_resource::SearchEntry> {
    let Ok(base) = base_of::<P>(base) else {
        return Vec::new();
    };
    P::search_over(base, parts_of::<P>(parts))
}

fn read_over<P: Chained>(base: &dyn Any, parts: &dyn Any, item: &str) -> ReadOver {
    match base_of::<P>(base) {
        Ok(base) => P::read_over(base, parts_of::<P>(parts), item),
        Err(error) => Some(Err(error)),
    }
}

fn prepare_read<P: Chained>(
    filter: &mut Filter,
    base: &dyn Any,
    parts: &dyn Any,
) -> Result<(), String> {
    P::prepare_read(filter, base_of::<P>(base)?, parts_of::<P>(parts))
}

/// The members of a group by the part each changes (`indices`, in the
/// group's order), parts in order: one function for every part type.
#[inline(never)]
pub(super) fn by_part(indices: &[u64]) -> Vec<(u64, Vec<usize>)> {
    let mut per_part: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (at, index) in indices.iter().enumerate() {
        per_part.entry(*index).or_default().push(at);
    }
    per_part.into_iter().collect()
}

/// A history verb on a part, from its payload: decoded by one function for
/// every part type.
#[inline(never)]
pub(super) fn part_edit(payload: &[u8]) -> Result<PartEdit, String> {
    ui_core::history_source::verbs::part_from_payload(payload)
}
